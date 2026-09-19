//! Clear print queue files under `%SystemRoot%\System32\spool\PRINTERS`
//! (or `\\host\ADMIN$\…` for a remote PC).
//! Must only run while the Spooler service is stopped.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::host::Host;
use crate::spooler::{self, ServiceState, SpoolerStatus};

/// Result of deleting queue files (no file contents / secrets).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearReport {
    pub directory: PathBuf,
    pub deleted: usize,
    /// Sample of deleted file names for the UI (capped).
    pub deleted_names: Vec<String>,
    pub failed: Vec<String>,
}

const MAX_NAMED_FILES: usize = 12;

impl ClearReport {
    pub fn summary_ru(&self) -> String {
        if self.deleted == 0 && self.failed.is_empty() {
            format!("Очередь пуста ({})", self.directory.display())
        } else if self.failed.is_empty() {
            format!("Удалено файлов: {}", self.deleted)
        } else {
            format!(
                "Удалено: {}, ошибок: {} ({})",
                self.deleted,
                self.failed.len(),
                self.failed.join("; ")
            )
        }
    }

    /// Longer text for the GUI message line.
    pub fn detail_ru(&self) -> String {
        let mut text = self.summary_ru();
        if !self.deleted_names.is_empty() {
            let listed = self.deleted_names.join(", ");
            let extra = self.deleted.saturating_sub(self.deleted_names.len());
            if extra > 0 {
                text.push_str(&format!(". Файлы: {listed} … ещё {extra}"));
            } else {
                text.push_str(&format!(". Файлы: {listed}"));
            }
        }
        text
    }
}

/// `%SystemRoot%\System32\spool\PRINTERS` (local) or remote ADMIN$ path.
pub fn printers_queue_dir() -> Result<PathBuf, String> {
    Host::local().printers_queue_dir()
}

pub fn printers_queue_dir_on(host: &Host) -> Result<PathBuf, String> {
    host.printers_queue_dir()
}

/// Counts files in the PRINTERS directory (subfolders are ignored).
pub fn queue_file_count() -> Result<usize, String> {
    queue_file_count_on(&Host::local())
}

pub fn queue_file_count_on(host: &Host) -> Result<usize, String> {
    let dir = printers_queue_dir_on(host)?;
    count_files_in(&dir)
}

/// Snapshot for UI/CLI: spooler jobs (Winspool) + optional on-disk files.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QueueSnapshot {
    /// Jobs reported by the Print Spooler API (works without admin).
    pub jobs: Option<usize>,
    /// Files in `spool\PRINTERS` when the folder is readable.
    pub files: Option<usize>,
    /// Soft note when the folder is not readable (e.g. access denied).
    pub files_note: Option<&'static str>,
    /// Local and connection printers with per-printer job counts.
    pub printers: Vec<PrinterEntry>,
}

/// One installed printer and its current spooler job count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PrinterEntry {
    pub name: String,
    pub jobs: usize,
    /// True for network / TCP / WSD / share printers (not USB virtual).
    pub network: bool,
    /// IPv4 from the port name when available.
    pub ip: Option<String>,
    /// Short port hint for UI (e.g. `WSD`, `IP_…`, raw port when useful).
    pub port_hint: Option<String>,
}

impl PrinterEntry {
    pub fn kind_ru(&self) -> &'static str {
        if self.network {
            "сеть"
        } else {
            "локальный"
        }
    }

    /// Second line in the printers panel: kind · IP or WSD / port hint.
    pub fn meta_ru(&self) -> String {
        if let Some(ip) = &self.ip {
            return format!("{} · {ip}", self.kind_ru());
        }
        if let Some(hint) = &self.port_hint {
            if self.network {
                return format!("{} · {hint} (IP в порте нет)", self.kind_ru());
            }
            return format!("{} · {hint}", self.kind_ru());
        }
        self.kind_ru().to_owned()
    }

    pub fn line_ru(&self) -> String {
        format!(
            "{} — {} — {}",
            self.name,
            self.meta_ru(),
            jobs_phrase(self.jobs)
        )
    }
}

