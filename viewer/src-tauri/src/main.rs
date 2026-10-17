//! Private, single-session RFB viewer.
//!
//! The webview is deliberately a fixed-function canvas. Remote Chromium DOM
//! and profile data never cross this process boundary: only RFB bytes do.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod mounts;
mod native_audio;
mod native_video;
mod session;
mod settings;
mod zipfs;

use std::sync::Arc;

use tauri::{RunEvent, WebviewUrl, WebviewWindowBuilder};

use mounts::Invocation;
use session::{Connection, SessionManager, Status};

fn main() {
  let options = match mounts::parse_args(std::env::args_os().skip(1)) {
    Ok(Invocation::Run(options)) => options,
    Ok(Invocation::Help) => {
      print!("{}", mounts::USAGE);
      return;
    }
    Err(error) => {
      eprintln!("docker-vm-viewer: {error}\n\n{}", mounts::USAGE);
      std::process::exit(2);
    }
  };
  let archives = session::prepare_mounts(&options).unwrap_or_else(|error| {
    eprintln!("archive mount refused: {error}");
    std::process::exit(1);
  });
  // Returns at once: Docker work runs in the background while the window
  // shows its progress.
  let manager = SessionManager::start(archives, settings::load()).unwrap_or_else(|error| {
    eprintln!("private session refused: {error}");
    std::process::exit(1);
  });
  let managed = Arc::clone(&manager);
  stop_on_signal(Arc::clone(&manager));
  let video = Arc::new(native_video::NativeVideo::default());
  let video_at_exit = Arc::clone(&video);
  let audio = Arc::new(native_audio::NativeAudio::default());
  let audio_at_exit = Arc::clone(&audio);

  tauri::Builder::default()
    .runtime(tauri_runtime_wry::Wry::default())
    .manage(manager)
    .manage(Arc::clone(&video))
    .manage(audio)
    .invoke_handler(tauri::generate_handler![status, connection, set_display_scale, set_display_size, stats_enabled, report_stats, rfb_tuning, video_event,
      native_video_start, native_video_state, native_video_place, native_audio_start,
      native_video_hidden, native_controls, settings_view, save_settings, restart_session])
    .setup(move |app| {
      let window = WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("Private desktop canvas")
        .inner_size(1280., 800.)
        .min_inner_size(800., 500.)
        // The local viewer is an off-the-record, fixed-function RFB canvas.
        .incognito(true)
        .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny)
        .on_permission_request(|_, _| tauri::webview::PermissionResponse::Deny)
        .build()?;
      // GTK main thread: put the (hidden) native video layer over the webview.
      native_video::install(&window, &video);
      Ok(())
    })
    .build(tauri::generate_context!())
    .expect("failed to build private WebKitGTK viewer")
    .run(move |_, event| {
      if matches!(event, RunEvent::Exit) {
        video_at_exit.stop();
        audio_at_exit.stop();
        managed.stop();
      }
    });
}

/// SIGTERM/SIGINT/SIGHUP (a `kill`, Ctrl-C, or logout) must still remove the
/// container. The handler only writes to a pipe; a thread does the teardown.
fn stop_on_signal(manager: Arc<SessionManager>) {
  static WRITE_FD: std::sync::atomic::AtomicI32 = std::sync::atomic::AtomicI32::new(-1);
  extern "C" fn handle(signal: libc::c_int) {
    let byte = signal as u8;
    unsafe { libc::write(WRITE_FD.load(std::sync::atomic::Ordering::Relaxed), (&raw const byte).cast(), 1) };
  }
  let mut fds = [0; 2];
  if unsafe { libc::pipe2(fds.as_mut_ptr(), libc::O_CLOEXEC) } != 0 { return; }
  WRITE_FD.store(fds[1], std::sync::atomic::Ordering::Relaxed);
  std::thread::spawn(move || {
    let mut byte = 0u8;
    if unsafe { libc::read(fds[0], (&raw mut byte).cast(), 1) } == 1 {
      manager.stop();
      std::process::exit(128 + i32::from(byte));
    }
  });
  for signal in [libc::SIGTERM, libc::SIGINT, libc::SIGHUP] {
    unsafe { libc::signal(signal, handle as *const () as libc::sighandler_t) };
  }
}

/// Opt-in performance counters (`DOCKER_VM_STATS=1`): the UI counts painted
/// frames and decode time; the bridge counts RFB bytes. Printed to stderr only.
fn stats_on() -> bool { std::env::var_os("DOCKER_VM_STATS").is_some_and(|value| value == "1") }

