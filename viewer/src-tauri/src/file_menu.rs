//! Native File menu. Paths enter only through GTK's host picker; the webview
//! has no command that accepts a source path.

use std::{cell::RefCell, path::PathBuf, rc::Rc, sync::Arc, time::Duration};

use gtk::prelude::*;
use tauri::{Manager, menu::{Menu, MenuItem, Submenu}};

use crate::{session::{SessionManager, SharingStatus}, settings::SharingTier};

const MOUNT_FOLDER: &str = "mount-folder";
const MOUNT_ARCHIVE: &str = "mount-archive";
const QUIT: &str = "quit";
const UNMOUNT_PREFIX: &str = "unmount:";

pub fn install(app: tauri::AppHandle, window: tauri::WebviewWindow, manager: Arc<SessionManager>) -> tauri::Result<()> {
  let status = manager.sharing_status();
  build(&app, &window, &status)?;

  let last = Rc::new(RefCell::new(status));
  let refresh_window = window.clone();
  let refresh_app = app.clone();
  let refresh_manager = Arc::clone(&manager);
  gtk::glib::timeout_add_local(Duration::from_millis(350), move || {
    let status = refresh_manager.sharing_status();
    if *last.borrow() != status {
      if let Err(error) = build(&refresh_app, &refresh_window, &status) { eprintln!("refresh File menu: {error}"); }
      *last.borrow_mut() = status;
    }
    gtk::glib::ControlFlow::Continue
  });

  window.on_menu_event(move |_, event| {
    let id = event.id().0.as_str();
    if id == QUIT { app.exit(0); return; }
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
    if id != MOUNT_FOLDER && id != MOUNT_ARCHIVE { return; }
    let Some(window) = app.get_webview_window("main") else { return; };
    let folder = id == MOUNT_FOLDER;
    let status = manager.sharing_status();
    if status.unavailable.is_some() || status.allowed_now == SharingTier::Off { return; }
    if folder && status.allowed_now == SharingTier::Encrypted { return; }
    let Some(source) = pick(&window, folder) else { return; };
    let writable = folder && status.allowed_now == SharingTier::Rw && allow_changes(&window);
    let manager = Arc::clone(&manager);
    let app = app.clone();
    std::thread::spawn(move || {
      let result = if folder { manager.mount_folder_live(source, writable) } else { manager.mount_archive_live(source) }
        .map(|name| format!("Mounted {name} at ~/mnt/{name}"));
      report(&app, result);
    });
  });
  Ok(())
}

fn build(app: &tauri::AppHandle, window: &tauri::WebviewWindow, status: &SharingStatus) -> tauri::Result<()> {
  let menu = Menu::new(app)?;
  let file = Submenu::new(app, "&File", true)?;
  let enabled = status.unavailable.is_none() && status.allowed_now != SharingTier::Off;
  if !enabled {
    let reason = status.unavailable.as_deref().unwrap_or("File sharing is off in Settings");
    file.append(&MenuItem::with_id(app, "sharing-unavailable", reason, false, None::<&str>)?)?;
  } else {
    if status.allowed_now != SharingTier::Encrypted {
      file.append(&MenuItem::with_id(app, MOUNT_FOLDER, "Mount Folder…", true, None::<&str>)?)?;
    }
    let label = if status.allowed_now == SharingTier::Encrypted { "Mount Encrypted Archive…" } else { "Mount Archive…" };
    file.append(&MenuItem::with_id(app, MOUNT_ARCHIVE, label, true, None::<&str>)?)?;
  }
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

fn pick(window: &tauri::WebviewWindow, folder: bool) -> Option<PathBuf> {
  let parent = window.gtk_window().ok()?;
  let title = if folder { "Mount Folder" } else { "Mount ZIP Archive" };
  let action = if folder { gtk::FileChooserAction::SelectFolder } else { gtk::FileChooserAction::Open };
  let dialog = gtk::FileChooserDialog::new(Some(title), Some(&parent), action);
  dialog.add_buttons(&[("Cancel", gtk::ResponseType::Cancel), ("Mount", gtk::ResponseType::Accept)]);
  if !folder {
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("ZIP archives"));
    filter.add_pattern("*.zip");
    dialog.add_filter(filter);
  }
  let selected = (dialog.run() == gtk::ResponseType::Accept).then(|| dialog.filename()).flatten();
  dialog.hide();
  selected
}

fn allow_changes(window: &tauri::WebviewWindow) -> bool {
  let Ok(parent) = window.gtk_window() else { return false; };
  let dialog = gtk::MessageDialog::new(Some(&parent), gtk::DialogFlags::MODAL,
    gtk::MessageType::Question, gtk::ButtonsType::None,
    "Allow the desktop to change files in this folder?");
  dialog.add_buttons(&[("Read-only", gtk::ResponseType::No), ("Allow changes", gtk::ResponseType::Yes)]);
  dialog.set_default_response(gtk::ResponseType::No);
  let writable = dialog.run() == gtk::ResponseType::Yes;
  dialog.hide();
  writable
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
    dialog.run();
    dialog.hide();
  });
}
