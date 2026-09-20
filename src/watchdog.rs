//! Hang watchdog for the Print Spooler / stuck print queue.
//! Auto-restart runs only when the user explicitly enables it.

use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use crate::queue::PrinterEntry;
use crate::spooler::ServiceState;

/// Jobs fingerprint unchanged this long → possible hang.
pub const QUEUE_STUCK_AFTER: Duration = Duration::from_secs(90);
/// Spooler stuck in *Pending this long → possible hang.
pub const PENDING_STUCK_AFTER: Duration = Duration::from_secs(60);
/// Minimum pause between automatic restarts.
pub const AUTO_RESTART_COOLDOWN: Duration = Duration::from_secs(180);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Settings {
    /// Observe queue / Spooler and warn on hang.
    pub watch_enabled: bool,
    /// Restart Spooler automatically when a hang is detected (needs admin).
    pub auto_restart: bool,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            watch_enabled: false,
            auto_restart: false,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HangKind {
    StuckQueue { jobs: usize },
    PendingTooLong { state: ServiceState },
}

impl HangKind {
    pub fn summary_ru(self) -> String {
        match self {
            Self::StuckQueue { jobs } => format!(
                "Похоже на зависание очереди: {jobs} заданий без прогресса дольше {} с.",
                QUEUE_STUCK_AFTER.as_secs()
            ),
            Self::PendingTooLong { state } => format!(
                "Служба печати слишком долго в состоянии «{}» ({} с).",
                state.as_ru_str(),
                PENDING_STUCK_AFTER.as_secs()
            ),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Show a one-shot warning (watch on, auto-restart off).
    Warn,
    /// Start an automatic Spooler restart (watch + auto-restart).
    AutoRestart,
}

#[derive(Debug)]
pub struct Watchdog {
    pub settings: Settings,
    queue_fp: Option<String>,
    queue_unchanged_since: Option<Instant>,
    pending_since: Option<Instant>,
    last_auto_restart: Option<Instant>,
    /// Avoid repeating the same warn for one hang episode.
    warned_fp: Option<String>,
}

impl Watchdog {
    pub fn load() -> Self {
        Self {
            settings: load_settings(),
            queue_fp: None,
            queue_unchanged_since: None,
            pending_since: None,
            last_auto_restart: None,
            warned_fp: None,
        }
    }

    pub fn save_settings(&self) {
        save_settings(&self.settings);
    }

    pub fn reset_tracking(&mut self) {
        self.queue_fp = None;
        self.queue_unchanged_since = None;
        self.pending_since = None;
        self.warned_fp = None;
    }

    /// Call after a successful (manual or auto) restart so we do not loop.
    pub fn note_restart_done(&mut self) {
        self.last_auto_restart = Some(Instant::now());
        self.reset_tracking();
    }

    /// Update observation from the latest status poll. Returns an action if any.
    pub fn observe(
        &mut self,
        state: ServiceState,
        printers: &[PrinterEntry],
    ) -> Option<(HangKind, Action)> {
        if !self.settings.watch_enabled {
            self.reset_tracking();
            return None;
        }

        let now = Instant::now();
        let hang = self.detect_hang(now, state, printers)?;
        let episode_key = hang_key(&hang, printers);

        if self.settings.auto_restart {
            if let Some(last) = self.last_auto_restart
                && now.duration_since(last) < AUTO_RESTART_COOLDOWN
            {
                return None;
            }
            Some((hang, Action::AutoRestart))
        } else {
            if self.warned_fp.as_deref() == Some(episode_key.as_str()) {
                return None;
            }
            self.warned_fp = Some(episode_key);
            Some((hang, Action::Warn))
        }
    }

    fn detect_hang(
        &mut self,
        now: Instant,
        state: ServiceState,
        printers: &[PrinterEntry],
    ) -> Option<HangKind> {
        if is_pending(state) {
            match self.pending_since {
                Some(since) if now.duration_since(since) >= PENDING_STUCK_AFTER => {
                    return Some(HangKind::PendingTooLong { state });
                }
                Some(_) => {}
                None => self.pending_since = Some(now),
            }
        } else {
            self.pending_since = None;
        }

        let total_jobs: usize = printers.iter().map(|p| p.jobs).sum();
        let fp = fingerprint(printers);

        if total_jobs == 0 {
            self.queue_fp = Some(fp);
            self.queue_unchanged_since = None;
            self.warned_fp = None;
            return None;
        }

        match &self.queue_fp {
            Some(prev) if prev == &fp => {
                let since = *self.queue_unchanged_since.get_or_insert(now);
                if now.duration_since(since) >= QUEUE_STUCK_AFTER {
                    return Some(HangKind::StuckQueue { jobs: total_jobs });
                }
            }
            _ => {
                self.queue_fp = Some(fp);
                self.queue_unchanged_since = Some(now);
                self.warned_fp = None;
            }
        }
        None
    }
}

fn is_pending(state: ServiceState) -> bool {
    matches!(
        state,
        ServiceState::StartPending
            | ServiceState::StopPending
            | ServiceState::ContinuePending
            | ServiceState::PausePending
    )
}

fn fingerprint(printers: &[PrinterEntry]) -> String {
    let mut parts: Vec<String> = printers
        .iter()
        .map(|p| format!("{}:{}", p.name, p.jobs))
        .collect();
    parts.sort();
    parts.join("|")
}

fn hang_key(hang: &HangKind, printers: &[PrinterEntry]) -> String {
    match hang {
        HangKind::StuckQueue { .. } => format!("q:{}", fingerprint(printers)),
        HangKind::PendingTooLong { state } => format!("p:{}", state.as_ru_str()),
    }
}

fn settings_path() -> PathBuf {
    settings_path_for_peers()
}

/// Shared path for `spoolctl-settings.txt` (watchdog + print_notify).
pub fn settings_path_for_peers() -> PathBuf {
    if let Ok(exe) = std::env::current_exe()
        && let Some(parent) = exe.parent()
    {
        let beside = parent.join("spoolctl-settings.txt");
        // Prefer next to the EXE when the folder is writable (portable mode).
        if fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&beside)
            .is_ok()
        {
            return beside;
        }
    }
    let base = std::env::var_os("LOCALAPPDATA")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    let dir = base.join("SpoolCtl");
    let _ = fs::create_dir_all(&dir);
    dir.join("spoolctl-settings.txt")
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
        match k.trim() {
            "watch_enabled" | "watch" => s.watch_enabled = on,
            "auto_restart" => s.auto_restart = on,
            _ => {}
        }
    }
    if s.auto_restart {
        s.watch_enabled = true;
    }
    s
}

fn save_settings(settings: &Settings) {
    let path = settings_path();
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    // Preserve keys owned by other modules (e.g. notify_print_jobs).
    let existing = fs::read_to_string(&path).unwrap_or_default();
    let mut extras: Vec<String> = Vec::new();
    for line in existing.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') {
            continue;
        }
        let Some((k, _)) = trimmed.split_once('=') else {
            continue;
        };
        match k.trim() {
            "watch_enabled" | "watch" | "auto_restart" => {}
            _ => extras.push(trimmed.to_owned()),
        }
    }
    let mut body = format!(
        "# SpoolCtl settings\nwatch_enabled={}\nauto_restart={}\n",
        if settings.watch_enabled { "1" } else { "0" },
        if settings.auto_restart { "1" } else { "0" },
    );
    for e in extras {
        body.push_str(&e);
        body.push('\n');
    }
    let _ = fs::write(path, body);
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
    fn no_watch_no_action() {
        let mut w = Watchdog::load();
        w.settings = Settings::default();
        w.reset_tracking();
        let printers = vec![printer("A", 2)];
        assert!(w.observe(ServiceState::Running, &printers).is_none());
    }

    #[test]
    fn stuck_queue_warns_once() {
        let mut w = Watchdog {
            settings: Settings {
                watch_enabled: true,
                auto_restart: false,
            },
            queue_fp: None,
            queue_unchanged_since: None,
            pending_since: None,
            last_auto_restart: None,
            warned_fp: None,
        };
        let printers = vec![printer("A", 1)];
        assert!(w.observe(ServiceState::Running, &printers).is_none());
        w.queue_unchanged_since = Some(Instant::now() - QUEUE_STUCK_AFTER - Duration::from_secs(1));
        let first = w.observe(ServiceState::Running, &printers);
        assert!(matches!(
            first,
            Some((HangKind::StuckQueue { jobs: 1 }, Action::Warn))
        ));
        assert!(w.observe(ServiceState::Running, &printers).is_none());
    }

    #[test]
    fn auto_restart_action() {
        let mut w = Watchdog {
            settings: Settings {
                watch_enabled: true,
                auto_restart: true,
            },
            queue_fp: Some("A:1".into()),
            queue_unchanged_since: Some(Instant::now() - QUEUE_STUCK_AFTER - Duration::from_secs(1)),
            pending_since: None,
            last_auto_restart: None,
            warned_fp: None,
        };
        let printers = vec![printer("A", 1)];
        assert!(matches!(
            w.observe(ServiceState::Running, &printers),
            Some((_, Action::AutoRestart))
        ));
    }

    #[test]
    fn fingerprint_stable() {
        let a = vec![printer("B", 1), printer("A", 2)];
        let b = vec![printer("A", 2), printer("B", 1)];
        assert_eq!(fingerprint(&a), fingerprint(&b));
    }
}
