//! UAC elevation helpers (relaunch with runas + `--show` + optional `--pos`).

use std::env;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use eframe::egui;

use windows::{
    Win32::{
        Foundation::{CloseHandle, HANDLE, HWND, RECT},
        Security::{GetTokenInformation, TOKEN_ELEVATION, TOKEN_QUERY, TokenElevation},
        System::Threading::{GetCurrentProcess, OpenProcessToken},
        UI::{
            Shell::ShellExecuteW,
            WindowsAndMessaging::{GetForegroundWindow, GetWindowRect, SW_SHOWNORMAL},
        },
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

/// Top-left of the SpoolCtl window (virtual-screen coords, multi-monitor OK).
pub fn current_window_pos(ctx: &egui::Context) -> Option<(i32, i32)> {
    let main = crate::tray::current_hwnd();
    if let Some(p) = window_rect_pos(main) {
        return Some(p);
    }
    if let Some(p) = window_rect_pos(unsafe { GetForegroundWindow() }) {
        return Some(p);
    }
    ctx.input(|i| {
        i.viewport()
            .outer_rect
            .map(|r| (r.min.x.round() as i32, r.min.y.round() as i32))
    })
}

fn window_rect_pos(hwnd: HWND) -> Option<(i32, i32)> {
    if hwnd.0.is_null() {
        return None;
    }
    let mut rect = RECT::default();
    // SAFETY: GetWindowRect for a live HWND.
    unsafe { GetWindowRect(hwnd, &mut rect) }.ok()?;
    Some((rect.left, rect.top))
}

/// Relaunches this EXE via UAC (`runas`) with `--show` and optional `--pos=x,y`.
/// Caller must release single-instance mutex first, then exit on success.
pub fn relaunch_elevated(pos: Option<(i32, i32)>) -> Result<(), String> {
    if is_elevated() {
        return Err("Уже запущено с правами администратора.".to_owned());
    }

    let exe = env::current_exe().map_err(|err| format!("Не удалось найти путь к EXE: {err}"))?;
    let exe_wide = to_wide(exe.as_os_str());
    let verb = to_wide(OsStr::new("runas"));
    let params_owned = match pos {
        Some((x, y)) => format!("--show --pos={x},{y}"),
        None => "--show".to_owned(),
    };
    let params = to_wide(OsStr::new(&params_owned));

    // SAFETY: NUL-terminated wide strings; ShellExecuteW does not take ownership.
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(exe_wide.as_ptr()),
            PCWSTR(params.as_ptr()),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };

    // Per MSDN, values > 32 mean the function succeeded.
    if result.0 as isize <= 32 {
        return Err(format!(
            "Не удалось запросить права администратора (код {}). Отмена UAC?",
            result.0 as isize
        ));
    }
    Ok(())
}

/// Parse `--pos=x,y` from argv (screen coordinates).
pub fn parse_pos_arg(args: &[String]) -> Option<(f32, f32)> {
    for a in args {
        let Some(rest) = a.strip_prefix("--pos=") else {
            continue;
        };
        let mut parts = rest.split(',');
        let x: f32 = parts.next()?.trim().parse().ok()?;
        let y: f32 = parts.next()?.trim().parse().ok()?;
        return Some((x, y));
    }
    None
}

fn to_wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}
