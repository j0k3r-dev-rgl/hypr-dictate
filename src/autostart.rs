use anyhow::Result;
use std::process::Command;

#[allow(dead_code)]
pub fn is_autostart_enabled() -> bool {
    let Ok(output) = Command::new("reg")
        .args([
            "query",
            "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
            "/v",
            "HyprDictate",
        ])
        .output()
    else {
        return false;
    };
    output.status.success()
}

pub fn set_autostart(enable: bool) -> Result<()> {
    if enable {
        let exe = std::env::current_exe()?;
        let path = exe.to_string_lossy().to_string();
        Command::new("reg")
            .args([
                "add",
                "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                "/v",
                "HyprDictate",
                "/t",
                "REG_SZ",
                "/d",
                &path,
                "/f",
            ])
            .output()?;
    } else {
        let _ = Command::new("reg")
            .args([
                "delete",
                "HKCU\\Software\\Microsoft\\Windows\\CurrentVersion\\Run",
                "/v",
                "HyprDictate",
                "/f",
            ])
            .output();
    }
    Ok(())
}
