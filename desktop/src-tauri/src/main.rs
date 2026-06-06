//! GTO Lab desktop shell (P1): the existing Vite viz in a Tauri webview, window opening
//! MAXIMISED (titlebar present, taskbar visible - a study tool used beside other windows,
//! deliberately NOT fullscreen). P1 runs against a manually started gto-server; the
//! sidecar lifecycle (spawn on launch, tree-kill on exit, stale-instance handling,
//! matrix-path fail-loudly) lands in P2.
//!
//! The webview origin on Windows is http://tauri.localhost (useHttpsScheme defaults to
//! false in v2), so the viz's fetch/SSE to http://127.0.0.1:7474 is same-scheme: no
//! mixed-content block, and the server's permissive CORS admits the origin.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use tauri::Manager;

fn main() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .setup(|app| {
            // belt-and-braces against tauri#11554 (maximized:true reported flaky on some
            // setups): assert the maximised state from code as well as config
            if let Some(window) = app.get_webview_window("main") {
                let _ = window.maximize();
            }
            Ok(())
        })
        .run(tauri::generate_context!())
        .expect("error while running GTO Lab");
}
