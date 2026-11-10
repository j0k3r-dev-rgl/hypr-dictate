use crate::ui::UiEvent;
use eframe::egui;
use std::sync::{Arc, RwLock, mpsc::Sender};
use std::thread;
use std::time::Duration;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

pub const PRESET_SHORTCUTS: [&str; 8] = [
    "",
    "F9",
    "F10",
    "F8",
    "CTRL + SHIFT + D",
    "CTRL + ALT + D",
    "SUPER + SHIFT + D",
    "ALT + D",
];

pub fn is_token_down(token: &str) -> bool {
    let t = token.trim().to_uppercase();
    let vk: i32 = match t.as_str() {
        "CTRL" | "CONTROL" => 0x11, // VK_CONTROL
        "SHIFT" => 0x10,            // VK_SHIFT
        "ALT" => 0x12,              // VK_MENU
        "SUPER" | "WIN" => {
            let left = (unsafe { GetAsyncKeyState(0x5B) } as u16 & 0x8000) != 0;
            let right = (unsafe { GetAsyncKeyState(0x5C) } as u16 & 0x8000) != 0;
            return left || right;
        }
        "F1" => 0x70,
        "F2" => 0x71,
        "F3" => 0x72,
        "F4" => 0x73,
        "F5" => 0x74,
        "F6" => 0x75,
        "F7" => 0x76,
        "F8" => 0x77,
        "F9" => 0x78,
        "F10" => 0x79,
        "F11" => 0x7A,
        "F12" => 0x7B,
        "SPACE" => 0x20,
        s if s.len() == 1 => s.chars().next().unwrap().to_ascii_uppercase() as i32,
        _ => return false,
    };
    (unsafe { GetAsyncKeyState(vk) } as u16 & 0x8000) != 0
}

pub fn shortcut_held(shortcut: &str) -> bool {
    let trimmed = shortcut.trim();
    if trimmed.is_empty() {
        return false;
    }
    trimmed.split('+').all(is_token_down)
}

pub fn spawn_listener(
    tx: Sender<UiEvent>,
    ctx: egui::Context,
    shared_shortcut: Arc<RwLock<(String, bool)>>,
) {
    thread::spawn(move || {
        let mut was_held = false;
        loop {
            thread::sleep(Duration::from_millis(10));
            let (shortcut, _) = {
                let guard = shared_shortcut.read().unwrap();
                guard.clone()
            };
            if shortcut.trim().is_empty() {
                was_held = false;
                continue;
            }

            let held = shortcut_held(&shortcut);
            if held && !was_held {
                let _ = tx.send(UiEvent::ShortcutDown);
                ctx.request_repaint();
            } else if !held && was_held {
                let _ = tx.send(UiEvent::ShortcutUp);
                ctx.request_repaint();
            }
            was_held = held;
        }
    });
}
