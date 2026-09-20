//! Notify when a new print job appears (local printers).
//! Background watcher: Spooler change notifications + 1s poll (independent of egui/tray hide).

use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::host::Host;
use crate::log;
use crate::queue::{self, PrinterEntry};
use crate::tray;

const POLL_MS: u64 = 1000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Settings {
    /// Balloon when a job is submitted to any local printer.
    pub notify_print_jobs: bool,
}

pub struct PrintNotify {
    pub settings: Settings,
    rx: Receiver<Vec<String>>,
    event_tx: Sender<Vec<String>>,
    stop: Option<Arc<AtomicBool>>,
}

impl std::fmt::Debug for PrintNotify {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrintNotify")
            .field("settings", &self.settings)
            .field("watching", &self.stop.is_some())
            .finish()
    }
}

impl PrintNotify {
    pub fn load() -> Self {
        let (event_tx, rx) = mpsc::channel();
        let mut s = Self {
            settings: load_settings(),
            rx,
            event_tx,
            stop: None,
        };
        if s.settings.notify_print_jobs {
            s.start_watcher();
        }
        s
    }

    pub fn save_settings(&self) {
        save_settings(&self.settings);
    }

    /// Enable/disable watcher and persist.
    pub fn set_enabled(&mut self, on: bool) {
        self.settings.notify_print_jobs = on;
        self.save_settings();
        if on {
            self.start_watcher();
        } else {
            self.stop_watcher();
        }
    }

    fn start_watcher(&mut self) {
        self.stop_watcher();
        let stop = Arc::new(AtomicBool::new(false));
        let stop_thread = Arc::clone(&stop);
        let tx = self.event_tx.clone();
        thread::Builder::new()
            .name("spoolctl-print-notify".into())
            .spawn(move || watcher_main(stop_thread, tx))
            .ok();
        self.stop = Some(stop);
        log::info("Уведомления о печати: фоновое слежение запущено");
    }

    fn stop_watcher(&mut self) {
        if let Some(stop) = self.stop.take() {
            stop.store(true, Ordering::SeqCst);
            log::info("Уведомления о печати: фоновое слежение остановлено");
        }
    }

    /// Drain pending printer-name batches from the watcher.
    pub fn poll_events(&mut self) -> Vec<String> {
        let mut names = HashSet::new();
        while let Ok(batch) = self.rx.try_recv() {
            for n in batch {
                names.insert(n);
            }
        }
        let mut out: Vec<String> = names.into_iter().collect();
        out.sort();
        out
    }
}

impl Drop for PrintNotify {
    fn drop(&mut self) {
        self.stop_watcher();
    }
}

pub fn message_ru(printer_names: &[String]) -> String {
    match printer_names {
        [] => String::new(),
        [one] => format!("На принтер «{one}» отправлено на печать."),
        many => {
            let list = many.join(", ");
            format!("Отправлено на печать: {list}.")
        }
    }
}

fn watcher_main(stop: Arc<AtomicBool>, tx: Sender<Vec<String>>) {
    let mut baseline: Option<HashMap<String, usize>> = None;
    // Prefer Spooler change handle; fall back to sleep-only poll.
    let change = open_job_change_notification();
    if change.is_some() {
        log::info("Уведомления о печати: подписка Spooler (ADD_JOB) активна");
    } else {
        log::warn("Уведомления о печати: только опрос 1 с (подписка Spooler недоступна)");
    }

    while !stop.load(Ordering::SeqCst) {
        let woken = wait_for_change_or_timeout(change, Duration::from_millis(POLL_MS));
        if stop.load(Ordering::SeqCst) {
            break;
        }
        // After Spooler wake, retry a few times — jobs can appear briefly.
        let attempts = if woken { 5 } else { 1 };
        for i in 0..attempts {
            if stop.load(Ordering::SeqCst) {
                break;
            }
            let printers = match queue::list_printers_on(&Host::local()) {
                Ok(p) => p,
                Err(e) => {
                    log::warn(&format!("Уведомления о печати: список принтеров: {e}"));
                    break;
                }
            };
            if let Some(names) = detect_new_jobs(&mut baseline, &printers) {
                emit_notify(&tx, names);
                break;
            }
            if woken && i + 1 < attempts {
                thread::sleep(Duration::from_millis(80));
            }
        }
    }

    if let Some(h) = change {
        close_change_notification(h);
    }
}

fn detect_new_jobs(
    baseline: &mut Option<HashMap<String, usize>>,
    printers: &[PrinterEntry],
) -> Option<Vec<String>> {
    let mut current = HashMap::with_capacity(printers.len());
    for p in printers {
        current.insert(p.name.clone(), p.jobs);
    }
    let Some(prev) = baseline.as_ref() else {
        *baseline = Some(current);
        return None;
    };
    let mut newly = Vec::new();
    for p in printers {
        let before = prev.get(&p.name).copied().unwrap_or(0);
        if p.jobs > before {
            newly.push(p.name.clone());
        }
    }
    *baseline = Some(current);
    if newly.is_empty() {
        None
    } else {
        Some(newly)
    }
}

fn emit_notify(tx: &Sender<Vec<String>>, names: Vec<String>) {
    let text = message_ru(&names);
    log::info(&text);
    // Balloon from watcher so it works even if egui is asleep in tray.
    tray::balloon_now("SpoolCtl — печать", &text);
    let _ = tx.send(names);
}