impl QueueSnapshot {
    pub fn line_ru(&self) -> String {
        match (self.jobs, self.files) {
            (Some(0), Some(0)) => "Очередь: заданий нет".to_owned(),
            (Some(0), None) => {
                if self.files_note.is_some() {
                    "Очередь: заданий нет (папка spool\\PRINTERS недоступна без прав администратора)"
                        .to_owned()
                } else {
                    "Очередь: заданий нет".to_owned()
                }
            }
            (Some(jobs), Some(files)) => {
                format!("Очередь: заданий {jobs}, файлов в spool\\PRINTERS — {files}")
            }
            (Some(jobs), None) => format!("Очередь: заданий {jobs}"),
            (None, Some(0)) => "Очередь: файлов в spool\\PRINTERS нет".to_owned(),
            (None, Some(files)) => {
                format!("Очередь: файлов в spool\\PRINTERS — {files}")
            }
            (None, None) => self
                .files_note
                .unwrap_or("Очередь: сведения недоступны")
                .to_owned(),
        }
    }
}

fn jobs_phrase(n: usize) -> String {
    let n10 = n % 10;
    let n100 = n % 100;
    if (11..=14).contains(&n100) {
        format!("{n} заданий")
    } else if n10 == 1 {
        format!("{n} задание")
    } else if (2..=4).contains(&n10) {
        format!("{n} задания")
    } else {
        format!("{n} заданий")
    }
}

/// Prefer Winspool job count; file count is secondary (needs folder ACL).
pub fn queue_snapshot() -> QueueSnapshot {
    queue_snapshot_on(&Host::local())
}

pub fn queue_snapshot_on(host: &Host) -> QueueSnapshot {
    let (jobs, printers) = match list_printers_on(host) {
        Ok(list) => {
            let total = list.iter().map(|p| p.jobs).sum();
            (Some(total), list)
        }
        Err(_) => (None, Vec::new()),
    };
    match queue_file_count_on(host) {
        Ok(files) => QueueSnapshot {
            jobs,
            files: Some(files),
            files_note: None,
            printers,
        },
        Err(error) if is_access_denied(&error) => QueueSnapshot {
            jobs,
            files: None,
            files_note: Some("папка spool\\PRINTERS недоступна без прав администратора"),
            printers,
        },
        Err(_) => QueueSnapshot {
            jobs,
            files: None,
            files_note: Some("не удалось прочитать папку очереди"),
            printers,
        },
    }
}

fn is_access_denied(error: &str) -> bool {
    let lower = error.to_ascii_lowercase();
    lower.contains("отказано в доступе")
        || lower.contains("access is denied")
        || lower.contains("os error 5")
}

fn count_files_in(dir: &Path) -> Result<usize, String> {
    if !dir.exists() {
        return Ok(0);
    }
    if !dir.is_dir() {
        return Err(format!(
            "Путь очереди не является папкой: {}",
            dir.display()
        ));
    }
    let entries = fs::read_dir(dir)
        .map_err(|err| format!("Не удалось прочитать {}: {err}", dir.display()))?;
    let mut count = 0usize;
    for entry in entries {
        let entry = match entry {
            Ok(value) => value,
            Err(_) => continue,
        };
        if entry.metadata().map(|m| m.is_file()).unwrap_or(false) {
            count += 1;
        }
    }
    Ok(count)
}

/// Local and connection printers with current job counts (Winspool).
pub fn list_printers() -> Result<Vec<PrinterEntry>, String> {
    list_printers_on(&Host::local())
}

