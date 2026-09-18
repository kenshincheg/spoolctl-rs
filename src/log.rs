//! Append-only application log without secrets or file contents.
//! Rotates `spoolctl.log` when it exceeds [`MAX_LOG_BYTES`].

use std::fs::{self, OpenOptions};
use std::io::{BufRead, BufReader, Write};
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use windows::{
    Win32::{
        System::SystemInformation::GetLocalTime,
        UI::{Shell::ShellExecuteW, WindowsAndMessaging::SW_SHOWNORMAL},
    },
    core::PCWSTR,
};

static LOG_LOCK: Mutex<()> = Mutex::new(());

/// Rotate when the active log grows past 1 MiB.
pub const MAX_LOG_BYTES: u64 = 1_048_576;
/// Keep `spoolctl.log.1` .. `spoolctl.log.N`.
pub const ROTATED_FILES: u32 = 3;

/// Directory for log files: next to the EXE when writable, else `%LOCALAPPDATA%\SpoolCtl\logs`.
pub fn log_dir() -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let beside = parent.join("logs");
        if fs::create_dir_all(&beside).is_ok() {
            return beside;
        }
    }

    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("SpoolCtl").join("logs");
    let _ = fs::create_dir_all(&dir);
    dir
}

pub fn log_path() -> PathBuf {
    log_dir().join("spoolctl.log")
}

pub fn info(message: &str) {
    write_line("INFO", message);
}

pub fn warn(message: &str) {
    write_line("WARN", message);
}

pub fn error(message: &str) {
    write_line("ERROR", message);
}

/// Opens the log directory in Explorer.
pub fn open_log_dir() -> Result<(), String> {
    let dir = log_dir();
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
            "Не удалось открыть папку журнала (код {}).",
            result.0 as isize
        ))
    } else {
        Ok(())
    }
}

fn write_line(level: &str, message: &str) {
    let sanitized = sanitize(message);
    let line = format!("{} [{level}] {sanitized}\n", timestamp_now());
    let _guard = LOG_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let path = log_path();
    rotate_if_needed(&path, MAX_LOG_BYTES, ROTATED_FILES);
    if let Ok(mut file) = OpenOptions::new().create(true).append(true).open(&path) {
        let _ = file.write_all(line.as_bytes());
    }
}

/// `spoolctl.log` → `.1` → `.2` → `.3` (oldest dropped).
pub fn rotate_if_needed(path: &Path, max_bytes: u64, keep: u32) {
    let Ok(meta) = fs::metadata(path) else {
        return;
    };
    if meta.len() < max_bytes || keep == 0 {
        return;
    }

    let oldest = rotated_name(path, keep);
    let _ = fs::remove_file(&oldest);
    for n in (1..keep).rev() {
        let from = rotated_name(path, n);
        let to = rotated_name(path, n + 1);
        let _ = fs::rename(&from, &to);
    }
    let _ = fs::rename(path, rotated_name(path, 1));
}

fn rotated_name(path: &Path, index: u32) -> PathBuf {
    PathBuf::from(format!("{}.{}", path.display(), index))
}

/// Last `max_lines` of the log file (oldest → newest among the tail).
pub fn read_tail(max_lines: usize) -> Vec<String> {
    let path = log_path();
    let file = match fs::File::open(path) {
        Ok(file) => file,
        Err(_) => return Vec::new(),
    };
    let reader = BufReader::new(file);
    let mut lines: Vec<String> = reader.lines().map_while(Result::ok).collect();
    if lines.len() > max_lines {
        lines = lines.split_off(lines.len() - max_lines);
    }
    lines
}

fn timestamp_now() -> String {
    // SAFETY: GetLocalTime writes a fully initialized SYSTEMTIME on the stack.
    let st = unsafe { GetLocalTime() };
    format!(
        "{:04}-{:02}-{:02} {:02}:{:02}:{:02}",
        st.wYear, st.wMonth, st.wDay, st.wHour, st.wMinute, st.wSecond
    )
}

fn sanitize(message: &str) -> String {
    message
        .chars()
        .map(|ch| if ch == '\r' || ch == '\n' { ' ' } else { ch })
        .collect::<String>()
        .trim()
        .chars()
        .take(500)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn sanitize_strips_newlines_and_caps_length() {
        let long = "a\nb\r".repeat(200);
        let out = sanitize(&long);
        assert!(!out.contains('\n'));
        assert!(!out.contains('\r'));
        assert!(out.chars().count() <= 500);
    }

    #[test]
    fn log_path_is_spoolctl_log() {
        let path = log_path();
        assert_eq!(
            path.file_name().and_then(|s| s.to_str()),
            Some("spoolctl.log")
        );
    }

    #[test]
    fn rotate_if_needed_shifts_files() {
        let dir = std::env::temp_dir().join(format!("spoolctl-log-rot-{}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("dir");
        let path = dir.join("spoolctl.log");
        fs::File::create(&path)
            .and_then(|mut f| f.write_all(b"current"))
            .expect("log");
        fs::File::create(rotated_name(&path, 1))
            .and_then(|mut f| f.write_all(b"one"))
            .expect("1");

        rotate_if_needed(&path, 1, 3); // size 7 >= 1 → rotate

        assert!(!path.exists());
        assert_eq!(fs::read(rotated_name(&path, 1)).unwrap(), b"current");
        assert_eq!(fs::read(rotated_name(&path, 2)).unwrap(), b"one");
        let _ = fs::remove_dir_all(&dir);
    }
}
