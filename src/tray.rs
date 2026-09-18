//! System tray via Win32 Shell_NotifyIcon (reliable with elevated apps).
//! Hides the main window with ShowWindow(SW_HIDE) — egui Visible(false) stalls the loop.

use std::sync::atomic::{AtomicBool, AtomicIsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::thread;
use std::time::Duration;

use raw_window_handle::{HasWindowHandle, RawWindowHandle};
use windows::{
    Win32::{
        Foundation::{HINSTANCE, HWND, LPARAM, LRESULT, POINT, WPARAM},
        System::LibraryLoader::GetModuleHandleW,
        UI::{
            Shell::{
                NIF_ICON, NIF_INFO, NIF_MESSAGE, NIF_SHOWTIP, NIF_TIP, NIIF_INFO, NIM_ADD,
                NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW, Shell_NotifyIconW,
            },
            WindowsAndMessaging::{
                AppendMenuW, CREATESTRUCTW, CreatePopupMenu, CreateWindowExW, DefWindowProcW,
                DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, EnumWindows,
                GetClassNameW, GetCursorPos, GetMessageW, GetWindowLongPtrW, GetWindowTextW,
                GWLP_USERDATA, HICON, HWND_MESSAGE, IMAGE_ICON, IsWindow, LoadImageW, MF_STRING,
                PostMessageW, RegisterClassExW, SetForegroundWindow, SetMenuDefaultItem,
                SetWindowLongPtrW, ShowWindow, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON,
                TrackPopupMenu, TranslateMessage, CW_USEDEFAULT, IDI_APPLICATION, IMAGE_FLAGS,
                LR_DEFAULTSIZE, LR_SHARED, MSG, SW_HIDE, SW_RESTORE, SW_SHOW, WM_APP, WM_CLOSE,
                WM_COMMAND, WM_CONTEXTMENU, WM_CREATE, WM_DESTROY, WM_LBUTTONDBLCLK, WM_LBUTTONUP,
                WM_NULL, WM_QUIT, WM_RBUTTONUP, WNDCLASSEXW, WINDOW_EX_STYLE, WINDOW_STYLE,
                WS_EX_TOOLWINDOW,
            },
        },
    },
    core::{BOOL, PCWSTR, w},
};

use crate::log;

/// Main SpoolCtl window HWND (for show/hide).
static MAIN_HWND: AtomicIsize = AtomicIsize::new(0);
static TRAY_HWND: AtomicIsize = AtomicIsize::new(0);
static TRAY_READY: AtomicBool = AtomicBool::new(false);
/// Set by tray thread so GUI close handler allows real exit.
static ALLOW_EXIT: AtomicBool = AtomicBool::new(false);

const WM_TRAYICON: u32 = WM_APP + 40;
/// Custom: another process asks us to show the main window.
const WM_SPOOLCTL_ACTIVATE: u32 = WM_APP + 41;
const ID_SHOW: usize = 1;
const ID_HIDE: usize = 2;
const ID_QUIT: usize = 3;
const TRAY_UID: u32 = 1;
const TRAY_CLASS: PCWSTR = w!("SpoolCtlTrayHidden");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCmd {
    Show,
    Hide,
    Quit,
}

pub struct AppTray {
    rx: Receiver<TrayCmd>,
}

impl AppTray {
    /// Start a dedicated tray thread with its own message pump (needed for Shell_NotifyIcon).
    pub fn install(app_version: &str) -> Result<Self, String> {
        let (tx, rx) = mpsc::channel();
        let version = app_version.to_owned();

        thread::Builder::new()
            .name("spoolctl-tray".into())
            .spawn(move || {
                if let Err(error) = tray_thread_main(&version, tx) {
                    log::error(&format!("Поток трея: {error}"));
                }
            })
            .map_err(|e| format!("Не удалось запустить поток трея: {e}"))?;

        for _ in 0..50 {
            if TRAY_READY.load(Ordering::SeqCst) {
                break;
            }
            thread::sleep(Duration::from_millis(20));
        }
        if !TRAY_READY.load(Ordering::SeqCst) {
            return Err("Иконка трея не зарегистрировалась вовремя".to_owned());
        }

        Ok(Self { rx })
    }

    pub fn poll(&self) -> Option<TrayCmd> {
        match self.rx.try_recv() {
            Ok(cmd) => Some(cmd),
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => None,
        }
    }

    pub fn take_allow_exit() -> bool {
        ALLOW_EXIT.swap(false, Ordering::SeqCst)
    }

    pub fn set_tooltip(&self, text: &str) {
        let hwnd = tray_hwnd();
        if hwnd.0.is_null() {
            return;
        }
        let mut nid = base_nid(hwnd);
        nid.uFlags = NIF_TIP | NIF_SHOWTIP;
        write_wide(&mut nid.szTip, text);
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }

    pub fn balloon(&self, title: &str, body: &str) {
        let hwnd = tray_hwnd();
        if hwnd.0.is_null() {
            return;
        }
        let mut nid = base_nid(hwnd);
        nid.uFlags = NIF_INFO | NIF_SHOWTIP;
        write_wide(&mut nid.szInfoTitle, title);
        write_wide(&mut nid.szInfo, body);
        nid.dwInfoFlags = NIIF_INFO;
        unsafe {
            let _ = Shell_NotifyIconW(NIM_MODIFY, &nid);
        }
    }
}

impl Drop for AppTray {
    fn drop(&mut self) {
        let hwnd = tray_hwnd();
        if !hwnd.0.is_null() {
            unsafe {
                let _ = PostMessageW(Some(hwnd), WM_QUIT, WPARAM(0), LPARAM(0));
            }
        }
    }
}

fn tray_thread_main(version: &str, tx: Sender<TrayCmd>) -> Result<(), String> {
    let module = unsafe { GetModuleHandleW(None) }.map_err(|e| format!("GetModuleHandle: {e}"))?;
    let hinstance = HINSTANCE(module.0);

    let wc = WNDCLASSEXW {
        cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
        lpfnWndProc: Some(tray_wnd_proc),
        hInstance: hinstance,
        lpszClassName: TRAY_CLASS,
        ..Default::default()
    };
    let _ = unsafe { RegisterClassExW(&wc) };

    let tx_box = Box::new(tx);
    let tx_ptr = Box::into_raw(tx_box);

    // Distinct title so EnumWindows does not confuse this with the GUI.
    let title_wide: Vec<u16> = "SpoolCtl_TrayHost"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();

    let hwnd = unsafe {
        CreateWindowExW(
            WINDOW_EX_STYLE(WS_EX_TOOLWINDOW.0),
            TRAY_CLASS,
            PCWSTR(title_wide.as_ptr()),
            WINDOW_STYLE(0),
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            Some(HWND_MESSAGE),
            None,
            Some(hinstance),
            Some(tx_ptr as *const _),
        )
    };
    let hwnd = match hwnd {
        Ok(h) => h,
        Err(e) => {
            unsafe {
                drop(Box::from_raw(tx_ptr));
            }
            return Err(format!("CreateWindowEx (tray): {e}"));
        }
    };

    TRAY_HWND.store(hwnd.0 as isize, Ordering::SeqCst);

    let icon = load_tray_icon(hinstance);
    let mut nid = base_nid(hwnd);
    nid.hIcon = icon;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP | NIF_SHOWTIP;
    nid.uCallbackMessage = WM_TRAYICON;
    write_wide(
        &mut nid.szTip,
        &format!("SpoolCtl {version}\nЛКМ — окно, ПКМ — меню"),
    );

    // Classic callback (no NIM_SETVERSION): lParam is WM_L/RBUTTONUP directly.
    let added = unsafe { Shell_NotifyIconW(NIM_ADD, &nid) };
    if !added.as_bool() {
        unsafe {
            let _ = DestroyWindow(hwnd);
            drop(Box::from_raw(tx_ptr));
        }
        return Err("Shell_NotifyIcon(NIM_ADD) вернул FALSE".to_owned());
    }

    TRAY_READY.store(true, Ordering::SeqCst);
    log::info("Иконка трея зарегистрирована (ЛКМ — показать, ПКМ — меню)");

    let mut msg = MSG::default();
    loop {
        let ok = unsafe { GetMessageW(&mut msg, None, 0, 0) };
        if ok.0 == 0 || ok.0 == -1 {
            break;
        }
        unsafe {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }

    let del = base_nid(hwnd);
    unsafe {
        let _ = Shell_NotifyIconW(NIM_DELETE, &del);
        let shared = load_shared_app_icon();
        if !icon.is_invalid() && icon.0 != shared.0 {
            let _ = DestroyIcon(icon);
        }
        let _ = DestroyWindow(hwnd);
        drop(Box::from_raw(tx_ptr));
    }
    TRAY_READY.store(false, Ordering::SeqCst);
    TRAY_HWND.store(0, Ordering::SeqCst);
    Ok(())
}

unsafe extern "system" fn tray_wnd_proc(
    hwnd: HWND,
    msg: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match msg {
        WM_CREATE => {
            let create = lparam.0 as *const CREATESTRUCTW;
            if !create.is_null() {
                let tx_ptr = unsafe { (*create).lpCreateParams as isize };
                unsafe {
                    SetWindowLongPtrW(hwnd, GWLP_USERDATA, tx_ptr);
                }
            }
            LRESULT(0)
        }
        WM_TRAYICON => {
            // Classic notify: lParam is the mouse message. LOWORD-safe if version bits appear.
            let event = (lparam.0 as u32) & 0xffff;
            match event {
                WM_LBUTTONUP | WM_LBUTTONDBLCLK => {
                    activate_show(hwnd);
                }
                WM_RBUTTONUP | WM_CONTEXTMENU => {
                    show_context_menu(hwnd);
                }
                _ => {}
            }
            LRESULT(0)
        }
        WM_SPOOLCTL_ACTIVATE => {
            activate_show(hwnd);
            LRESULT(0)
        }
        WM_COMMAND => {
            match wparam.0 & 0xffff {
                ID_SHOW => activate_show(hwnd),
                ID_HIDE => {
                    hide_main_window();
                    send_cmd(hwnd, TrayCmd::Hide);
                }
                ID_QUIT => request_quit(hwnd),
                _ => {}
            }
            LRESULT(0)
        }
        WM_DESTROY => LRESULT(0),
        _ => unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) },
    }
}

