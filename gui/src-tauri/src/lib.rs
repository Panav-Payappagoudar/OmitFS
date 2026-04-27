// OmitFS Desktop — Tauri v2 main entry point
//
// Architecture:
//   1. On startup → spawn `omitfs serve --port 3031` as a sidecar subprocess
//   2. Register Ctrl+Space global hotkey to toggle window visibility
//   3. System tray icon with Show / Quit menu items
//   4. WebView loads http://localhost:3031 (the embedded OmitFS web UI)
//
// To run: `npm run dev` inside gui/
// To build: `npm run build` inside gui/

use tauri::{
    menu::{Menu, MenuItem},
    tray::{MouseButton, TrayIconBuilder, TrayIconEvent},
    AppHandle, Manager, WebviewWindowBuilder,
};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut};
use tauri_plugin_shell::ShellExt;

const OMITFS_PORT: u16 = 3031;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_shell::init())
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .setup(|app| {
            // ── 1. Spawn omitfs serve ───────────────────────────────────────
            let _ = app.shell()
                .command("omitfs")
                .args(["serve", "--port", &OMITFS_PORT.to_string()])
                .spawn();

            // Give the server a moment to bind before the WebView connects
            std::thread::sleep(std::time::Duration::from_millis(800));

            // ── 2. Create the main (hidden) window ──────────────────────────
            let win = WebviewWindowBuilder::new(
                app,
                "main",
                tauri::WebviewUrl::External(
                    format!("http://localhost:{OMITFS_PORT}").parse().unwrap(),
                ),
            )
            .title("OmitFS")
            .inner_size(780.0, 560.0)
            .min_inner_size(600.0, 400.0)
            .decorations(false)
            .transparent(true)
            .visible(false)
            .center()
            .always_on_top(true)
            .skip_taskbar(true)
            .build()?;

            // ── 3. Global hotkey Ctrl+Space ─────────────────────────────────
            let win_clone = win.clone();
            app.global_shortcut().on_shortcut(
                Shortcut::new(Some(Modifiers::CONTROL), Code::Space),
                move |_app, _shortcut, _event| {
                    if win_clone.is_visible().unwrap_or(false) {
                        let _ = win_clone.hide();
                    } else {
                        let _ = win_clone.show();
                        let _ = win_clone.set_focus();
                    }
                },
            )?;

            // ── 4. System tray ──────────────────────────────────────────────
            let show_item = MenuItem::with_id(app, "show",  "Show OmitFS",  true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit",  "Quit",         true, None::<&str>)?;
            let menu      = Menu::with_items(app, &[&show_item, &quit_item])?;

            let _tray = TrayIconBuilder::new()
                .menu(&menu)
                .on_menu_event(|app: &AppHandle, event| match event.id.as_ref() {
                    "show" => {
                        if let Some(w) = app.get_webview_window("main") {
                            let _ = w.show();
                            let _ = w.set_focus();
                        }
                    }
                    "quit" => std::process::exit(0),
                    _ => {}
                })
                .on_tray_icon_event(|tray, event| {
                    // Left-click on tray icon toggles the window
                    if let TrayIconEvent::Click { button: MouseButton::Left, .. } = event {
                        let app = tray.app_handle();
                        if let Some(w) = app.get_webview_window("main") {
                            if w.is_visible().unwrap_or(false) {
                                let _ = w.hide();
                            } else {
                                let _ = w.show();
                                let _ = w.set_focus();
                            }
                        }
                    }
                })
                .build(app)?;

            Ok(())
        })
        .on_window_event(|win, event| {
            // Hide instead of close so the tray icon keeps working
            if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = win.hide();
            }
        })
        .run(tauri::generate_context!())
        .expect("OmitFS GUI crashed");
}
