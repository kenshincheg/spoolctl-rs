//! Ensure only one SpoolCtl GUI process is running.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::sync::atomic::{AtomicIsize, Ordering};

use windows::{
    Win32::{
        Foundation::{CloseHandle, ERROR_ALREADY_EXISTS, GetLastError, HANDLE, HWND, LPARAM},
        System::Threading::CreateMutexW,
        UI::WindowsAndMessaging::{
            EnumWindows, GetWindowTextW, SW_RESTORE, SW_SHOW, SetForegroundWindow, ShowWindow,
        },
    },
    core::{BOOL, PCWSTR},
};

const MUTEX_NAME: &str = "Local\\SpoolCtl_GUI_SingleInstance";

/// Owned mutex handle (0 = none). Kept so we can release before UAC relaunch.
static GUI_MUTEX: AtomicIsize = AtomicIsize::new(0);

/// Returns `true` if this process owns the GUI slot.
/// If another GUI is already running, tries to bring it to the foreground and returns `false`.
pub fn try_acquire_gui() -> bool {
    let name = to_wide(OsStr::new(MUTEX_NAME));
    // SAFETY: NUL-terminated name.
    let handle = match unsafe { CreateMutexW(None, true, PCWSTR(name.as_ptr())) } {
        Ok(handle) => handle,
        Err(_) => return true, // fail open: allow GUI rather than block forever
    };

    let already = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    if already {
        let _ = unsafe { CloseHandle(handle) };
        focus_existing_gui();
        return false;
    }

    GUI_MUTEX.store(handle.0 as isize, Ordering::SeqCst);
    true
}

/// Releases the single-instance mutex so an elevated copy can start.
pub fn release_gui() {
    let raw = GUI_MUTEX.swap(0, Ordering::SeqCst);
    if raw != 0 {
        // SAFETY: value came from CreateMutexW in this process.
        let _ = unsafe { CloseHandle(HANDLE(raw as *mut _)) };
    }
}

fn focus_existing_gui() {
    // Second process: ask the running instance (via its tray HWND) to show the window.
    if crate::tray::activate_running_instance() {
        return;
    }
    // Last resort: SW_SHOW + restore any SpoolCtl GUI title.
    let mut found: Option<HWND> = None;
    let _ = unsafe {
        EnumWindows(
            Some(enum_spoolctl_windows),
            LPARAM(std::ptr::from_mut(&mut found) as isize),
        )
    };
    if let Some(hwnd) = found {
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
    }
}

unsafe extern "system" fn enum_spoolctl_windows(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let found = unsafe { &mut *(lparam.0 as *mut Option<HWND>) };
    let mut class = [0u16; 64];
    let cn = unsafe { windows::Win32::UI::WindowsAndMessaging::GetClassNameW(hwnd, &mut class) };
    if cn > 0 {
        let name = String::from_utf16_lossy(&class[..cn as usize]);
        if name == "SpoolCtlTrayHidden" {
            return BOOL(1);
        }
    }
    let mut buf = [0u16; 512];
    let len = unsafe { GetWindowTextW(hwnd, &mut buf) };
    if len <= 0 {
        return BOOL(1);
    }
    let title = String::from_utf16_lossy(&buf[..len as usize]);
    if title.starts_with("SpoolCtl") {
        *found = Some(hwnd);
        return BOOL(0);
    }
    BOOL(1)
}

fn to_wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}