/// Show the GUI immediately (from tray thread) and notify the egui side.
fn activate_show(tray_hwnd: HWND) {
    show_main_window();
    send_cmd(tray_hwnd, TrayCmd::Show);
}

fn request_quit(tray_hwnd: HWND) {
    ALLOW_EXIT.store(true, Ordering::SeqCst);
    show_main_window();
    send_cmd(tray_hwnd, TrayCmd::Quit);
    // Wake egui / trigger close even if the channel is slow.
    let main = current_hwnd();
    if !main.0.is_null() && unsafe { IsWindow(Some(main)).as_bool() } {
        unsafe {
            let _ = PostMessageW(Some(main), WM_CLOSE, WPARAM(0), LPARAM(0));
        }
    }
}

fn send_cmd(hwnd: HWND, cmd: TrayCmd) {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const Sender<TrayCmd>;
    if ptr.is_null() {
        return;
    }
    let tx = unsafe { &*ptr };
    let _ = tx.send(cmd);
}

fn show_context_menu(hwnd: HWND) {
    unsafe {
        let Ok(menu) = CreatePopupMenu() else {
            return;
        };
        let _ = AppendMenuW(menu, MF_STRING, ID_SHOW, w!("Показать окно"));
        let _ = AppendMenuW(menu, MF_STRING, ID_HIDE, w!("Скрыть в трей"));
        let _ = AppendMenuW(menu, MF_STRING, ID_QUIT, w!("Выход"));
        let _ = SetMenuDefaultItem(menu, ID_SHOW as u32, 0);

        let mut pt = POINT::default();
        let _ = GetCursorPos(&mut pt);
        let _ = SetForegroundWindow(hwnd);
        let cmd = TrackPopupMenu(
            menu,
            TPM_LEFTALIGN | TPM_RIGHTBUTTON | TPM_RETURNCMD,
            pt.x,
            pt.y,
            Some(0),
            hwnd,
            None,
        );
        let _ = PostMessageW(Some(hwnd), WM_NULL, WPARAM(0), LPARAM(0));
        let _ = DestroyMenu(menu);
        match cmd.0 as usize {
            ID_SHOW => activate_show(hwnd),
            ID_HIDE => {
                hide_main_window();
                send_cmd(hwnd, TrayCmd::Hide);
            }
            ID_QUIT => request_quit(hwnd),
            _ => {}
        }
    }
}