/// RFB encoding levels requested by the viewer. Each is 0-9; unset or invalid
/// keeps the built-in default. `DOCKER_VM_RFB_QUALITY` is the Tight JPEG
/// quality, `DOCKER_VM_RFB_COMPRESSION` the zlib level.
#[derive(serde::Serialize)]
struct RfbTuning { quality: Option<u8>, compression: Option<u8> }

#[tauri::command]
fn rfb_tuning() -> RfbTuning {
  let level = |name: &str| std::env::var(name).ok()?.parse::<u8>().ok().filter(|level| *level <= 9);
  RfbTuning { quality: level("DOCKER_VM_RFB_QUALITY"), compression: level("DOCKER_VM_RFB_COMPRESSION") }
}

/// Hardware-video transport state from the UI (started, or why it fell back
/// to RFB frames), for the session log.
#[tauri::command]
fn video_event(message: String) {
  eprintln!("video: {}", message.chars().filter(|c| !c.is_control()).take(200).collect::<String>());
}

#[tauri::command]
fn stats_enabled() -> bool { stats_on() }

#[tauri::command]
fn report_stats(manager: tauri::State<'_, Arc<SessionManager>>, frames: u32, decode_ms: f64, seconds: f64) {
  if !stats_on() || !(seconds.is_finite() && seconds > 0.0) { return; }
  static LAST_BYTES: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
  let total = manager.bridged_bytes();
  let bytes = total.saturating_sub(LAST_BYTES.swap(total, std::sync::atomic::Ordering::Relaxed));
  eprintln!(
    "stats: {:.1} content fps, decode {:.0} ms/s, {:.1} Mbit/s RFB",
    f64::from(frames) / seconds, decode_ms / seconds, bytes as f64 * 8.0 / 1e6 / seconds,
  );
}

#[tauri::command]
fn status(manager: tauri::State<'_, Arc<SessionManager>>) -> Status {
  manager.status()
}

#[tauri::command]
fn connection(manager: tauri::State<'_, Arc<SessionManager>>) -> Result<Connection, String> {
  manager.connection().ok_or_else(|| "session is no longer available".to_string())
}

/// Starts native presentation of the container's NVENC stream; false when it
/// is not offered or this host cannot present natively (the UI then uses its
/// WebCodecs path).
#[tauri::command]
fn native_video_start(manager: tauri::State<'_, Arc<SessionManager>>, video: tauri::State<'_, Arc<native_video::NativeVideo>>) -> bool {
  video.start(manager.video_address())
}

/// Hides the native video layer while the settings panel is open over it.
#[tauri::command]
fn native_video_hidden(video: tauri::State<'_, Arc<native_video::NativeVideo>>, hidden: bool) {
  video.set_hidden(hidden);
}

#[tauri::command]
fn native_controls(video: tauri::State<'_, Arc<native_video::NativeVideo>>) -> bool {
  video.has_controls()
}

#[tauri::command]
fn settings_view(manager: tauri::State<'_, Arc<SessionManager>>) -> settings::SettingsView {
  manager.settings_view()
}

/// Saves the choices; they apply when the next session starts.
#[tauri::command]
fn save_settings(gpu: settings::Mode, wayland: settings::Mode, video: settings::Mode) -> Result<(), String> {
  settings::save(settings::Settings { gpu, wayland, video })
}

/// Ends this session (container removed) and relaunches the viewer, which
/// starts a fresh session with the saved settings.
#[tauri::command]
fn restart_session(app: tauri::AppHandle, manager: tauri::State<'_, Arc<SessionManager>>) {
  manager.stop();
  app.restart();
}

/// Starts playing the desktop's audio, if the container offers it.
#[tauri::command]
fn native_audio_start(manager: tauri::State<'_, Arc<SessionManager>>, audio: tauri::State<'_, Arc<native_audio::NativeAudio>>) {
  audio.start(manager.audio_address());
}

#[tauri::command]
fn native_video_state(video: tauri::State<'_, Arc<native_video::NativeVideo>>) -> native_video::VideoState {
  video.state()
}

/// Moves the native video layer onto noVNC's canvas (CSS px). Main thread.
#[tauri::command]
fn native_video_place(video: tauri::State<'_, Arc<native_video::NativeVideo>>, x: i32, y: i32, width: i32, height: i32) {
  video.place(x, y, width, height);
}

#[tauri::command]
fn set_display_size(manager: tauri::State<'_, Arc<SessionManager>>, width: u32, height: u32) -> Result<(), String> {
  manager.set_display_size(width, height)
}

#[tauri::command]
fn set_display_scale(manager: tauri::State<'_, Arc<SessionManager>>, scale: f64) -> Result<(), String> {
  manager.set_display_scale(scale)
}
