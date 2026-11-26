//! Iniciar Acustia con Windows: entrada `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//!
//! No requiere permisos de administrador. Se usa `reg.exe` (viene con Windows) para no
//! añadir dependencias ni más código `unsafe`.

#[cfg(windows)]
mod imp {
    use std::os::windows::process::CommandExt;
    use std::process::Command;

    const KEY: &str = r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run";
    const NAME: &str = "AcustiaPlayer";
    /// Que `reg.exe` no abra una ventana de consola.
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;

    fn reg() -> Command {
        let mut c = Command::new("reg");
        c.creation_flags(CREATE_NO_WINDOW);
        c
    }

    pub fn supported() -> bool {
        true
    }

    pub fn is_enabled() -> bool {
        reg()
            .args(["query", KEY, "/v", NAME])
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    }

    pub fn set(enabled: bool) -> Result<(), String> {
        let out = if enabled {
            let exe = std::env::current_exe().map_err(|e| e.to_string())?;
            let value = format!("\"{}\"", exe.display());
            reg()
                .args(["add", KEY, "/v", NAME, "/t", "REG_SZ", "/d", &value, "/f"])
                .output()
        } else {
            if !is_enabled() {
                return Ok(());
            }
            reg().args(["delete", KEY, "/v", NAME, "/f"]).output()
        }
        .map_err(|e| e.to_string())?;
        if out.status.success() {
            Ok(())
        } else {
            Err(String::from_utf8_lossy(&out.stderr).trim().to_string())
        }
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn supported() -> bool {
        false
    }
    pub fn set(_enabled: bool) -> Result<(), String> {
        Err("El inicio con el equipo solo está disponible en Windows.".into())
    }
}

pub use imp::{set, supported};