fn base_nid(hwnd: HWND) -> NOTIFYICONDATAW {
    let mut nid = NOTIFYICONDATAW::default();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = hwnd;
    nid.uID = TRAY_UID;
    nid
}

fn write_wide(buf: &mut [u16], text: &str) {
    buf.fill(0);
    let max = buf.len().saturating_sub(1);
    for (i, unit) in text.encode_utf16().take(max).enumerate() {
        buf[i] = unit;
    }
}

fn load_tray_icon(hinstance: HINSTANCE) -> HICON {
    let from_res = unsafe {
        LoadImageW(
            Some(hinstance),
            PCWSTR(1usize as *const u16),
            IMAGE_ICON,
            16,
            16,
            IMAGE_FLAGS(0),
        )
    };
    if let Ok(handle) = from_res {
        return HICON(handle.0);
    }
    load_shared_app_icon()
}

fn load_shared_app_icon() -> HICON {
    let shared = unsafe {
        LoadImageW(
            None,
            IDI_APPLICATION,
            IMAGE_ICON,
            16,
            16,
            LR_SHARED | LR_DEFAULTSIZE,
        )
    };
    match shared {
        Ok(h) => HICON(h.0),
        Err(_) => HICON(std::ptr::null_mut()),
    }
}

fn tray_hwnd() -> HWND {
    HWND(TRAY_HWND.load(Ordering::SeqCst) as *mut _)
}