fn open_job_change_notification() -> Option<windows::Win32::Foundation::HANDLE> {
    use windows::Win32::Graphics::Printing::{
        ClosePrinter, FindFirstPrinterChangeNotification, OpenPrinterW, PRINTER_CHANGE_ADD_JOB,
        PRINTER_HANDLE,
    };
    use windows::core::PCWSTR;

    let mut printer = PRINTER_HANDLE::default();
    // NULL name = local print server.
    if unsafe { OpenPrinterW(PCWSTR::null(), &mut printer, None) }.is_err() {
        return None;
    }
    let change = unsafe {
        FindFirstPrinterChangeNotification(
            printer,
            PRINTER_CHANGE_ADD_JOB,
            0,
            None,
        )
    };
    // Keep printer handle open for the lifetime of the change notification.
    // MSDN: do not ClosePrinter until FindClosePrinterChangeNotification.
    // We stash printer in thread-local via leaking pair... simpler: don't close printer here;
    // FindClose closes the change handle; we ClosePrinter after FindClose in close_change.
    // Store both: encode as change handle only and leak printer handle (small leak per run) —
    // better keep (change, printer) together.

    if change.is_invalid() {
        let _ = unsafe { ClosePrinter(printer) };
        return None;
    }
    // Leak printer handle until process exit of watcher — closed in close_change_notification
    // via a side table. Use a simple static for the open printer handle.
    WATCH_PRINTER.store(printer.Value as isize, Ordering::SeqCst);
    Some(change)
}

fn close_change_notification(change: windows::Win32::Foundation::HANDLE) {
    use windows::Win32::Graphics::Printing::{
        ClosePrinter, FindClosePrinterChangeNotification, PRINTER_HANDLE,
    };

    let _ = unsafe { FindClosePrinterChangeNotification(change) };
    let p = WATCH_PRINTER.swap(0, Ordering::SeqCst);
    if p != 0 {
        let handle = PRINTER_HANDLE {
            Value: p as *mut _,
        };
        let _ = unsafe { ClosePrinter(handle) };
    }
}

fn wait_for_change_or_timeout(
    change: Option<windows::Win32::Foundation::HANDLE>,
    timeout: Duration,
) -> bool {
    use windows::Win32::Foundation::WAIT_OBJECT_0;
    use windows::Win32::Graphics::Printing::FindNextPrinterChangeNotification;
    use windows::Win32::System::Threading::WaitForSingleObject;

    let Some(h) = change else {
        thread::sleep(timeout);
        return false;
    };
    let ms = timeout.as_millis().min(u32::MAX as u128) as u32;
    let wait = unsafe { WaitForSingleObject(h, ms) };
    if wait == WAIT_OBJECT_0 {
        let mut flags = 0u32;
        let _ = unsafe { FindNextPrinterChangeNotification(h, Some(&mut flags), None, None) };
        true
    } else {
        false
    }
}

static WATCH_PRINTER: std::sync::atomic::AtomicIsize = std::sync::atomic::AtomicIsize::new(0);

fn settings_path() -> PathBuf {
    crate::watchdog::settings_path_for_peers()
}

fn load_settings() -> Settings {
    let path = settings_path();
    let Ok(text) = fs::read_to_string(&path) else {
        return Settings::default();
    };
    let mut s = Settings::default();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let Some((k, v)) = line.split_once('=') else {
            continue;
        };
        let on = matches!(v.trim(), "1" | "true" | "yes" | "on");
        if matches!(k.trim(), "notify_print_jobs" | "notify_print" | "print_notify") {
            s.notify_print_jobs = on;
        }
    }
    s
}

fn save_settings(settings: &Settings) {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let value = if settings.notify_print_jobs { "1" } else { "0" };
    if let Err(e) = upsert_key(&path, "notify_print_jobs", value) {
        log::warn(&format!("Не удалось сохранить notify_print_jobs: {e}"));
    }
}

fn upsert_key(path: &PathBuf, key: &str, value: &str) -> Result<(), String> {
    let mut lines: Vec<String> = if path.exists() {
        fs::read_to_string(path)
            .map_err(|e| e.to_string())?
            .lines()
            .map(str::to_owned)
            .collect()
    } else {
        vec!["# SpoolCtl settings".to_owned()]
    };

    let mut found = false;
    for line in &mut lines {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        if let Some((k, _)) = trimmed.split_once('=')
            && k.trim() == key
        {
            *line = format!("{key}={value}");
            found = true;
            break;
        }
    }
    if !found {
        lines.push(format!("{key}={value}"));
    }
    fs::write(path, lines.join("\n") + "\n").map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn printer(name: &str, jobs: usize) -> PrinterEntry {
        PrinterEntry {
            name: name.to_owned(),
            jobs,
            network: false,
            ip: None,
            port_hint: None,
        }
    }

    #[test]
    fn first_sample_no_notify() {
        let mut baseline = None;
        assert!(detect_new_jobs(&mut baseline, &[printer("P", 1)]).is_none());
    }

    #[test]
    fn job_increase_notifies() {
        let mut baseline = None;
        assert!(detect_new_jobs(&mut baseline, &[printer("Panasonic KX-MB2110RU", 0)]).is_none());
        let hit = detect_new_jobs(&mut baseline, &[printer("Panasonic KX-MB2110RU", 1)]);
        assert_eq!(hit, Some(vec!["Panasonic KX-MB2110RU".to_owned()]));
    }

    #[test]
    fn message_one_printer() {
        let m = message_ru(&["Panasonic KX-MB2110RU".to_owned()]);
        assert!(m.contains("Panasonic KX-MB2110RU"));
    }
}
