//! Native File menu. Paths enter only through GTK's host picker; the webview
//! has no command that accepts a source path.

use std::{cell::{Cell, RefCell}, path::PathBuf, rc::Rc, sync::{Arc, atomic::{AtomicBool, Ordering}}, time::Duration};

use gtk::prelude::*;
use tauri::{Manager, menu::{Menu, MenuItem, Submenu}};

use crate::{session::{SessionManager, SharingStatus}, settings::SharingTier, shares::{self, Kind, SavedShare}};

const MOUNT_FOLDER: &str = "mount-folder";
const MOUNT_ARCHIVE: &str = "mount-archive";
const QUIT: &str = "quit";
const UNMOUNT_PREFIX: &str = "unmount:";
const RECENT_PREFIX: &str = "recent:";
const FORGET_ALL: &str = "forget-all";

pub fn install(app: tauri::AppHandle, window: tauri::WebviewWindow, manager: Arc<SessionManager>) -> tauri::Result<()> {
  let status = manager.sharing_status();
  let saved = shares::load();
  build(&app, &window, &status, &saved)?;

  let last = Rc::new(RefCell::new((status, saved)));
  let refresh_window = window.clone();
  let refresh_app = app.clone();
  let refresh_manager = Arc::clone(&manager);
  gtk::glib::timeout_add_local(Duration::from_millis(350), move || {
    let status = refresh_manager.sharing_status();
    let saved = shares::load();
    if *last.borrow() != (status.clone(), saved.clone()) {
      if let Err(error) = build(&refresh_app, &refresh_window, &status, &saved) { eprintln!("refresh File menu: {error}"); }
      *last.borrow_mut() = (status, saved);
    }
    gtk::glib::ControlFlow::Continue
  });

  let picking = Arc::new(AtomicBool::new(false));
  window.on_menu_event(move |_, event| {
    let id = event.id().0.as_str();
    if id == QUIT { app.exit(0); return; }
    if id == FORGET_ALL {
      report(&app, shares::forget_all().map(|()| "Forgot all saved shares".into()));
      return;
    }
    if let Some(name) = id.strip_prefix(UNMOUNT_PREFIX) {
      let name = name.to_string();
      let manager = Arc::clone(&manager);
      let app = app.clone();
      std::thread::spawn(move || {
        let result = manager.unmount_live(&name).map(|()| format!("Unmounted {name}"));
        report(&app, result);
      });
      return;
    }
    if let Some(index) = id.strip_prefix(RECENT_PREFIX).and_then(|index| index.parse::<usize>().ok()) {
      let Some(share) = shares::load().get(index).cloned() else { return; };
      let status = manager.sharing_status();
      if status.unavailable.is_some() || shares::reason(&share, status.allowed_now).is_some() { return; }
      let manager = Arc::clone(&manager);
      let app = app.clone();
      std::thread::spawn(move || {
        let result = mount_saved(&manager, share).map(|name| format!("Mounted {name} at ~/mnt/{name}"));
        report(&app, result);
      });
      return;
    }
    if id != MOUNT_FOLDER && id != MOUNT_ARCHIVE { return; }
    let folder = id == MOUNT_FOLDER;
    let status = manager.sharing_status();
    if status.unavailable.is_some() || status.allowed_now == SharingTier::Off { return; }
    if folder && status.allowed_now == SharingTier::Encrypted { return; }
    if picking.swap(true, Ordering::AcqRel) { return; }
    let manager = Arc::clone(&manager);
    let app = app.clone();
    let picking = Arc::clone(&picking);
    // Let GTK finish dismissing the native menu before presenting another
    // surface. Modal dialogs use response signals, never a nested event loop.
    gtk::glib::idle_add_once(move || {
      let Some(window) = app.get_webview_window("main") else {
        picking.store(false, Ordering::Release);
        return;
      };
      let confirm_window = window.clone();
      pick(&window, folder, move |selected| {
        let Some((source, remember)) = selected else {
          picking.store(false, Ordering::Release);
          return;
        };
        let mount = move |writable| {
          picking.store(false, Ordering::Release);
          mount_selected(app, manager, source, folder, remember, writable);
        };
        if folder && status.allowed_now == SharingTier::Rw {
          allow_changes(&confirm_window, mount);
        } else {
          mount(false);
        }
      });
    });
  });
  Ok(())
}

