use crate::ui::UiEvent;
use eframe::egui;
use hypr_dictate::DictationMode;
use std::sync::{Arc, RwLock, mpsc::Sender};
use std::thread;
use std::time::Duration;
use windows_sys::Win32::UI::Input::KeyboardAndMouse::GetAsyncKeyState;

#[derive(Clone, Debug, PartialEq)]
pub struct ShortcutSettings {
    pub standard: String,
    pub formal: String,
    pub hold_to_talk: bool,
}

pub const PRESET_SHORTCUTS: [&str; 12] = [
    "",
    "F9",
    "F10",
    "F8",
    "F11",
    "F12",
    "CTRL + SHIFT + D",
    "CTRL + SHIFT + F",
    "CTRL + ALT + D",
    "CTRL + ALT + F",
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
        "ESC" | "ESCAPE" => 0x1B,
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
    shared_shortcuts: Arc<RwLock<ShortcutSettings>>,
) {
    thread::spawn(move || {
        let mut was_held_std = false;
        let mut was_held_formal = false;
        let mut was_held_esc = false;

        loop {
            thread::sleep(Duration::from_millis(10));
            let cfg = {
                let guard = shared_shortcuts.read().unwrap();
                guard.clone()
            };

            // 1. ESC key detection to cancel active operations
            let esc_held = is_token_down("ESC");
            if esc_held && !was_held_esc {
                let _ = tx.send(UiEvent::Cancel);
                ctx.request_repaint();
            }
            was_held_esc = esc_held;

            // 2. Standard transcription shortcut
            if !cfg.standard.trim().is_empty() {
                let held = shortcut_held(&cfg.standard);
                if held && !was_held_std {
                    let _ = tx.send(UiEvent::ShortcutDown(DictationMode::Transcription));
                    ctx.request_repaint();
                } else if !held && was_held_std {
                    let _ = tx.send(UiEvent::ShortcutUp(DictationMode::Transcription));
                    ctx.request_repaint();
                }
                was_held_std = held;
            } else {
                was_held_std = false;
            }

            // 3. Formal redaction shortcut
            if !cfg.formal.trim().is_empty()
                && cfg.formal.trim().to_uppercase() != cfg.standard.trim().to_uppercase()
            {
                let held = shortcut_held(&cfg.formal);
                if held && !was_held_formal {
                    let _ = tx.send(UiEvent::ShortcutDown(DictationMode::Formal));
                    ctx.request_repaint();
                } else if !held && was_held_formal {
                    let _ = tx.send(UiEvent::ShortcutUp(DictationMode::Formal));
                    ctx.request_repaint();
                }
                was_held_formal = held;
            } else {
                was_held_formal = false;
            }
        }
    });
}
