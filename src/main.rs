#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use hypr_dictate::{api, audio, input, settings};

mod autostart;
mod ipc;
mod paint;
mod shortcut;
mod ui;

use anyhow::Result;
use std::sync::{Arc, RwLock};
use ui::{DictateApp, UiEvent};
use windows_sys::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows_sys::Win32::UI::WindowsAndMessaging::{GetSystemMetrics, SM_CXSCREEN, SM_CYSCREEN};

fn main() -> Result<()> {
    unsafe {
        AttachConsole(ATTACH_PARENT_PROCESS);
    }
    let command = std::env::args().nth(1).unwrap_or_else(|| "run".into());

    if command != "run" {
        if let Err(e) = ipc::send_command(&command) {
            eprintln!("Error enviando comando a hypr-dictate: {e}");
            std::process::exit(1);
        }
        return Ok(());
    }

    let settings = settings::Settings::load().unwrap_or_default();

    if settings.autostart {
        let _ = autostart::set_autostart(true);
    }

    let screen_w = unsafe { GetSystemMetrics(SM_CXSCREEN) };
    let screen_h = unsafe { GetSystemMetrics(SM_CYSCREEN) };
    let (init_x, init_y) = match settings.position.as_str() {
        "Arriba" => (((screen_w - 176) / 2).max(10), 38),
        "Izquierda" => (0, ((screen_h - 44) / 2).max(10)),
        "Derecha" => ((screen_w - 176).max(10), ((screen_h - 44) / 2).max(10)),
        _ => (((screen_w - 176) / 2).max(10), (screen_h - 44).max(0)),
    };

    let shared_shortcuts = Arc::new(RwLock::new(shortcut::ShortcutSettings {
        standard: settings.shortcut.clone(),
        formal: settings.shortcut_formal.clone(),
        hold_to_talk: settings.hold_to_talk,
    }));

    let viewport = eframe::egui::ViewportBuilder::default()
        .with_title("hypr-dictate")
        .with_decorations(false)
        .with_transparent(true)
        .with_always_on_top()
        .with_inner_size([176.0, 44.0])
        .with_position([init_x as f32, init_y as f32]);

    let options = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };

    let shared_sc_clone = shared_shortcuts.clone();

    eframe::run_native(
        "hypr-dictate",
        options,
        Box::new(move |cc| {
            let app = DictateApp::new(cc, settings, shared_shortcuts);

            // Start IPC listener
            let tx_ipc = app.tx.clone();
            let ctx_ipc = cc.egui_ctx.clone();
            let (ipc_cmd_tx, ipc_cmd_rx) = std::sync::mpsc::channel();
            if let Ok(()) = ipc::start_listener(ipc_cmd_tx) {
                std::thread::spawn(move || {
                    while let Ok(cmd) = ipc_cmd_rx.recv() {
                        let _ = tx_ipc.send(UiEvent::IpcCommand(cmd));
                        ctx_ipc.request_repaint();
                    }
                });
            }

            // Start global shortcut listener with UI repaint triggers
            let tx_sc = app.tx.clone();
            let ctx_sc = cc.egui_ctx.clone();
            shortcut::spawn_listener(tx_sc, ctx_sc, shared_sc_clone);

            Ok(Box::new(app))
        }),
    )
    .map_err(|e| anyhow::anyhow!("Error en eframe: {e}"))
}