pub fn remember_main_hwnd_from_cc(cc: &eframe::CreationContext<'_>) {
    if let Ok(handle) = cc.window_handle()
        && let RawWindowHandle::Win32(win) = handle.as_raw()
    {
        MAIN_HWND.store(win.hwnd.get() as isize, Ordering::SeqCst);
    }
}

pub fn hide_main_window() {
    ensure_main_hwnd();
    let hwnd = current_hwnd();
    if hwnd.0.is_null() {
        return;
    }
    unsafe {
        let _ = ShowWindow(hwnd, SW_HIDE);
    }
}

pub fn show_main_window() {
    ensure_main_hwnd();
    let hwnd = current_hwnd();
    if hwnd.0.is_null() {
        log::warn("Показ окна: HWND главного окна не найден");
        return;
    }
    unsafe {
        let _ = ShowWindow(hwnd, SW_SHOW);
        let _ = ShowWindow(hwnd, SW_RESTORE);
        let _ = SetForegroundWindow(hwnd);
    }
}

fn ensure_main_hwnd() {
    let cur = current_hwnd();
    if !cur.0.is_null() && unsafe { IsWindow(Some(cur)).as_bool() } {
        return;
    }
    if let Some(hwnd) = find_main_gui_hwnd() {
        MAIN_HWND.store(hwnd.0 as isize, Ordering::SeqCst);
    }
}

fn find_main_gui_hwnd() -> Option<HWND> {
    let mut found: Option<HWND> = None;
    unsafe {
        let _ = EnumWindows(
            Some(enum_spoolctl_main),
            LPARAM(std::ptr::from_mut(&mut found) as isize),
        );
    }
    found
}

unsafe extern "system" fn enum_spoolctl_main(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let found = unsafe { &mut *(lparam.0 as *mut Option<HWND>) };
    if is_tray_host(hwnd) {
        return BOOL(1);
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

fn is_tray_host(hwnd: HWND) -> bool {
    let mut class = [0u16; 64];
    let n = unsafe { GetClassNameW(hwnd, &mut class) };
    if n <= 0 {
        return false;
    }
    let name = String::from_utf16_lossy(&class[..n as usize]);
    name == "SpoolCtlTrayHidden"
}

pub fn current_hwnd() -> HWND {
    HWND(MAIN_HWND.load(Ordering::SeqCst) as *mut _)
}

/// Called from a second SpoolCtl process: ask the running instance to show its window.
pub fn activate_running_instance() -> bool {
    // Prefer posting to the tray host (always exists while GUI runs in tray).
    let mut tray: Option<HWND> = None;
    unsafe {
        let _ = EnumWindows(
            Some(enum_tray_host),
            LPARAM(std::ptr::from_mut(&mut tray) as isize),
        );
    }
    if let Some(hwnd) = tray {
        unsafe {
            let _ = PostMessageW(Some(hwnd), WM_SPOOLCTL_ACTIVATE, WPARAM(0), LPARAM(0));
        }
        return true;
    }
    // Fallback: show any SpoolCtl GUI window directly (needs SW_SHOW for hidden).
    if let Some(hwnd) = find_main_gui_hwnd() {
        unsafe {
            let _ = ShowWindow(hwnd, SW_SHOW);
            let _ = ShowWindow(hwnd, SW_RESTORE);
            let _ = SetForegroundWindow(hwnd);
        }
        return true;
    }
    false
}

unsafe extern "system" fn enum_tray_host(hwnd: HWND, lparam: LPARAM) -> BOOL {
    let found = unsafe { &mut *(lparam.0 as *mut Option<HWND>) };
    if is_tray_host(hwnd) {
        *found = Some(hwnd);
        return BOOL(0);
    }
    BOOL(1)
}