fn mount_selected(app: tauri::AppHandle, manager: Arc<SessionManager>, source: PathBuf,
                  folder: bool, remember: bool, writable: bool) {
  std::thread::spawn(move || {
    let saved = SavedShare { source: source.clone(), kind: if folder { Kind::Folder } else { Kind::Zip }, allow_changes: writable };
    let result = if folder { manager.mount_folder_live(source, writable) } else { manager.mount_archive_live(source) }
      .and_then(|name| {
        if remember { shares::remember(saved)?; }
        Ok(format!("Mounted {name} at ~/mnt/{name}"))
      });
    report(&app, result);
  });
}

fn mount_saved(manager: &SessionManager, share: SavedShare) -> Result<String, String> {
  match share.kind {
    Kind::Folder => manager.mount_folder_live(share.source, share.allow_changes),
    Kind::Zip => manager.mount_archive_live(share.source),
  }
}

fn build(app: &tauri::AppHandle, window: &tauri::WebviewWindow, status: &SharingStatus, saved: &[SavedShare]) -> tauri::Result<()> {
  let menu = Menu::new(app)?;
  let file = Submenu::new(app, "&File", true)?;
  let mount = Submenu::new(app, "Mount", true)?;
  let enabled = status.unavailable.is_none() && status.allowed_now != SharingTier::Off;
  if !enabled {
    let reason = status.unavailable.as_deref().unwrap_or("File sharing is off in Settings");
    mount.append(&MenuItem::with_id(app, "sharing-unavailable", reason, false, None::<&str>)?)?;
  } else {
    if status.allowed_now != SharingTier::Encrypted {
      mount.append(&MenuItem::with_id(app, MOUNT_FOLDER, "Folder…", true, None::<&str>)?)?;
    }
    let label = if status.allowed_now == SharingTier::Encrypted { "Encrypted Archive…" } else { "Archive…" };
    mount.append(&MenuItem::with_id(app, MOUNT_ARCHIVE, label, true, None::<&str>)?)?;
  }
  let recent = Submenu::new(app, "Recent", true)?;
  if saved.is_empty() {
    recent.append(&MenuItem::with_id(app, "no-saved-shares", "No saved shares", false, None::<&str>)?)?;
  } else {
    for (index, share) in saved.iter().enumerate() {
      let reason = status.unavailable.clone().or_else(|| shares::reason(share, status.allowed_now));
      let label = if let Some(reason) = &reason {
        format!("{} ({reason})", share.source.display())
      } else { share.source.display().to_string() };
      recent.append(&MenuItem::with_id(app, format!("{RECENT_PREFIX}{index}"), label.replace('&', "&&"), reason.is_none(), None::<&str>)?)?;
    }
  }
  mount.append(&recent)?;
  mount.append(&MenuItem::with_id(app, FORGET_ALL, "Forget all", !saved.is_empty(), None::<&str>)?)?;
  file.append(&mount)?;
  let unmount = Submenu::new(app, "Unmount", true)?;
  if status.mounts.is_empty() {
    unmount.append(&MenuItem::with_id(app, "no-shares", "No folders shared", false, None::<&str>)?)?;
  } else {
    for name in &status.mounts {
      unmount.append(&MenuItem::with_id(app, format!("{UNMOUNT_PREFIX}{name}"), name.replace('&', "&&"), true, None::<&str>)?)?;
    }
  }
  file.append(&unmount)?;
  file.append(&MenuItem::with_id(app, QUIT, "Quit", true, None::<&str>)?)?;
  menu.append(&file)?;
  window.set_menu(menu)?;
  Ok(())
}

