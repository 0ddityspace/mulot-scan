//! Launching Mulot-scan when the user logs in, hidden in the notification area.
//!
//! Windows: value in `HKCU\Software\Microsoft\Windows\CurrentVersion\Run`.
//! Elsewhere the option is not offered (a Wayland window cannot be hidden).

use anyhow::Result;

/// Command-line flag used by the login entry.
pub const HIDDEN_FLAG: &str = "--tray";

#[cfg(windows)]
mod imp {
    use anyhow::{Context, Result};
    use windows::Win32::System::Registry::{
        HKEY, HKEY_CURRENT_USER, KEY_QUERY_VALUE, KEY_SET_VALUE, REG_SZ, RRF_RT_REG_SZ, RegCloseKey, RegDeleteValueW,
        RegGetValueW, RegOpenKeyExW, RegSetValueExW,
    };
    use windows::core::w;

    const RUN: windows::core::PCWSTR = w!("Software\\Microsoft\\Windows\\CurrentVersion\\Run");
    const VALUE: windows::core::PCWSTR = w!("Mulot-scan");

    pub fn is_enabled() -> bool {
        // SAFETY: constant, NUL-terminated strings; no output buffer requested.
        unsafe { RegGetValueW(HKEY_CURRENT_USER, RUN, VALUE, RRF_RT_REG_SZ, None, None, None).is_ok() }
    }

    pub fn set(enabled: bool) -> Result<()> {
        let mut key = HKEY::default();
        // SAFETY: `key` is a valid out pointer; it is closed below.
        unsafe { RegOpenKeyExW(HKEY_CURRENT_USER, RUN, None, KEY_SET_VALUE | KEY_QUERY_VALUE, &mut key) }
            .ok()
            .context("ouverture de la clé Run du registre")?;
        let result = if enabled {
            let exe = std::env::current_exe()?;
            let cmd = format!("\"{}\" {}", exe.display(), super::HIDDEN_FLAG);
            let bytes: Vec<u8> = cmd.encode_utf16().chain([0]).flat_map(u16::to_le_bytes).collect();
            // SAFETY: `key` is open; `bytes` is a NUL-terminated UTF-16 string.
            unsafe { RegSetValueExW(key, VALUE, None, REG_SZ, Some(&bytes)) }.ok()
        } else {
            // SAFETY: `key` is open; a missing value is not an error for us.
            match unsafe { RegDeleteValueW(key, VALUE) }.ok() {
                Err(e) if e.code() == windows::Win32::Foundation::ERROR_FILE_NOT_FOUND.to_hresult() => Ok(()),
                r => r,
            }
        };
        // SAFETY: closing the key opened above.
        let _ = unsafe { RegCloseKey(key) };
        result.context("écriture dans le registre")
    }
}

#[cfg(not(windows))]
mod imp {
    pub fn is_enabled() -> bool {
        false
    }

    pub fn set(_enabled: bool) -> anyhow::Result<()> {
        anyhow::bail!("lancement au démarrage disponible sous Windows seulement")
    }
}

pub fn is_enabled() -> bool {
    imp::is_enabled()
}

pub fn set(enabled: bool) -> Result<()> {
    imp::set(enabled)
}