pub fn list_printers_on(host: &Host) -> Result<Vec<PrinterEntry>, String> {
    use std::os::windows::ffi::OsStrExt;

    use windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows::Win32::Graphics::Printing::{
        ClosePrinter, EnumPrintersW, OpenPrinterW, PRINTER_ATTRIBUTE_NETWORK,
        PRINTER_ENUM_CONNECTIONS, PRINTER_ENUM_LOCAL, PRINTER_HANDLE, PRINTER_INFO_4W,
    };
    use windows::core::{HRESULT, PCWSTR};

    let flags = PRINTER_ENUM_LOCAL | PRINTER_ENUM_CONNECTIONS;
    let server_wide = host.winspool_server_wide();
    let server = server_wide
        .as_ref()
        .map(|w| PCWSTR(w.as_ptr()))
        .unwrap_or(PCWSTR::null());
    let mut needed = 0u32;
    let mut returned = 0u32;

    // SAFETY: server null = local; first call probes buffer size.
    let probe =
        unsafe { EnumPrintersW(flags, server, 4, None, &mut needed, &mut returned) };
    if let Err(error) = probe {
        if error.code() != HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0) {
            return Err(format!("EnumPrinters ({}): {error}", host.label_ru()));
        }
    } else if needed == 0 {
        return Ok(Vec::new());
    }

    let mut buffer = vec![0u8; needed as usize];
    // SAFETY: buffer sized from previous pcbNeeded.
    unsafe {
        EnumPrintersW(
            flags,
            server,
            4,
            Some(&mut buffer),
            &mut needed,
            &mut returned,
        )
        .map_err(|error| format!("EnumPrinters ({}): {error}", host.label_ru()))?;
    }

    let count = returned as usize;
    let infos = buffer.as_ptr() as *const PRINTER_INFO_4W;
    let mut printers = Vec::with_capacity(count);

    for index in 0..count {
        // SAFETY: `returned` entries were written by EnumPrintersW.
        let info = unsafe { &*infos.add(index) };
        let name = unsafe {
            if info.pPrinterName.is_null() {
                continue;
            }
            match info.pPrinterName.to_string() {
                Ok(value) if !value.is_empty() => value,
                _ => continue,
            }
        };
        let attr_network = (info.Attributes & PRINTER_ATTRIBUTE_NETWORK) != 0;

        let open_name = host.printer_open_name(&name);
        let wide: Vec<u16> = std::ffi::OsStr::new(&open_name)
            .encode_wide()
            .chain(std::iter::once(0))
            .collect();
        let mut handle = PRINTER_HANDLE::default();
        // SAFETY: NUL-terminated printer name.
        let (jobs, port) =
            if unsafe { OpenPrinterW(PCWSTR(wide.as_ptr()), &mut handle, None) }.is_ok() {
                let jobs = count_jobs_for_printer(handle).unwrap_or(0);
                let port = printer_port_name(handle);
                let _ = unsafe { ClosePrinter(handle) };
                (jobs, port)
            } else {
                (0, None)
            };

        let (network, ip, port_hint) = classify_printer(&name, port.as_deref(), attr_network);
        printers.push(PrinterEntry {
            name,
            jobs,
            network,
            ip,
            port_hint,
        });
    }

    printers.sort_by_key(|a| a.name.to_lowercase());
    Ok(printers)
}

fn printer_port_name(handle: windows::Win32::Graphics::Printing::PRINTER_HANDLE) -> Option<String> {
    use windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows::Win32::Graphics::Printing::{GetPrinterW, PRINTER_INFO_5W};
    use windows::core::HRESULT;

    let mut needed = 0u32;
    // SAFETY: empty buffer probes size for level-5 info.
    let probe = unsafe { GetPrinterW(handle, 5, None, &mut needed) };
    match probe {
        Ok(()) => {}
        Err(error) if error.code() == HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0) => {}
        Err(_) => return None,
    }
    if needed == 0 {
        return None;
    }
    let mut buffer = vec![0u8; needed as usize];
    // SAFETY: buffer sized from pcbNeeded.
    if unsafe { GetPrinterW(handle, 5, Some(&mut buffer), &mut needed) }.is_err() {
        return None;
    }
    // SAFETY: level-5 layout written by GetPrinterW.
    let info = unsafe { &*(buffer.as_ptr() as *const PRINTER_INFO_5W) };
    unsafe {
        if info.pPortName.is_null() {
            None
        } else {
            info.pPortName.to_string().ok().filter(|s| !s.is_empty())
        }
    }
}

