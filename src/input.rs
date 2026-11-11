use anyhow::{Result, bail, ensure};
use std::{mem::size_of, thread, time::Duration};
use windows_sys::Win32::{
    Foundation::HWND,
    UI::{
        Input::KeyboardAndMouse::{
            INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_KEYUP, KEYEVENTF_UNICODE,
            SendInput,
        },
        WindowsAndMessaging::{GetForegroundWindow, IsWindow, SetForegroundWindow},
    },
};

pub fn active_window() -> Result<String> {
    let hwnd = unsafe { GetForegroundWindow() };
    if hwnd.is_null() {
        bail!("No hay ninguna ventana enfocada");
    }
    Ok(format!("0x{:016x}", hwnd as usize))
}

pub fn restore_focus(address: &str) -> Result<()> {
    let raw = address.trim_start_matches("0x");
    let addr = usize::from_str_radix(raw, 16)
        .map_err(|_| anyhow::anyhow!("Dirección de ventana inválida"))?;
    let hwnd = addr as HWND;

    if hwnd.is_null() || unsafe { IsWindow(hwnd) } == 0 {
        bail!("La ventana original ya no existe; no se insertó texto");
    }

    let current = unsafe { GetForegroundWindow() };
    if current == hwnd {
        return Ok(());
    }

    unsafe {
        SetForegroundWindow(hwnd);
    }

    for _ in 0..20 {
        if unsafe { GetForegroundWindow() } == hwnd {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }

    // Try once more but don't bail if Windows denies focus change
    Ok(())
}

pub fn type_text(text: &str) -> Result<()> {
    if text.is_empty() {
        return Ok(());
    }

    let mut inputs: Vec<INPUT> = Vec::with_capacity(text.len() * 4);

    for ch in text.chars() {
        if ch == '\r' {
            continue;
        }
        if ch == '\n' {
            // Emulate Enter key
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: 0x0D, // VK_RETURN
                        wScan: 0,
                        dwFlags: 0,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: 0x0D, // VK_RETURN
                        wScan: 0,
                        dwFlags: KEYEVENTF_KEYUP,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
            continue;
        }

        let mut utf16_buf = [0u16; 2];
        let units = ch.encode_utf16(&mut utf16_buf);

        for &code in units.iter() {
            // Key down
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: 0,
                        wScan: code,
                        dwFlags: KEYEVENTF_UNICODE,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
            // Key up
            inputs.push(INPUT {
                r#type: INPUT_KEYBOARD,
                Anonymous: INPUT_0 {
                    ki: KEYBDINPUT {
                        wVk: 0,
                        wScan: code,
                        dwFlags: KEYEVENTF_UNICODE | KEYEVENTF_KEYUP,
                        time: 0,
                        dwExtraInfo: 0,
                    },
                },
            });
        }
    }

    let sent = unsafe {
        SendInput(
            inputs.len() as u32,
            inputs.as_ptr(),
            size_of::<INPUT>() as i32,
        )
    };

    ensure!(
        sent == inputs.len() as u32,
        "No se pudieron enviar todos los caracteres a la ventana enfocada"
    );

    Ok(())
}
