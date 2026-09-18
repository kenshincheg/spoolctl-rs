//! UAC elevation helpers (relaunch with runas).

use std::env;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE},
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
        UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    },
    core::PCWSTR,
};

/// Returns true if the current process token is elevated.
pub fn is_elevated() -> bool {
    query_elevated().unwrap_or_default()
}

fn query_elevated() -> Result<bool, String> {
    let mut token = HANDLE::default();
    // SAFETY: current process handle is valid; token is closed below.
    unsafe { OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token) }
        .map_err(|err| format!("OpenProcessToken: {err}"))?;

    let elevated = (|| {
        let mut elevation = TOKEN_ELEVATION::default();
        let mut returned = 0u32;
        // SAFETY: buffer size matches TOKEN_ELEVATION; only used on success.
        unsafe {
            GetTokenInformation(
                token,
                TokenElevation,
                Some(std::ptr::from_mut(&mut elevation).cast()),
                u32::try_from(std::mem::size_of_val(&elevation)).unwrap_or(0),
                &mut returned,
            )
        }
        .map_err(|err| format!("GetTokenInformation: {err}"))?;
        Ok::<bool, String>(elevation.TokenIsElevated != 0)
    })();

    // SAFETY: token opened by OpenProcessToken in this function.
    let _ = unsafe { CloseHandle(token) };
    elevated
}

/// Relaunches this EXE via UAC (`runas`). Caller should exit on success.
pub fn relaunch_elevated() -> Result<(), String> {
    if is_elevated() {
        return Err("Уже запущено с правами администратора.".to_owned());
    }

    let exe = env::current_exe().map_err(|err| format!("Не удалось найти путь к EXE: {err}"))?;
    let exe_wide = to_wide(exe.as_os_str());
    let verb = to_wide(OsStr::new("runas"));

    // SAFETY: NUL-terminated wide strings; ShellExecuteW does not take ownership.
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(exe_wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };

    // Per MSDN, values > 32 mean the function succeeded.
    if result.0 as isize <= 32 {
        return Err(format!(
            "Не удалось запросить права администратора (код {}).",
            result.0 as isize
        ));
    }
    Ok(())
}

fn to_wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}
