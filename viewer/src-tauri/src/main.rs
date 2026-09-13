//! Private, single-session RFB viewer.
//!
//! The webview is deliberately a fixed-function canvas. Remote Chromium DOM
//! and profile data never cross this process boundary: only RFB bytes do.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod session;

use std::sync::Arc;

use tauri::{RunEvent, WebviewUrl, WebviewWindowBuilder};
use tauri_runtime_cef::{Cef, SandboxPolicy, SecretStorage, cef::LogSeverity};

use session::{Connection, SessionManager};

#[tauri_runtime_cef::cef_entry_point]
fn main() {
  let manager = Arc::new(SessionManager::start().unwrap_or_else(|error| {
    eprintln!("private session refused: {error}");
    std::process::exit(1);
  }));
  let managed = Arc::clone(&manager);

  tauri::Builder::default()
    .runtime(cef_runtime(&manager))
    .manage(manager)
    .invoke_handler(tauri::generate_handler![connection])
    .setup(|app| {
      WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("Private desktop canvas")
        .inner_size(1280., 800.)
        .min_inner_size(800., 500.)
        // Empty request-context cache path: CEF off-the-record mode.
        .incognito(true)
        .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny)
        .on_permission_request(|_, _| tauri::webview::PermissionResponse::Deny)
        .build()?;
      Ok(())
    })
    .build(tauri::generate_context!())
    .expect("failed to build private CEF viewer")
    .run(move |_, event| {
      if matches!(event, RunEvent::Exit) {
        managed.stop();
      }
    });
}

fn cef_runtime(manager: &SessionManager) -> Cef {
  // The global root cache still exists in CEF even for an off-the-record
  // request context. It is created only under the verified tmpfs session dir.
  Cef::default()
    .sandbox(SandboxPolicy::Required)
    .secret_storage(SecretStorage::Mock)
    .root_cache_path(manager.cef_root())
    // DISABLE still permits FATAL stderr, hence the launcher must also keep
    // stderr out of persistent journald/log collectors.
    .log_severity(LogSeverity::DISABLE)
    .log_file("/dev/null")
    .allow_chromium_command_line_args(false)
    .profile_preference("safebrowsing.enabled", false)
    .profile_preference("credentials_enable_service", false)
    .profile_preference("profile.password_manager_leak_detection", false)
    .profile_preference("autofill.profile_enabled", false)
    .profile_preference("autofill.credit_card_enabled", false)
    .profile_preference("printing.enabled", false)
    .profile_preference("hardware.audio_capture_enabled", false)
    .profile_preference("hardware.video_capture_enabled", false)
    .global_preference("devtools.remote_debugging.allowed", false)
    .command_line_args([
      ("disable-gpu", None),
      ("use-gl", Some("swiftshader")),
      ("use-angle", Some("swiftshader")),
      ("disable-breakpad", None),
      ("disable-features", Some("AutofillServerCommunication,MediaRouter")),
      ("no-first-run", None),
      ("no-default-browser-check", None),
    ])
}

#[tauri::command]
fn connection(manager: tauri::State<'_, Arc<SessionManager>>) -> Result<Connection, String> {
  manager.connection().ok_or_else(|| "session is no longer available".to_string())
}
