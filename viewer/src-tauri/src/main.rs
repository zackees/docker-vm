//! Private, single-session RFB viewer.
//!
//! The webview is deliberately a fixed-function canvas. Remote Chromium DOM
//! and profile data never cross this process boundary: only RFB bytes do.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod session;

use std::sync::Arc;

use tauri::{RunEvent, WebviewUrl, WebviewWindowBuilder};

use session::{Connection, SessionManager};

fn main() {
  let manager = Arc::new(SessionManager::start().unwrap_or_else(|error| {
    eprintln!("private session refused: {error}");
    std::process::exit(1);
  }));
  let managed = Arc::clone(&manager);

  tauri::Builder::default()
    .runtime(tauri_runtime_wry::Wry::default())
    .manage(manager)
    .invoke_handler(tauri::generate_handler![connection, set_display_scale])
    .setup(|app| {
      WebviewWindowBuilder::new(app, "main", WebviewUrl::App("index.html".into()))
        .title("Private desktop canvas")
        .inner_size(1280., 800.)
        .min_inner_size(800., 500.)
        // The local viewer is an off-the-record, fixed-function RFB canvas.
        .incognito(true)
        .on_new_window(|_, _| tauri::webview::NewWindowResponse::Deny)
        .on_permission_request(|_, _| tauri::webview::PermissionResponse::Deny)
        .build()?;
      Ok(())
    })
    .build(tauri::generate_context!())
    .expect("failed to build private WebKitGTK viewer")
    .run(move |_, event| {
      if matches!(event, RunEvent::Exit) {
        managed.stop();
      }
    });
}

#[tauri::command]
fn connection(manager: tauri::State<'_, Arc<SessionManager>>) -> Result<Connection, String> {
  manager.connection().ok_or_else(|| "session is no longer available".to_string())
}

#[tauri::command]
fn set_display_scale(manager: tauri::State<'_, Arc<SessionManager>>, scale: f64) -> Result<(), String> {
  manager.set_display_scale(scale)
}