fn pick(window: &tauri::WebviewWindow, folder: bool, done: impl FnOnce(Option<(PathBuf, bool)>) + 'static) {
  let Ok(parent) = window.gtk_window() else { done(None); return; };
  let title = if folder { "Mount Folder" } else { "Mount ZIP Archive" };
  let action = if folder { gtk::FileChooserAction::SelectFolder } else { gtk::FileChooserAction::Open };
  let dialog = gtk::FileChooserDialog::new(Some(title), Some(&parent), action);
  dialog.set_modal(true);
  dialog.set_destroy_with_parent(true);
  dialog.set_position(gtk::WindowPosition::CenterOnParent);
  // GTK restores chooser geometry independently of the viewer. Override it
  // after mapping so a saved size cannot hide the modal picker off-screen.
  let width = (parent.allocated_width() - 80).clamp(640, 900);
  let height = (parent.allocated_height() - 80).clamp(400, 600);
  let max_width = parent.allocated_width();
  let max_height = parent.allocated_height();
  dialog.set_default_size(width, height);
  dialog.connect_map(move |dialog| dialog.resize(width, height));
  dialog.connect_size_allocate(move |dialog, allocation| {
    // The chooser can restore its saved size from an idle callback after map.
    if allocation.width() > max_width || allocation.height() > max_height {
      dialog.resize(width, height);
    }
  });
  dialog.add_buttons(&[("Cancel", gtk::ResponseType::Cancel), ("Mount", gtk::ResponseType::Accept)]);
  let remember = gtk::CheckButton::with_label("Remember this share");
  dialog.set_extra_widget(&remember);
  if !folder {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("ZIP archives"));
    filter.add_pattern("*.zip");
    dialog.add_filter(filter);
  }
  let done = RefCell::new(Some(done));
  dialog.connect_response(move |dialog, response| {
    let Some(done) = done.borrow_mut().take() else { return; };
    let selected = (response == gtk::ResponseType::Accept)
      .then(|| dialog.filename().map(|source| (source, remember.is_active()))).flatten();
    dialog.hide();
    dialog.close();
    done(selected);
  });
  dialog.present();
}

fn allow_changes(window: &tauri::WebviewWindow, done: impl FnOnce(bool) + 'static) {
  let Ok(parent) = window.gtk_window() else { done(false); return; };
  let dialog = gtk::MessageDialog::new(Some(&parent), gtk::DialogFlags::MODAL,
    gtk::MessageType::Question, gtk::ButtonsType::None,
    "Allow the desktop to change files in this folder?");
  dialog.add_buttons(&[("Read-only", gtk::ResponseType::No), ("Allow changes", gtk::ResponseType::Yes)]);
  dialog.set_default_response(gtk::ResponseType::No);
  dialog.set_destroy_with_parent(true);
  let done = RefCell::new(Some(done));
  dialog.connect_response(move |dialog, response| {
    let Some(done) = done.borrow_mut().take() else { return; };
    dialog.hide();
    dialog.close();
    done(response == gtk::ResponseType::Yes);
  });
  dialog.present();
}

fn report(app: &tauri::AppHandle, result: Result<String, String>) {
  let app = app.clone();
  let _ = app.clone().run_on_main_thread(move || {
    let Some(window) = app.get_webview_window("main") else { return; };
    let Ok(parent) = window.gtk_window() else { return; };
    let (message, kind) = match result {
      Ok(message) => (message, gtk::MessageType::Info),
      Err(error) => (error, gtk::MessageType::Error),
    };
    let dialog = gtk::MessageDialog::new(Some(&parent), gtk::DialogFlags::MODAL, kind, gtk::ButtonsType::Ok, &message);
    dialog.set_destroy_with_parent(true);
    let closed = Cell::new(false);
    dialog.connect_response(move |dialog, _| {
      if closed.replace(true) { return; }
      dialog.hide();
      dialog.close();
    });
    dialog.present();
  });
}