/// Decide local vs network from port name (more reliable than PRINTER_ATTRIBUTE_NETWORK alone).
/// Returns `(network, ip, port_hint)`.
fn classify_printer(
    name: &str,
    port: Option<&str>,
    attr_network: bool,
) -> (bool, Option<String>, Option<String>) {
    let port = port.unwrap_or("");
    let ip = extract_ipv4(port).or_else(|| extract_ipv4(name));
    let upper = port.to_ascii_uppercase();
    let port_hint = port_hint_from(port);

    let local_port = upper.starts_with("USB")
        || upper.starts_with("DOT4")
        || upper.starts_with("LPT")
        || upper.starts_with("COM")
        || upper.starts_with("FILE")
        || upper.starts_with("PORTPROMPT")
        || upper.starts_with("NUL")
        || upper.starts_with("SHRFAX")
        || upper.starts_with("XPSSPOOL")
        || upper.contains("DOCUMENT WRITER")
        || upper.contains("PDF")
        || upper.contains("ONENOTE")
        || upper.starts_with("TS") // Remote Desktop redirected
        || upper.is_empty() && looks_like_virtual_printer(name);

    if local_port && ip.is_none() && !upper.starts_with("\\\\") {
        return (false, None, port_hint);
    }

    let network = attr_network
        || ip.is_some()
        || upper.starts_with("IP_")
        || upper.starts_with("WSD")
        || upper.contains("IPP")
        || upper.starts_with("\\\\")
        || upper.contains("TCP")
        || upper.starts_with("HTTP")
        || upper.starts_with("SMB")
        || upper.contains("NETWORK");

    if network {
        (true, ip, port_hint)
    } else if looks_like_virtual_printer(name) {
        (false, None, port_hint)
    } else {
        (false, ip, port_hint)
    }
}

fn port_hint_from(port: &str) -> Option<String> {
    if port.is_empty() {
        return None;
    }
    let upper = port.to_ascii_uppercase();
    if upper.starts_with("WSD") {
        return Some("WSD".to_owned());
    }
    if upper.contains("IPP") {
        return Some("IPP".to_owned());
    }
    if port
        .strip_prefix("IP_")
        .or_else(|| port.strip_prefix("ip_"))
        .is_some()
        && extract_ipv4(port).is_some()
    {
        return Some("TCP/IP".to_owned());
    }
    if upper.starts_with("USB") {
        return Some("USB".to_owned());
    }
    if port.chars().count() <= 24 {
        Some(port.to_owned())
    } else {
        let truncated: String = port.chars().take(20).collect();
        Some(format!("{truncated}…"))
    }
}

fn looks_like_virtual_printer(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.contains("rustdesk")
        || lower.contains("anydesk")
        || lower.contains("onenote")
        || lower.contains("pdf")
        || lower.contains("xps")
        || lower.contains("fax")
        || lower.contains("microsoft print to")
}

fn extract_ipv4(text: &str) -> Option<String> {
    for token in text.split(|c: char| !(c.is_ascii_digit() || c == '.')) {
        if is_valid_ipv4(token) {
            return Some(token.to_owned());
        }
    }
    None
}

fn is_valid_ipv4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('.').collect();
    if parts.len() != 4 {
        return false;
    }
    parts
        .iter()
        .all(|part| !part.is_empty() && part.len() <= 3 && part.parse::<u8>().is_ok())
}

fn count_jobs_for_printer(
    handle: windows::Win32::Graphics::Printing::PRINTER_HANDLE,
) -> Result<usize, String> {
    use windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows::Win32::Graphics::Printing::EnumJobsW;
    use windows::core::HRESULT;

    let mut needed = 0u32;
    let mut returned = 0u32;
    // SAFETY: empty buffer probes size for level-1 job info.
    let probe = unsafe { EnumJobsW(handle, 0, u32::MAX, 1, None, &mut needed, &mut returned) };
    match probe {
        Ok(()) => return Ok(returned as usize),
        Err(error) if error.code() == HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0) => {}
        Err(error) => return Err(format!("EnumJobs: {error}")),
    }

    if needed == 0 {
        return Ok(0);
    }
    let mut buffer = vec![0u8; needed as usize];
    // SAFETY: buffer sized from pcbNeeded.
    unsafe {
        EnumJobsW(
            handle,
            0,
            u32::MAX,
            1,
            Some(&mut buffer),
            &mut needed,
            &mut returned,
        )
        .map_err(|error| format!("EnumJobs: {error}"))?;
    }
    Ok(returned as usize)
}

/// Opens the PRINTERS queue directory in Explorer.
pub fn open_queue_dir() -> Result<(), String> {
    use std::os::windows::ffi::OsStrExt;
    use windows::{
        Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
        core::PCWSTR,
    };

    let dir = printers_queue_dir()?;
    if !dir.exists() {
        return Err(format!("Папка очереди не найдена: {}", dir.display()));
    }
    let dir_wide: Vec<u16> = dir
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect();
    let verb: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: NUL-terminated wide strings; ShellExecuteW does not take ownership.
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(dir_wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        Err(format!(
            "Не удалось открыть папку очереди (код {}).",
            result.0 as isize
        ))
    } else {
        Ok(())
    }
}

