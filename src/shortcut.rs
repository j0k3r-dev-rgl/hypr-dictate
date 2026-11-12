use crate::{hypr, settings::binding_block};
use anyhow::{Context, Result, ensure};
use std::io::Write;

pub fn apply(shortcut: &str, formal: &str, old: &str, old_formal: &str) -> Result<()> {
    ensure!(
        shortcut.is_empty() || shortcut != formal,
        "Los dos modos necesitan atajos distintos"
    );
    let binds: serde_json::Value = serde_json::from_slice(&hypr(&["-j", "binds"])?)?;
    for shortcut in [shortcut, formal] {
        ensure!(
            shortcut.is_empty()
                || shortcut
                    .split('+')
                    .next_back()
                    .is_some_and(|s| !s.trim().is_empty()),
            "Atajo vacío"
        );
        if !shortcut.is_empty() && shortcut != old && shortcut != old_formal {
            let parts: Vec<_> = shortcut.split('+').map(str::trim).collect();
            let key = parts.last().unwrap();
            let mut mask = 0;
            for modifier in &parts[..parts.len() - 1] {
                mask |= match *modifier {
                    "SUPER" => 64,
                    "CTRL" => 4,
                    "ALT" => 8,
                    "SHIFT" => 1,
                    _ => anyhow::bail!("Modificador inválido"),
                };
            }
            ensure!(
                !binds
                    .as_array()
                    .context("Lista de atajos inválida")?
                    .iter()
                    .any(|b| b["modmask"].as_i64() == Some(mask)
                        && b["key"]
                            .as_str()
                            .is_some_and(|k| k.eq_ignore_ascii_case(key))),
                "Ese atajo ya está ocupado en Hyprland"
            );
        }
    }
    let path = std::path::PathBuf::from(std::env::var_os("HOME").context("Falta HOME")?)
        .join(".config/hypr/keybinds.lua");
    for p in path.ancestors() {
        ensure!(
            !std::fs::symlink_metadata(p)?.file_type().is_symlink(),
            "Symlink inesperado en configuración Hyprland"
        );
    }
    let original = std::fs::read_to_string(&path)?;
    let exe = std::env::current_exe()?;
    let updated = binding_block(
        &original,
        shortcut,
        formal,
        exe.to_str().context("Ruta no UTF-8")?,
    )?;
    let mut temporary = tempfile::NamedTempFile::new_in(path.parent().unwrap())?;
    temporary
        .as_file()
        .set_permissions(std::fs::metadata(&path)?.permissions())?;
    temporary.write_all(updated.as_bytes())?;
    temporary.as_file().sync_all()?;
    let syntax = std::process::Command::new("luac")
        .arg("-p")
        .arg(temporary.path())
        .output()
        .context("Necesitas luac para validar el atajo antes de guardarlo")?;
    ensure!(
        syntax.status.success(),
        "El bloque generado no pasa la validación Lua"
    );
    ensure!(
        std::fs::read_to_string(&path)? == original,
        "La configuración cambió mientras se guardaba"
    );
    temporary.persist(&path)?;
    if let Err(error) = hypr(&["reload", "config-only"]) {
        std::fs::write(&path, original)?;
        return Err(error);
    }
    let errors = hypr(&["configerrors"])?;
    if !String::from_utf8_lossy(&errors).trim().is_empty() {
        std::fs::write(&path, original)?;
        let _ = hypr(&["reload", "config-only"]);
        anyhow::bail!("Hyprland rechazó el cambio; se restauró el archivo anterior");
    }
    Ok(())
}

pub fn shortcut_held(shortcut: &str) -> Option<bool> {
    if shortcut.is_empty() {
        return None;
    }
    let mut groups: Vec<&[u16]> = Vec::new();
    for token in shortcut.split('+').map(str::trim) {
        groups.push(match token {
            "SUPER" => &[125, 126],
            "CTRL" => &[29, 97],
            "ALT" => &[56, 100],
            "SHIFT" => &[42, 54],
            "D" => &[32],
            "F9" => &[67],
            "F10" => &[68],
            _ => return None,
        });
    }
    let mut combined = [0u8; 96];
    let mut readable = false;
    for entry in std::fs::read_dir("/dev/input").ok()?.flatten() {
        let name = entry.file_name();
        if !name.to_string_lossy().starts_with("event") {
            continue;
        }
        if let Ok(file) = std::fs::File::open(entry.path()) {
            let mut bits = [0u8; 96];
            // EVIOCGKEY(96): _IOR('E', 0x18, [u8; 96]) = 0x80604518
            let rc = unsafe {
                libc::ioctl(
                    std::os::fd::AsRawFd::as_raw_fd(&file),
                    0x80604518u64 as _,
                    bits.as_mut_ptr(),
                )
            };
            if rc >= 0 {
                readable = true;
                for (dst, src) in combined.iter_mut().zip(bits) {
                    *dst |= src;
                }
            }
        }
    }
    if !readable {
        return None;
    }
    Some(groups.into_iter().all(|group| {
        group
            .iter()
            .any(|code| (combined[(*code as usize) / 8] & (1 << ((*code as usize) % 8))) != 0)
    }))
}

pub fn any_modifier_held() -> bool {
    let mut combined = [0u8; 96];
    let Ok(entries) = std::fs::read_dir("/dev/input") else {
        return false;
    };
    for entry in entries.flatten() {
        if !entry.file_name().to_string_lossy().starts_with("event") {
            continue;
        }
        if let Ok(file) = std::fs::File::open(entry.path()) {
            let mut bits = [0u8; 96];
            let rc = unsafe {
                libc::ioctl(
                    std::os::fd::AsRawFd::as_raw_fd(&file),
                    0x80604518u64 as _,
                    bits.as_mut_ptr(),
                )
            };
            if rc >= 0 {
                for (dst, src) in combined.iter_mut().zip(bits) {
                    *dst |= src;
                }
            }
        }
    }
    // CTRL(29,97), SHIFT(42,54), ALT(56,100), SUPER(125,126)
    [29usize, 97, 42, 54, 56, 100, 125, 126]
        .into_iter()
        .any(|code| (combined[code / 8] & (1 << (code % 8))) != 0)
}
