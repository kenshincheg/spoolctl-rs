//! Autostart SpoolCtl at user logon with administrator rights.
//! Uses Task Scheduler (`/RL HIGHEST`) so after reboot the process is elevated
//! (for accounts in the Administrators group). Creating the task needs elevation once.
//! Legacy HKCU Run entries are removed when enabling/disabling.

use std::os::windows::process::CommandExt;
use std::path::PathBuf;
use std::process::Command;

use windows::Win32::{
    Foundation::{ERROR_FILE_NOT_FOUND, WIN32_ERROR},
    System::{
        Registry::{
            HKEY_CURRENT_USER, KEY_READ, KEY_SET_VALUE, KEY_WRITE, REG_SZ, RegCloseKey,
            RegDeleteValueW, RegOpenKeyExW, RegQueryValueExW,
        },
        Threading::CREATE_NO_WINDOW,
    },
};
use windows::core::PCWSTR;

use crate::elevate;
use crate::log;

const TASK_NAME: &str = "SpoolCtl";
const RUN_KEY: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "SpoolCtl";

/// Absolute EXE path without `\\?\` prefix.
pub fn exe_path_clean() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| format!("current_exe: {e}"))?;
    let canon = exe.canonicalize().unwrap_or(exe);
    let s = canon.to_string_lossy();
    let trimmed = s.trim_start_matches(r"\\?\");
    Ok(PathBuf::from(trimmed))
}

/// Argument for `schtasks /TR`.
fn task_tr() -> Result<String, String> {
    let path = exe_path_clean()?;
    let path = path.to_string_lossy();
    if path.contains(' ') {
        Ok(format!("\"{path}\" --tray"))
    } else {
        Ok(format!("{path} --tray"))
    }
}

pub fn is_enabled() -> bool {
    task_exists() || legacy_run_enabled()
}

pub fn enable() -> Result<(), String> {
    if !elevate::is_elevated() {
        return Err(
            "Автозапуск с правами администратора: откройте SpoolCtl «от имени администратора» и снова включите галочку (один раз)."
                .to_owned(),
        );
    }
    let tr = task_tr()?;
    // /SC ONLOGON — при входе текущего пользователя; /RL HIGHEST — elevated для админов.
    let output = schtasks(&[
        "/Create",
        "/TN",
        TASK_NAME,
        "/TR",
        &tr,
        "/SC",
        "ONLOGON",
        "/RL",
        "HIGHEST",
        "/F",
    ])?;
    if !output.status.success() {
        let err = String::from_utf8_lossy(&output.stderr);
        let out = String::from_utf8_lossy(&output.stdout);
        return Err(format!(
            "Не удалось создать задачу автозапуска: {} {}",
            err.trim(),
            out.trim()
        ));
    }
    // Avoid double start from old HKCU Run.
    let _ = delete_legacy_run();
    log::info(&format!(
        "Автозапуск: задача планировщика «{TASK_NAME}» (/RL HIGHEST, --tray)"
    ));
    Ok(())
}

pub fn disable() -> Result<(), String> {
    if task_exists() {
        if !elevate::is_elevated() {
            // Deleting an HIGHEST task usually needs admin.
            return Err(
                "Чтобы выключить автозапуск с правами администратора, откройте SpoolCtl «от имени администратора»."
                    .to_owned(),
            );
        }
        let output = schtasks(&["/Delete", "/TN", TASK_NAME, "/F"])?;
        if !output.status.success() {
            let err = String::from_utf8_lossy(&output.stderr);
            let out = String::from_utf8_lossy(&output.stdout);
            return Err(format!(
                "Не удалось удалить задачу автозапуска: {} {}",
                err.trim(),
                out.trim()
            ));
        }
        log::info("Автозапуск: задача планировщика удалена");
    }
    let _ = delete_legacy_run();
    Ok(())
}

fn task_exists() -> bool {
    schtasks(&["/Query", "/TN", TASK_NAME])
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn schtasks(args: &[&str]) -> Result<std::process::Output, String> {
    Command::new("schtasks")
        .args(args)
        .creation_flags(CREATE_NO_WINDOW.0)
        .output()
        .map_err(|e| format!("schtasks: {e}"))
}

fn legacy_run_enabled() -> bool {
    read_run_value()
        .ok()
        .flatten()
        .is_some_and(|v| {
            let lower = v.to_lowercase();
            lower.contains("spoolctl")
        })
}

fn read_run_value() -> Result<Option<String>, String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    let key_wide: Vec<u16> = OsStr::new(RUN_KEY)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let name_wide: Vec<u16> = OsStr::new(RUN_VALUE)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut hkey = Default::default();
    let open = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_wide.as_ptr()),
            Some(0),
            KEY_READ,
            &mut hkey,
        )
    };
    if open != WIN32_ERROR(0) {
        return Ok(None);
    }
    let mut ty = REG_SZ;
    let mut needed = 0u32;
    let probe = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name_wide.as_ptr()),
            None,
            Some(&mut ty),
            None,
            Some(&mut needed),
        )
    };
    if probe != WIN32_ERROR(0) {
        unsafe {
            let _ = RegCloseKey(hkey);
        }
        if probe == ERROR_FILE_NOT_FOUND {
            return Ok(None);
        }
        return Ok(None);
    }
    if needed == 0 {
        unsafe {
            let _ = RegCloseKey(hkey);
        }
        return Ok(None);
    }
    let mut buf = vec![0u8; needed as usize];
    let read = unsafe {
        RegQueryValueExW(
            hkey,
            PCWSTR(name_wide.as_ptr()),
            None,
            Some(&mut ty),
            Some(buf.as_mut_ptr()),
            Some(&mut needed),
        )
    };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    if read != WIN32_ERROR(0) {
        return Ok(None);
    }
    let u16s: Vec<u16> = buf
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    Ok(Some(String::from_utf16_lossy(&u16s)))
}

fn delete_legacy_run() -> Result<(), String> {
    use std::ffi::OsStr;
    use std::os::windows::ffi::OsStrExt;

    let key_wide: Vec<u16> = OsStr::new(RUN_KEY)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let name_wide: Vec<u16> = OsStr::new(RUN_VALUE)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let mut hkey = Default::default();
    let open = unsafe {
        RegOpenKeyExW(
            HKEY_CURRENT_USER,
            PCWSTR(key_wide.as_ptr()),
            Some(0),
            KEY_WRITE | KEY_SET_VALUE,
            &mut hkey,
        )
    };
    if open != WIN32_ERROR(0) {
        return Ok(());
    }
    let del = unsafe { RegDeleteValueW(hkey, PCWSTR(name_wide.as_ptr())) };
    unsafe {
        let _ = RegCloseKey(hkey);
    }
    if del != WIN32_ERROR(0) && del != ERROR_FILE_NOT_FOUND {
        return Err(format!("RegDeleteValue: код {}", del.0));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn task_tr_includes_tray() {
        let tr = task_tr().expect("exe");
        assert!(tr.contains("--tray"), "{tr}");
    }
}