/// Opens the Windows Printers folder (`shell:PrintersFolder`).
pub fn open_printers_folder() -> Result<(), String> {
    use windows::{
        Win32::UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
        core::PCWSTR,
    };

    let target: Vec<u16> = "shell:PrintersFolder"
        .encode_utf16()
        .chain(std::iter::once(0))
        .collect();
    let verb: Vec<u16> = "open".encode_utf16().chain(std::iter::once(0)).collect();
    // SAFETY: NUL-terminated wide strings; ShellExecuteW does not take ownership.
    let result = unsafe {
        ShellExecuteW(
            None,
            PCWSTR(verb.as_ptr()),
            PCWSTR(target.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        )
    };
    if result.0 as isize <= 32 {
        Err(format!(
            "Не удалось открыть папку принтеров (код {}).",
            result.0 as isize
        ))
    } else {
        Ok(())
    }
}

/// Deletes files in the PRINTERS directory. Caller must ensure Spooler is stopped.
pub fn clear_queue_files(dir: &Path) -> Result<ClearReport, String> {
    if !dir.exists() {
        return Ok(ClearReport {
            directory: dir.to_path_buf(),
            deleted: 0,
            deleted_names: Vec::new(),
            failed: Vec::new(),
        });
    }
    if !dir.is_dir() {
        return Err(format!(
            "Путь очереди не является папкой: {}",
            dir.display()
        ));
    }

    let entries = fs::read_dir(dir)
        .map_err(|err| format!("Не удалось прочитать {}: {err}", dir.display()))?;

    let mut deleted = 0usize;
    let mut deleted_names = Vec::new();
    let mut failed = Vec::new();

    for entry in entries {
        let entry = match entry {
            Ok(value) => value,
            Err(err) => {
                failed.push(format!("чтение записи: {err}"));
                continue;
            }
        };
        let path = entry.path();
        let meta = match entry.metadata() {
            Ok(value) => value,
            Err(err) => {
                failed.push(format!("{}: {err}", file_label(&path)));
                continue;
            }
        };
        if meta.is_dir() {
            // Do not recurse into unexpected subfolders.
            continue;
        }
        let name = file_label(&path);
        match fs::remove_file(&path) {
            Ok(()) => {
                deleted += 1;
                if deleted_names.len() < MAX_NAMED_FILES {
                    deleted_names.push(name);
                }
            }
            Err(err) => failed.push(format!("{name}: {err}")),
        }
    }

    Ok(ClearReport {
        directory: dir.to_path_buf(),
        deleted,
        deleted_names,
        failed,
    })
}

/// Stops Spooler if needed, clears queue files, leaves Spooler stopped.
pub fn clear_queue(timeout: Duration) -> Result<(ClearReport, SpoolerStatus), String> {
    clear_queue_on(&Host::local(), timeout)
}

pub fn clear_queue_on(
    host: &Host,
    timeout: Duration,
) -> Result<(ClearReport, SpoolerStatus), String> {
    let status = spooler::stop_spooler_on(host, timeout)?;
    if status.state != ServiceState::Stopped {
        return Err(format!(
            "Spooler не остановлен (сейчас: «{}», {}). Очистка отменена.",
            status.state.as_ru_str(),
            host.label_ru()
        ));
    }
    let dir = printers_queue_dir_on(host)?;
    let report = clear_queue_files(&dir)?;
    let status = spooler::query_spooler_status_on(host)?;
    Ok((report, status))
}

/// Stops Spooler, clears queue files, starts Spooler again.
pub fn fix_queue(timeout: Duration) -> Result<(ClearReport, SpoolerStatus), String> {
    fix_queue_on(&Host::local(), timeout)
}

pub fn fix_queue_on(
    host: &Host,
    timeout: Duration,
) -> Result<(ClearReport, SpoolerStatus), String> {
    let (report, _) = clear_queue_on(host, timeout)?;
    let status = spooler::start_spooler_on(host, timeout)?;
    Ok((report, status))
}

fn file_label(path: &Path) -> String {
    path.file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_else(|| path.display().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn clear_queue_files_deletes_only_files() {
        let dir = std::env::temp_dir().join(format!("spoolctl-queue-test-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        let nested = dir.join("nested");
        fs::create_dir_all(&nested).expect("nested");

        let file_a = dir.join("00001.SPL");
        let file_b = dir.join("00001.SHD");
        fs::File::create(&file_a)
            .and_then(|mut f| f.write_all(b"a"))
            .expect("spl");
        fs::File::create(&file_b)
            .and_then(|mut f| f.write_all(b"b"))
            .expect("shd");
        fs::File::create(nested.join("keep.txt"))
            .and_then(|mut f| f.write_all(b"c"))
            .expect("nested file");

        let report = clear_queue_files(&dir).expect("clear");
        assert_eq!(report.deleted, 2);
        assert!(report.failed.is_empty());
        assert_eq!(report.deleted_names.len(), 2);
        assert!(!file_a.exists());
        assert!(!file_b.exists());
        assert!(nested.join("keep.txt").exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn printers_queue_dir_ends_with_printers() {
        let path = printers_queue_dir().expect("SystemRoot");
        assert!(
            path.ends_with(Path::new("System32\\spool\\PRINTERS"))
                || path.ends_with(Path::new("System32/spool/PRINTERS"))
        );
    }

    #[test]
    fn queue_file_count_counts_files_only() {
        let dir = std::env::temp_dir().join(format!("spoolctl-queue-count-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("temp dir");
        fs::create_dir_all(dir.join("nested")).expect("nested");
        fs::File::create(dir.join("a.SPL"))
            .and_then(|mut f| f.write_all(b"a"))
            .expect("spl");
        fs::File::create(dir.join("a.SHD"))
            .and_then(|mut f| f.write_all(b"b"))
            .expect("shd");

        assert_eq!(count_files_in(&dir).expect("count"), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn snapshot_line_for_empty_jobs() {
        let line = QueueSnapshot {
            jobs: Some(0),
            files: None,
            files_note: Some("папка spool\\PRINTERS недоступна без прав администратора"),
            printers: Vec::new(),
        }
        .line_ru();
        assert!(line.contains("заданий нет"));
        assert!(line.contains("администратора"));
    }

    #[test]
    fn jobs_phrase_russian_plural() {
        assert_eq!(jobs_phrase(0), "0 заданий");
        assert_eq!(jobs_phrase(1), "1 задание");
        assert_eq!(jobs_phrase(2), "2 задания");
        assert_eq!(jobs_phrase(5), "5 заданий");
        assert_eq!(jobs_phrase(21), "21 задание");
    }

    #[test]
    fn printer_entry_line_includes_kind() {
        let line = PrinterEntry {
            name: "HP".to_owned(),
            jobs: 1,
            network: false,
            ip: None,
            port_hint: None,
        }
        .line_ru();
        assert!(line.contains("локальный"));
        assert!(line.contains("1 задание"));
    }

    #[test]
    fn classify_tcp_port_as_network_with_ip() {
        let (network, ip, hint) = classify_printer("HP Laser", Some("IP_192.168.1.50"), false);
        assert!(network);
        assert_eq!(ip.as_deref(), Some("192.168.1.50"));
        assert_eq!(hint.as_deref(), Some("TCP/IP"));
    }

    #[test]
    fn classify_wsd_as_network_without_ip() {
        let (network, ip, hint) = classify_printer("HP Laser", Some("WSD-cf123456-abcd-…"), false);
        assert!(network);
        assert!(ip.is_none());
        assert_eq!(hint.as_deref(), Some("WSD"));
        let meta = PrinterEntry {
            name: "HP".into(),
            jobs: 0,
            network,
            ip,
            port_hint: hint,
        }
        .meta_ru();
        assert!(meta.contains("WSD"));
        assert!(meta.contains("IP в порте нет"));
    }

    #[test]
    fn classify_usb_as_local() {
        let (network, ip, _) = classify_printer("DeskJet", Some("USB001"), false);
        assert!(!network);
        assert!(ip.is_none());
    }

    #[test]
    fn classify_virtual_as_local() {
        let (network, _, _) = classify_printer("RustDesk Printer", Some("PORTPROMPT"), false);
        assert!(!network);
        let (network, _, _) = classify_printer("AnyDesk Printer", Some(""), false);
        assert!(!network);
    }

    #[test]
    fn access_denied_detects_os_error_5() {
        assert!(is_access_denied(
            "Не удалось прочитать C:\\Windows\\System32\\spool\\PRINTERS: Отказано в доступе. (os error 5)"
        ));
    }
}
