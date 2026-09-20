//! Simple egui window for Spooler status and control (Win10/11).

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, FontId, RichText, TextStyle, Theme, ThemePreference, Visuals};

use crate::autostart;
use crate::elevate;
use crate::host::Host;
use crate::log;
use crate::ncd;
use crate::print_notify::{self, PrintNotify};
use crate::queue::{self, ClearReport, PrinterEntry, QueueSnapshot};
use crate::single_instance;
use crate::spooler::{self, SpoolerStatus};
use crate::tray::{self, AppTray, TrayCmd};
use crate::watchdog::{self, Watchdog};

const APP_VERSION: &str = env!("CARGO_PKG_VERSION");
const AUTO_STATUS_REFRESH: Duration = Duration::from_secs(5);

/// Soft blue palette for light and dark system themes.
#[derive(Clone, Copy)]
struct Palette {
    bg: Color32,
    panel: Color32,
    text: Color32,
    accent: Color32,
    accent_hover: Color32,
    accent_active: Color32,
    on_accent: Color32,
    soft_btn: Color32,
    /// Footer utility buttons (green accent).
    util_btn: Color32,
    on_util: Color32,
    error: Color32,
    ok: Color32,
    tip_bg: Color32,
    tip_text: Color32,
}

impl Palette {
    fn for_dark(dark: bool) -> Self {
        if dark {
            // Soft night blues — readable tips on dark Windows theme.
            Self {
                bg: Color32::from_rgb(28, 40, 56),
                panel: Color32::from_rgb(36, 52, 72),
                text: Color32::from_rgb(220, 234, 248),
                accent: Color32::from_rgb(110, 168, 220),
                accent_hover: Color32::from_rgb(130, 185, 230),
                accent_active: Color32::from_rgb(80, 140, 195),
                on_accent: Color32::from_rgb(16, 28, 42),
                soft_btn: Color32::from_rgb(52, 78, 108),
                util_btn: Color32::from_rgb(46, 125, 90),
                on_util: Color32::from_rgb(230, 255, 240),
                error: Color32::from_rgb(255, 150, 150),
                ok: Color32::from_rgb(140, 220, 170),
                tip_bg: Color32::from_rgb(20, 32, 48),
                tip_text: Color32::from_rgb(235, 244, 255),
            }
        } else {
            // Soft daylight blues — lighter main canvas.
            Self {
                bg: Color32::from_rgb(245, 250, 255),
                panel: Color32::from_rgb(236, 245, 255),
                text: Color32::from_rgb(18, 52, 86),
                accent: Color32::from_rgb(37, 99, 160),
                accent_hover: Color32::from_rgb(56, 130, 200),
                accent_active: Color32::from_rgb(25, 80, 140),
                on_accent: Color32::from_rgb(245, 250, 255),
                soft_btn: Color32::from_rgb(210, 230, 248),
                util_btn: Color32::from_rgb(56, 142, 100),
                on_util: Color32::from_rgb(245, 255, 248),
                error: Color32::from_rgb(170, 35, 45),
                ok: Color32::from_rgb(20, 110, 60),
                tip_bg: Color32::from_rgb(252, 254, 255),
                tip_text: Color32::from_rgb(18, 52, 86),
            }
        }
    }
}

#[derive(Clone, Copy)]
enum Job {
    Refresh,
    Stop,
    Start,
    Restart,
    ClearOnly,
    FixClear,
}

#[derive(Clone, Copy)]
enum Confirm {
    Job(Job),
    DisableNcd,
    /// Explicit consent before enabling auto-restart on hang.
    EnableAutoRestart,
    /// Explicit consent before enabling Windows login autostart.
    EnableAutostart,
    /// Close button (X): ask tray vs exit.
    CloseOrTray,
}

enum JobResult {
    Done {
        label: &'static str,
        status: SpoolerStatus,
        report: Option<ClearReport>,
        /// Fetched on worker thread — never touch remote SCM/ADMIN$ on the UI thread.
        snap: QueueSnapshot,
    },
    Failed {
        label: &'static str,
        error: String,
    },
}

pub struct SpoolCtlApp {
    status_line: String,
    queue_line: String,
    printers: Vec<PrinterEntry>,
    message: String,
    message_is_error: bool,
    busy: bool,
    elevated: bool,
    /// Highlight «Запустить службу» after clear-only / when Spooler is stopped.
    prompt_start: bool,
    confirm: Option<Confirm>,
    result_rx: Option<Receiver<JobResult>>,
    log_lines: Vec<String>,
    last_auto_status: Instant,
    ncd_summary: String,
    watchdog: Watchdog,
    /// Balloon when a new print job appears (local, separate from hang watchdog).
    print_notify: PrintNotify,
    /// Last known Spooler state for the hang watchdog.
    last_service_state: Option<spooler::ServiceState>,
    tray: Option<AppTray>,
    /// Real exit (from tray «Выход»); otherwise close hides to tray.
    allow_exit: bool,
    window_in_tray: bool,
    /// Hide to tray once after startup (`--tray` / Windows autostart).
    start_in_tray: bool,
    /// After UAC (`--show`): force the window visible on first frame.
    force_show_window: bool,
    /// Mirrors HKCU Run key for SpoolCtl.
    autostart_enabled: bool,
    /// Text field for remote PC (empty = this computer).
    host_input: String,
    /// Parsed target for jobs / status.
    host: Host,
    /// Typewriter tooltip on «От админа»: when hover started.
    admin_tip_started: Option<Instant>,
}

impl SpoolCtlApp {
    fn new(cc: &eframe::CreationContext<'_>, start_in_tray: bool, force_show_window: bool) -> Self {
        apply_theme(&cc.egui_ctx);
        let elevated = elevate::is_elevated();
        log::info(&format!(
            "GUI готов (права: {})",
            if elevated {
                "администратор"
            } else {
                "обычный пользователь"
            }
        ));
        let mut app = Self {
            status_line: "статус службы печати ещё не запрошен".to_owned(),
            queue_line: "очередь: ещё не проверена".to_owned(),
            printers: Vec::new(),
            message: "Нажмите «Обновить» или выполните действие.".to_owned(),
            message_is_error: false,
            busy: false,
            elevated,
            prompt_start: false,
            confirm: None,
            result_rx: None,
            log_lines: Vec::new(),
            last_auto_status: Instant::now(),
            ncd_summary: ncd::query_status().summary_ru(),
            watchdog: Watchdog::load(),
            print_notify: PrintNotify::load(),
            last_service_state: None,
            tray: None,
            allow_exit: false,
            window_in_tray: false,
            start_in_tray,
            force_show_window: force_show_window && !start_in_tray,
            autostart_enabled: autostart::is_enabled(),
            host_input: String::new(),
            host: Host::local(),
            admin_tip_started: None,
        };
        tray::remember_main_hwnd_from_cc(cc);
        match AppTray::install(APP_VERSION) {
            Ok(tray) => {
                log::info("Иконка в трее: окно можно закрыть — сторож продолжит работу");
                app.tray = Some(tray);
            }
            Err(error) => {
                log::warn(&format!("Трей недоступен: {error}"));
                app.message = format!("Трей недоступен: {error}");
                app.message_is_error = true;
            }
        }
        if app.watchdog.settings.watch_enabled {
            log::info("Сторож зависания: включён");
        }
        if app.watchdog.settings.auto_restart {
            log::info("Сторож зависания: автоперезапуск включён");
        }
        if app.print_notify.settings.notify_print_jobs {
            log::info("Уведомления о печати: включены");
        }
        app.refresh_log_view();
        app.apply_status_query_local();
        app
    }

    fn refresh_ncd_summary(&mut self) {
        self.ncd_summary = ncd::query_status().summary_ru();
    }

    fn refresh_log_view(&mut self) {
        self.log_lines = log::read_tail(12);
    }

    fn refresh_queue_line(&mut self) {
        // Local only — remote queue enumeration can hang the UI for minutes.
        if !self.host.is_local() {
            return;
        }
        let snap = queue::queue_snapshot_on(&self.host);
        self.apply_queue_snap(snap);
    }

    fn apply_queue_snap(&mut self, snap: QueueSnapshot) {
        self.queue_line = snap.line_ru();
        self.printers = snap.printers;
    }

    fn apply_status_fields(&mut self, status: &SpoolerStatus) {
        let where_ = self.host.label_ru();
        self.status_line = format!("{} — {where_}", format_status(status));
        self.prompt_start = status.state == spooler::ServiceState::Stopped;
        self.last_service_state = Some(status.state);
    }

    /// Quiet status poll for the idle timer (does not touch the message line).
    fn quiet_status_refresh(&mut self) {
        self.last_auto_status = Instant::now();
        // Remote SCM/RPC timeouts block for a long time — never poll remote on the UI thread.
        if !self.host.is_local() {
            return;
        }
        if let Ok(status) = spooler::query_spooler_status_on(&self.host) {
            self.apply_status_fields(&status);
            self.refresh_queue_line();
        } else {
            self.refresh_queue_line();
        }
        self.run_watchdog_tick();
        self.run_print_notify_tick();
    }

    /// Parse the host field into `self.host`. Returns whether the target changed.
    fn sync_host_from_input(&mut self) -> Result<bool, String> {
        let host = Host::parse(&self.host_input)?;
        let changed = host != self.host;
        self.host = host;
        Ok(changed)
    }

    fn apply_host_input(&mut self) {
        match self.sync_host_from_input() {
            Ok(changed) => {
                if changed {
                    log::info(&format!("Цель: {}", self.host.label_ru()));
                }
                // Always refresh on Apply — remote work must be async.
                if self.host.is_local() {
                    self.apply_status_query_local();
                    self.refresh_log_view();
                } else if !self.busy {
                    self.start_job(Job::Refresh);
                }
            }
            Err(error) => {
                self.message = error;
                self.message_is_error = true;
            }
        }
    }

    fn run_watchdog_tick(&mut self) {
        if self.busy || self.confirm.is_some() {
            return;
        }
        let Some(state) = self.last_service_state else {
            return;
        };
        let printers = self.printers.clone();
        let Some((hang, action)) = self.watchdog.observe(state, &printers) else {
            return;
        };
        let summary = hang.summary_ru();
        match action {
            watchdog::Action::Warn => {
                log::warn(&summary);
                self.message = format!(
                    "{summary} Включите «Автоперезапуск» или нажмите «Перезапустить»."
                );
                self.message_is_error = true;
                if let Some(tray) = &self.tray {
                    tray.set_tooltip(&format!("SpoolCtl: {summary}"));
                }
                if self.window_in_tray {
                    tray::show_main_window();
                    self.window_in_tray = false;
                }
                self.refresh_log_view();
            }
            watchdog::Action::AutoRestart => {
                if !self.elevated {
                    log::warn(&format!("{summary} Автоперезапуск пропущен: нет прав администратора."));
                    self.message = format!(
                        "{summary} Автоперезапуск нужен от имени администратора."
                    );
                    self.message_is_error = true;
                    if let Some(tray) = &self.tray {
                        tray.set_tooltip(&format!("SpoolCtl: нужен администратор — {summary}"));
                    }
                    if self.window_in_tray {
                        tray::show_main_window();
                        self.window_in_tray = false;
                    }
                    self.refresh_log_view();
                    return;
                }
                log::warn(&format!("{summary} Автоперезапуск Spooler…"));
                self.message = format!("{summary} Выполняется автоперезапуск…");
                self.message_is_error = false;
                if let Some(tray) = &self.tray {
                    tray.set_tooltip("SpoolCtl: автоперезапуск Spooler…");
                }
                self.watchdog.note_restart_done();
                self.refresh_log_view();
                self.start_job(Job::Restart);
            }
        }
    }

    fn run_print_notify_tick(&mut self) {
        let newly = self.print_notify.poll_events();
        if newly.is_empty() {
            return;
        }
        let text = print_notify::message_ru(&newly);
        // Balloon already shown by the watcher; refresh UI/log here.
        if let Some(tray) = &self.tray {
            tray.set_tooltip(&format!("SpoolCtl: {text}"));
        }
        self.message = text;
        self.message_is_error = false;
        self.refresh_log_view();
    }

    fn apply_status_query_local(&mut self) {
        match spooler::query_spooler_status_on(&self.host) {
            Ok(status) => {
                self.apply_status_fields(&status);
                self.refresh_queue_line();
                self.message = format!("Статус обновлён ({}).", self.host.label_ru());
                self.message_is_error = false;
            }
            Err(error) => {
                self.status_line = format!("служба печати недоступна — {}", self.host.label_ru());
                self.message = error;
                self.message_is_error = true;
            }
        }
    }

    fn start_job(&mut self, job: Job) {
        if self.busy {
            log::warn("Повторный запуск операции проигнорирован: уже выполняется");
            return;
        }
        if let Err(error) = self.sync_host_from_input() {
            self.message = error;
            self.message_is_error = true;
            return;
        }
        self.busy = true;
        self.message_is_error = false;
        let where_ = self.host.label_ru();
        self.message = match job {
            Job::Refresh => format!("Обновление статуса ({where_})…"),
            Job::Stop => format!("Остановка Spooler ({where_})…"),
            Job::Start => format!("Запуск Spooler ({where_})…"),
            Job::Restart => format!("Перезапуск Spooler ({where_})…"),
            Job::ClearOnly => format!("Очистка очереди ({where_})…"),
            Job::FixClear => format!("Очистка очереди и перезапуск ({where_})…"),
        };
        log::info(&format!("{}: начало", self.message.trim_end_matches('…')));

        let (tx, rx) = mpsc::channel();
        self.result_rx = Some(rx);
        let host = self.host.clone();

        thread::spawn(move || {
            let timeout = spooler::DEFAULT_CONTROL_TIMEOUT;
            let pack = |label: &'static str,
                        status: SpoolerStatus,
                        report: Option<ClearReport>| {
                let snap = queue::queue_snapshot_on(&host);
                JobResult::Done {
                    label,
                    status,
                    report,
                    snap,
                }
            };
            let result = match job {
                Job::Refresh => match spooler::query_spooler_status_on(&host) {
                    Ok(status) => pack("Обновление", status, None),
                    Err(error) => JobResult::Failed {
                        label: "Обновление",
                        error,
                    },
                },
                Job::Stop => match spooler::stop_spooler_on(&host, timeout) {
                    Ok(status) => pack("Остановка", status, None),
                    Err(error) => JobResult::Failed {
                        label: "Остановка",
                        error,
                    },
                },
                Job::Start => match spooler::start_spooler_on(&host, timeout) {
                    Ok(status) => pack("Запуск", status, None),
                    Err(error) => JobResult::Failed {
                        label: "Запуск",
                        error,
                    },
                },
                Job::Restart => match spooler::restart_spooler_on(&host, timeout) {
                    Ok(status) => pack("Перезапуск", status, None),
                    Err(error) => JobResult::Failed {
                        label: "Перезапуск",
                        error,
                    },
                },
                Job::ClearOnly => match queue::clear_queue_on(&host, timeout) {
                    Ok((report, status)) => pack("Очистка очереди", status, Some(report)),
                    Err(error) => JobResult::Failed {
                        label: "Очистка очереди",
                        error,
                    },
                },
                Job::FixClear => match queue::fix_queue_on(&host, timeout) {
                    Ok((report, status)) => pack("Очистка и перезапуск", status, Some(report)),
                    Err(error) => JobResult::Failed {
                        label: "Очистка и перезапуск",
                        error,
                    },
                },
            };
            let _ = tx.send(result);
        });
    }

    fn poll_job(&mut self, ctx: &egui::Context) {
        let Some(rx) = self.result_rx.as_ref() else {
            return;
        };
        match rx.try_recv() {
            Ok(JobResult::Done {
                label,
                status,
                report,
                snap,
            }) => {
                self.apply_status_fields(&status);
                self.apply_queue_snap(snap);
                let had_failures = report.as_ref().is_some_and(|r| !r.failed.is_empty());
                let left_stopped =
                    label == "Очистка очереди" && status.state == spooler::ServiceState::Stopped;
                if left_stopped {
                    self.prompt_start = true;
                }
                self.message = if let Some(report) = report {
                    let detail = report.detail_ru();
                    if had_failures {
                        log::warn(&format!(
                            "{label}: {detail}; «{}»",
                            status.state.as_ru_str()
                        ));
                    } else {
                        log::info(&format!(
                            "{label}: {detail}; «{}»",
                            status.state.as_ru_str()
                        ));
                    }
                    let mut text = format!("{label}: {detail}");
                    if left_stopped {
                        text.push_str(
                            " Служба оставлена остановленной — нажмите «Запустить службу».",
                        );
                    }
                    text
                } else {
                    log::info(&format!("{label}: ок («{}»)", status.state.as_ru_str()));
                    done_message(label)
                };
                self.message_is_error = had_failures;
                self.busy = false;
                self.result_rx = None;
                self.last_auto_status = Instant::now();
                if matches!(label, "Перезапуск" | "Очистка и перезапуск") {
                    self.watchdog.note_restart_done();
                    if let Some(tray) = &self.tray {
                        tray.set_tooltip(&format!(
                            "SpoolCtl {APP_VERSION}\nСторож работает, пока иконка в трее"
                        ));
                    }
                }
                self.refresh_log_view();
            }
            Ok(JobResult::Failed { label, error }) => {
                log::error(&format!("{label}: {error}"));
                self.status_line = format!("служба печати недоступна — {}", self.host.label_ru());
                self.message = format!("{label}: {error}");
                self.message_is_error = true;
                self.busy = false;
                self.result_rx = None;
                self.last_auto_status = Instant::now();
                self.refresh_log_view();
            }
            Err(mpsc::TryRecvError::Empty) => {
                ctx.request_repaint_after(Duration::from_millis(100));
            }
            Err(mpsc::TryRecvError::Disconnected) => {
                log::error("Фоновая операция оборвалась");
                self.message = "Фоновая операция оборвалась.".to_owned();
                self.message_is_error = true;
                self.busy = false;
                self.result_rx = None;
                self.refresh_log_view();
            }
        }
    }

    fn request_admin_relaunch(&mut self, ctx: &egui::Context) {
        log::info("Запрос запуска от имени администратора");
        let pos = elevate::current_window_pos(ctx);
        if let Some((x, y)) = pos {
            log::info(&format!("UAC relaunch: сохраняем позицию окна ({x},{y})"));
        }
        // Tear down tray host first so the elevated copy does not activate us / inherit a hidden state.
        self.tray = None;
        // Release single-instance mutex first; otherwise the elevated copy sees
        // this process still holding it, "activates" this closing window, and exits.
        single_instance::release_gui();
        match elevate::relaunch_elevated(pos) {
            Ok(()) => {
                log::info("UAC принят, закрытие текущего окна");
                std::process::exit(0);
            }
            Err(error) => {
                log::error(&error);
                // UAC cancelled or failed — take the GUI slot again if possible.
                let _ = single_instance::try_acquire_gui();
                // Tray was dropped above; reinstall so watchdog balloon still works.
                match tray::AppTray::install(APP_VERSION) {
                    Ok(tray) => self.tray = Some(tray),
                    Err(tray_err) => log::warn(&format!("Трей после отмены UAC: {tray_err}")),
                }
                self.message = error;
                self.message_is_error = true;
                self.refresh_log_view();
            }
        }
    }

    fn draw_service_actions(&mut self, ui: &mut egui::Ui, p: Palette) {
        let gap = 8.0;
        let height = 72.0;
        let row_width = ui.available_width();
        let width = ((row_width - gap * 2.0) / 3.0).clamp(100.0, 220.0);
        let size = egui::vec2(width, height);

        let (start_fill, start_text) = if self.prompt_start {
            (p.accent, p.on_accent)
        } else {
            (p.soft_btn, p.text)
        };
        let start_tip = if self.prompt_start {
            "Spooler остановлен. Нажмите, чтобы снова запустить службу печати."
        } else {
            "Запускает службу Spooler, если она остановлена. Обычно нужны права администратора."
        };

        // Row 1 — управление службой; row 2 — статус и очередь.
        let rows = [
            [
                (
                    ServiceIcon::Start,
                    "Старт",
                    start_tip,
                    start_fill,
                    start_text,
                    ActionClick::Job(Job::Start),
                ),
                (
                    ServiceIcon::Stop,
                    "Стоп",
                    "Останавливает службу Spooler. Печать станет недоступна, пока службу не запустят снова.",
                    p.soft_btn,
                    p.text,
                    ActionClick::Confirm(Job::Stop),
                ),
                (
                    ServiceIcon::Restart,
                    "Перезапуск",
                    "Останавливает и снова запускает службу печати. Помогает при зависании очереди. Нужны права администратора.",
                    p.accent_active,
                    p.on_accent,
                    ActionClick::Confirm(Job::Restart),
                ),
            ],
            [
                (
                    ServiceIcon::Refresh,
                    "Обновить",
                    "Запрашивает текущее состояние службы печати Spooler без изменений.",
                    p.soft_btn,
                    p.text,
                    ActionClick::Job(Job::Refresh),
                ),
                (
                    ServiceIcon::Clear,
                    "Очистить",
                    "Останавливает Spooler и удаляет файлы заданий в spool\\PRINTERS. Службу нужно запустить отдельно.",
                    p.soft_btn,
                    p.text,
                    ActionClick::Confirm(Job::ClearOnly),
                ),
                (
                    ServiceIcon::ClearRestart,
                    "Очист.+старт",
                    "Останавливает Spooler, удаляет файлы заданий в spool\\PRINTERS, затем запускает службу снова. Удалит текущие задания печати.",
                    p.accent,
                    p.on_accent,
                    ActionClick::Confirm(Job::FixClear),
                ),
            ],
        ];

        for (row_idx, row) in rows.into_iter().enumerate() {
            if row_idx > 0 {
                ui.add_space(gap);
            }
            ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
                for (i, (icon, label, tip, fill, text_color, click)) in row.into_iter().enumerate()
                {
                    if i > 0 {
                        ui.add_space(gap);
                    }
                    if themed_action_tile(ui, size, icon, label, tip, fill, text_color) {
                        match click {
                            ActionClick::Job(job) => self.start_job(job),
                            ActionClick::Confirm(job) => self.confirm = Some(Confirm::Job(job)),
                        }
                    }
                }
            });
        }
    }

    fn draw_utils_bar(&mut self, ui: &mut egui::Ui, p: Palette) {
        // One row, equal boxes, vertically centered — no wrapping to different levels.
        ui.with_layout(egui::Layout::left_to_right(egui::Align::Center), |ui| {
            let gap = 8.0;
            let count = 5.0_f32;
            let height = 40.0;
            let width = ((ui.available_width() - gap * (count - 1.0)) / count).clamp(96.0, 160.0);
            let size = [width, height];

            let labels = [
                (
                    "Обновить журнал",
                    "Перечитать последние строки из файла журнала.",
                ),
                (
                    "Папка журнала",
                    "Открыть проводник с файлом spoolctl.log.",
                ),
                (
                    "Папка очереди",
                    "Открыть spool\\PRINTERS в проводнике (файлы заданий печати).",
                ),
                (
                    "Папка принтеров",
                    "Открыть список принтеров Windows (Devices and Printers / Printers folder).",
                ),
            ];

            for (i, (label, tip)) in labels.iter().enumerate() {
                if i > 0 {
                    ui.add_space(gap);
                }
                let clicked = themed_button_bar(ui, size, label, tip, p.util_btn, p.on_util);
                if !clicked {
                    continue;
                }
                match i {
                    0 => self.refresh_log_view(),
                    1 => match log::open_log_dir() {
                        Ok(()) => {
                            self.message = "Папка журнала открыта.".to_owned();
                            self.message_is_error = false;
                        }
                        Err(error) => {
                            self.message = error;
                            self.message_is_error = true;
                        }
                    },
                    2 => match queue::open_queue_dir() {
                        Ok(()) => {
                            self.message = "Папка очереди открыта.".to_owned();
                            self.message_is_error = false;
                        }
                        Err(error) => {
                            self.message = error;
                            self.message_is_error = true;
                        }
                    },
                    3 => match queue::open_printers_folder() {
                        Ok(()) => {
                            self.message = "Папка принтеров открыта.".to_owned();
                            self.message_is_error = false;
                        }
                        Err(error) => {
                            self.message = error;
                            self.message_is_error = true;
                        }
                    },
                    _ => {}
                }
            }

            ui.add_space(gap);
            let ncd_tip = format!(
                "{}\nОтключает автоустановку сетевых устройств (WSD) через реестр AutoSetup=0 и службу NcdAutoSetup. Нужен администратор. Рекомендуется перезагрузка.",
                self.ncd_summary
            );
            if themed_button_bar(
                ui,
                size,
                "Откл. автонастройку",
                &ncd_tip,
                p.util_btn,
                p.on_util,
            ) {
                if !self.elevated {
                    self.message =
                        "Для отключения автонастройки нужны права администратора.".to_owned();
                    self.message_is_error = true;
                } else {
                    self.confirm = Some(Confirm::DisableNcd);
                }
            }
        });
    }

    fn draw_watchdog_controls(&mut self, ui: &mut egui::Ui, p: Palette) {
        ui.vertical(|ui| {
            ui.label(
                RichText::new("Сторож зависания")
                    .size(15.0)
                    .strong()
                    .color(p.text),
            );
            ui.add_space(2.0);

            let mut watch = self.watchdog.settings.watch_enabled;
            let watch_resp = ui.checkbox(&mut watch, "Следить за зависанием очереди");
            if watch_resp.changed() {
                self.watchdog.settings.watch_enabled = watch;
                if !watch {
                    self.watchdog.settings.auto_restart = false;
                    self.watchdog.reset_tracking();
                }
                self.watchdog.save_settings();
                log::info(&format!(
                    "Сторож зависания: слежение {}",
                    if watch { "вкл" } else { "выкл" }
                ));
                self.refresh_log_view();
            }
            watch_resp.on_hover_text(
                "Каждые несколько секунд проверяет очередь и состояние Spooler. При зависании покажет предупреждение.",
            );

            let mut auto = self.watchdog.settings.auto_restart;
            let auto_resp = ui.checkbox(&mut auto, "Автоперезапуск при зависании");
            if auto_resp.changed() {
                if auto {
                    // Revert until the user confirms the warning dialog.
                    self.watchdog.settings.auto_restart = false;
                    self.confirm = Some(Confirm::EnableAutoRestart);
                } else {
                    self.watchdog.settings.auto_restart = false;
                    self.watchdog.save_settings();
                    log::info("Сторож зависания: автоперезапуск выкл");
                    self.message = "Автоперезапуск выключен.".to_owned();
                    self.message_is_error = false;
                    self.refresh_log_view();
                }
            }
            auto_resp.on_hover_text(
                "ВНИМАНИЕ: при зависании Spooler перезапустится сам. Нужно явное подтверждение и права администратора.",
            );

            if self.watchdog.settings.auto_restart {
                ui.label(
                    RichText::new(
                        "Автоперезапуск активен: при зависании служба печати будет перезапущена.",
                    )
                    .size(13.0)
                    .color(p.error),
                );
            } else if self.watchdog.settings.watch_enabled {
                ui.label(
                    RichText::new("Только предупреждение — перезапуск вручную.")
                        .size(13.0)
                        .color(p.accent),
                );
            }
            if self.tray.is_some() {
                ui.label(
                    RichText::new(
                        "Закрытие окна → в трей (сторож продолжает работу). Выход — из меню иконки.",
                    )
                    .size(13.0)
                    .color(p.accent),
                );
            }

            ui.add_space(6.0);
            ui.label(
                RichText::new("Автозапуск Windows")
                    .size(15.0)
                    .strong()
                    .color(p.text),
            );
            let mut boot = self.autostart_enabled;
            let boot_resp = ui.checkbox(
                &mut boot,
                "Запускать при входе в Windows (в трее, с правами администратора)",
            );
            if boot_resp.changed() {
                if boot {
                    self.autostart_enabled = false;
                    self.confirm = Some(Confirm::EnableAutostart);
                } else {
                    match autostart::disable() {
                        Ok(()) => {
                            self.autostart_enabled = false;
                            log::info("Автозапуск с Windows: выкл");
                            self.message = "Автозапуск при входе выключен.".to_owned();
                            self.message_is_error = false;
                        }
                        Err(error) => {
                            log::error(&format!("Автозапуск: {error}"));
                            self.message = error;
                            self.message_is_error = true;
                            self.autostart_enabled = autostart::is_enabled();
                        }
                    }
                    self.refresh_log_view();
                }
            }
            boot_resp.on_hover_text(
                "Задача Планировщика с наивысшими правами: после reboot SpoolCtl стартует в трее уже elevated (для учёток из группы Администраторы). Включение — один раз от имени администратора.",
            );
            if self.autostart_enabled {
                ui.label(
                    RichText::new(
                        "При входе: старт в трее с правами администратора (Планировщик).",
                    )
                    .size(13.0)
                    .color(p.ok),
                );
            } else if !self.elevated {
                ui.label(
                    RichText::new(
                        "Чтобы включить elevated-автозапуск — откройте программу от имени администратора.",
                    )
                    .size(13.0)
                    .color(p.accent),
                );
            }

            ui.add_space(6.0);
            ui.label(
                RichText::new("Уведомления о печати")
                    .size(15.0)
                    .strong()
                    .color(p.text),
            );
            let mut notify = self.print_notify.settings.notify_print_jobs;
            let notify_resp = ui.checkbox(
                &mut notify,
                "Сообщать, когда на принтер отправлено задание",
            );
            if notify_resp.changed() {
                self.print_notify.set_enabled(notify);
                log::info(&format!(
                    "Уведомления о печати: {}",
                    if notify { "вкл" } else { "выкл" }
                ));
                self.message = if notify {
                    "Уведомления о печати включены (все локальные принтеры, слежение в фоне)."
                        .to_owned()
                } else {
                    "Уведомления о печати выключены.".to_owned()
                };
                self.message_is_error = false;
                self.refresh_log_view();
            }
            notify_resp.on_hover_text(
                "Фоновое слежение: событие Spooler + опрос ~1 с. Balloon в трее с именем принтера. Не связано со сторожем зависания.",
            );
            if self.print_notify.settings.notify_print_jobs {
                ui.label(
                    RichText::new(
                        "Слежение в фоне: все локальные принтеры (работает и из трея).",
                    )
                    .size(13.0)
                    .color(p.accent),
                );
            }
        });
    }

    fn hide_to_tray(&mut self) {
        tray::hide_main_window();
        self.window_in_tray = true;
        log::info("Окно скрыто в трей — процесс и сторож продолжают работу");
        self.message =
            "Окно в трее. Сторож работает. Иконка у часов (смотрите «^»). Выход — ПКМ → «Выход»."
                .to_owned();
        self.message_is_error = false;
        if let Some(tray) = &self.tray {
            tray.balloon(
                "SpoolCtl в трее",
                "Программа продолжает работать. Иконка у часов — откройте «^», если её не видно.",
            );
            tray.set_tooltip(&format!(
                "SpoolCtl {APP_VERSION}\nВ трее — сторож работает"
            ));
        }
        self.refresh_log_view();
    }

    fn poll_tray(&mut self, ctx: &egui::Context) {
        let mut cmds = Vec::new();
        if let Some(tray) = &self.tray {
            while let Some(cmd) = tray.poll() {
                cmds.push(cmd);
            }
        } else {
            return;
        }
        for cmd in cmds {
            match cmd {
                TrayCmd::Show => {
                    tray::show_main_window();
                    self.window_in_tray = false;
                    log::info("Окно восстановлено из трея");
                    self.refresh_log_view();
                    ctx.request_repaint();
                }
                TrayCmd::Hide => {
                    self.hide_to_tray();
                    ctx.request_repaint();
                }
                TrayCmd::Quit => {
                    log::info("Выход из трея");
                    self.allow_exit = true;
                    tray::show_main_window();
                    self.window_in_tray = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::Close);
                }
            }
        }
    }
}

impl eframe::App for SpoolCtlApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if self.start_in_tray && self.tray.is_some() {
            self.start_in_tray = false;
            log::info("Старт в трее (--tray)");
            self.hide_to_tray();
        } else if self.force_show_window {
            // After UAC relaunch (`--show`) always bring the window forward — never stay in tray.
            self.force_show_window = false;
            tray::show_main_window();
            self.window_in_tray = false;
            ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            log::info("Окно показано после запуска от администратора");
        }

        self.poll_tray(ctx);

        if ctx.input(|i| i.viewport().close_requested()) {
            if AppTray::take_allow_exit() {
                self.allow_exit = true;
            }
            if self.allow_exit || self.tray.is_none() {
                // Let the window close and the process exit.
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                // Ask once; don't stack dialogs if already open.
                if !matches!(self.confirm, Some(Confirm::CloseOrTray)) {
                    self.confirm = Some(Confirm::CloseOrTray);
                }
            }
        }

        self.poll_job(ctx);
        if self.print_notify.settings.notify_print_jobs {
            self.run_print_notify_tick();
        }
        if self.busy || self.confirm.is_some() {
            return;
        }
        let elapsed = self.last_auto_status.elapsed();
        if elapsed >= AUTO_STATUS_REFRESH {
            self.quiet_status_refresh();
            ctx.request_repaint_after(AUTO_STATUS_REFRESH);
        } else {
            ctx.request_repaint_after(AUTO_STATUS_REFRESH.saturating_sub(elapsed));
        }
    }

    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let p = Palette::for_dark(ui.visuals().dark_mode);

        // Right: printers as a vertical list (own panel).
        egui::Panel::right("printers_panel")
            .resizable(true)
            .default_size(230.0)
            .size_range(180.0..=300.0)
            .frame(egui::Frame::side_top_panel(ui.style()).fill(p.bg))
            .show(ui, |ui| {
                ui.add_space(8.0);
                ui.label(
                    RichText::new("Принтеры в системе")
                        .size(15.0)
                        .strong()
                        .color(p.text),
                );
                ui.add_space(6.0);
                egui::ScrollArea::vertical()
                    .id_salt("printers_list")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.printers.is_empty() {
                            ui.label(
                                RichText::new("Список пуст или недоступен.")
                                    .size(13.0)
                                    .color(p.text),
                            );
                        } else {
                            for (i, printer) in self.printers.iter().enumerate() {
                                if i > 0 {
                                    ui.add_space(8.0);
                                    ui.separator();
                                    ui.add_space(6.0);
                                }
                                ui.label(
                                    RichText::new(&printer.name)
                                        .size(14.0)
                                        .strong()
                                        .color(p.text),
                                );
                                let meta = printer.meta_ru();
                                ui.label(RichText::new(meta).size(13.0).color(p.accent));
                                ui.label(
                                    RichText::new(jobs_phrase_ui(printer.jobs))
                                        .size(12.0)
                                        .color(p.text),
                                );
                            }
                        }
                    });
            });

        egui::CentralPanel::default()
            .frame(egui::Frame::central_panel(ui.style()).fill(p.panel))
            .show(ui, |ui| {
                // Outer scroll so footer buttons stay reachable when content grows.
                egui::ScrollArea::vertical()
                    .id_salt("main_scroll")
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.heading(
                        RichText::new("SpoolCtl")
                            .color(p.accent)
                            .size(28.0)
                            .strong(),
                    );
                    ui.label(
                        RichText::new(format!("v{APP_VERSION}"))
                            .size(16.0)
                            .color(p.accent),
                    );
                });
                ui.label(
                    RichText::new("Управление службой печати Windows (Spooler)")
                        .size(17.0)
                        .color(p.text),
                );
                ui.add_space(10.0);

                ui.horizontal(|ui| {
                    ui.label(
                        RichText::new("Удалённый ПК:")
                            .size(16.0)
                            .strong()
                            .color(p.text),
                    );
                    let resp = ui.add(
                        egui::TextEdit::singleline(&mut self.host_input)
                            .desired_width(380.0)
                            .hint_text("например: 192.168.1.50 или OFFICE-PC"),
                    );
                    if resp.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                        self.apply_host_input();
                    }
                    resp.on_hover_text(
                        "Формат: имя ПК (OFFICE-PC) или IPv4 (192.168.1.50).\n\
                         Можно с \\\\ в начале. Пусто — этот компьютер.\n\
                         Не указывайте путь, порт и http://",
                    );
                    if themed_button_bar(
                        ui,
                        [100.0, 30.0],
                        "Применить",
                        "Подключиться к указанному ПК через SCM (агент не нужен). Пустое поле — этот компьютер.",
                        p.soft_btn,
                        p.text,
                    ) {
                        self.apply_host_input();
                    }
                    if !self.host.is_local() {
                        ui.label(
                            RichText::new(format!("→ {}", self.host.name()))
                                .size(14.0)
                                .color(p.accent)
                                .strong(),
                        );
                    }
                });
                ui.label(
                    RichText::new(
                        "Формат адреса: 192.168.1.50  ·  OFFICE-PC  ·  pc.firma.local   (пусто = этот ПК)",
                    )
                    .size(13.0)
                    .color(p.accent),
                );
                if !self.host.is_local() {
                    ui.label(
                        RichText::new(
                            "Сторож зависания работает только для этого ПК; кнопки ниже — для удалённого.",
                        )
                        .size(13.0)
                        .color(p.text),
                    );
                }

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Права:").size(16.0).strong().color(p.text));
                    if self.elevated {
                        ui.label(
                            RichText::new("администратор")
                                .size(16.0)
                                .color(p.ok)
                                .strong(),
                        );
                    } else {
                        ui.label(
                            RichText::new("обычный пользователь")
                                .size(16.0)
                                .color(p.error)
                                .strong(),
                        );
                        ui.add_space(10.0);
                        // Статическая подсказка (было):
                        // if themed_button_bar(
                        //     ui,
                        //     [168.0, 30.0],
                        //     "От админа",
                        //     "Откроет окно UAC и перезапустит SpoolCtl с правами администратора. Нужно для остановки и перезапуска службы.",
                        //     p.accent,
                        //     p.on_accent,
                        // ) {
                        //     self.request_admin_relaunch();
                        // }
                        {
                            const ADMIN_TIP: &str = "Откроет окно UAC и перезапустит SpoolCtl с правами администратора. Нужно для остановки и перезапуска службы.";
                            let resp = themed_button_bar_resp(
                                ui,
                                [168.0, 30.0],
                                "От админа",
                                p.accent,
                                p.on_accent,
                            );
                            if resp.hovered() {
                                let started =
                                    self.admin_tip_started.get_or_insert_with(Instant::now);
                                let chars = (started.elapsed().as_secs_f32() * 36.0) as usize;
                                let shown = typewriter_prefix(ADMIN_TIP, chars);
                                let tip_color = if ui.visuals().dark_mode {
                                    Color32::from_rgb(235, 244, 255)
                                } else {
                                    Color32::from_rgb(18, 52, 86)
                                };
                                resp.show_tooltip_ui(|ui| {
                                    ui.set_max_width(320.0);
                                    ui.label(
                                        RichText::new(shown)
                                            .font(FontId::proportional(BUTTON_FONT_SIZE))
                                            .color(tip_color),
                                    );
                                });
                                if shown != ADMIN_TIP {
                                    ui.ctx().request_repaint();
                                }
                            } else {
                                self.admin_tip_started = None;
                            }
                            if resp.clicked() {
                                self.request_admin_relaunch(ui.ctx());
                            }
                        }
                    }
                });

                ui.add_space(6.0);
                ui.label(
                    RichText::new(&self.status_line)
                        .size(20.0)
                        .color(p.accent)
                        .strong(),
                )
                .on_hover_text(
                    "Как проверить, что SpoolCtl видит службу:\n\
                     в командной строке от администратора:\n\
                     sc stop Spooler  — здесь должно стать «остановлена»\n\
                     sc start Spooler — снова «работает»\n\
                     (это ручная проверка статуса, не автоперезапуск)",
                );
                if self.host.is_local() {
                    ui.label(
                        RichText::new(
                            "Проверка: sc stop Spooler / sc start Spooler — статус выше должен смениться",
                        )
                        .size(13.0)
                        .color(p.text),
                    );
                }
                ui.label(RichText::new(&self.queue_line).size(16.0).color(p.text));
                ui.add_space(6.0);
                self.draw_watchdog_controls(ui, p);
                ui.add_space(8.0);
                let msg_color = if self.message_is_error {
                    p.error
                } else if self.busy {
                    p.accent
                } else {
                    p.text
                };
                ui.label(RichText::new(&self.message).size(16.0).color(msg_color));

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(10.0);

                ui.add_enabled_ui(!self.busy, |ui| {
                    self.draw_service_actions(ui, p);
                });

                if self.busy {
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            RichText::new("Выполняется… не закрывайте окно.")
                                .size(16.0)
                                .color(p.accent),
                        );
                    });
                }

                ui.add_space(12.0);
                ui.separator();
                ui.add_space(6.0);
                if !self.elevated {
                    ui.label(
                        RichText::new(
                            "Для остановки, очистки очереди и перезапуска нужны права администратора.",
                        )
                        .size(15.0)
                        .color(p.text),
                    );
                    ui.add_space(6.0);
                }

                ui.label(RichText::new("Журнал").size(16.0).strong().color(p.text));
                ui.label(
                    RichText::new(format!("Файл: {}", log::log_path().display()))
                        .size(13.0)
                        .color(p.accent),
                );
                ui.add_space(4.0);
                egui::ScrollArea::vertical()
                    .id_salt("log_tail")
                    .max_height(90.0)
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        if self.log_lines.is_empty() {
                            ui.label(
                                RichText::new("Записей пока нет.")
                                    .size(14.0)
                                    .color(p.text),
                            );
                        } else {
                            for line in &self.log_lines {
                                ui.label(RichText::new(line).size(13.0).color(p.text));
                            }
                        }
                    });

                ui.add_space(6.0);
                ui.separator();
                ui.add_space(8.0);
                self.draw_utils_bar(ui, p);
                ui.add_space(12.0);
                    });
            });

        if let Some(confirm) = self.confirm {
            if matches!(confirm, Confirm::CloseOrTray) {
                egui::Window::new(
                    RichText::new("Закрыть окно?")
                        .size(18.0)
                        .color(p.text)
                        .strong(),
                )
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .frame(egui::Frame::window(ui.style()).fill(p.bg))
                .show(ui.ctx(), |ui| {
                    ui.label(
                        RichText::new(
                            "Свернуть в трей (сторож продолжит работу) или полностью закрыть программу?",
                        )
                        .size(16.0)
                        .color(p.text),
                    );
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if themed_button_bar(
                            ui,
                            [160.0, 34.0],
                            "Отмена",
                            "Оставить окно открытым.",
                            p.soft_btn,
                            p.text,
                        ) {
                            self.confirm = None;
                        }
                        if themed_button_bar(
                            ui,
                            [160.0, 34.0],
                            "В трей",
                            "Скрыть окно; процесс и сторож продолжат работу.",
                            p.accent,
                            p.on_accent,
                        ) {
                            self.confirm = None;
                            self.hide_to_tray();
                        }
                        if themed_button_bar(
                            ui,
                            [160.0, 34.0],
                            "Закрыть программу",
                            "Полностью выйти из SpoolCtl.",
                            p.accent,
                            p.on_accent,
                        ) {
                            self.confirm = None;
                            self.allow_exit = true;
                            log::info("Выход по выбору пользователя (закрыть программу)");
                            ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                        }
                    });
                });
                return;
            }

            let (title, body) = match confirm {
                Confirm::Job(Job::Stop) => (
                    "Остановить службу?",
                    "Печать на этом ПК будет недоступна, пока службу не запустят снова.",
                ),
                Confirm::Job(Job::Restart) => (
                    "Перезапустить службу?",
                    "Текущие задания печати могут прерваться.",
                ),
                Confirm::Job(Job::ClearOnly) => (
                    "Очистить очередь печати?",
                    "Spooler будет остановлен, файлы заданий удалены. Служба останется остановленной — запустите её кнопкой «Запустить службу».",
                ),
                Confirm::Job(Job::FixClear) => (
                    "Очистить очередь и перезапустить?",
                    "Будут удалены файлы заданий в папке очереди, затем Spooler перезапустится. Отменить удаление будет нельзя.",
                ),
                Confirm::Job(Job::Refresh | Job::Start) => unreachable!(),
                Confirm::DisableNcd => (
                    "Отключить автонастройку устройств?",
                    "Будет выключена автоматическая установка сетевых устройств (WSD): реестр AutoSetup=0 и служба NcdAutoSetup. Уже установленные принтеры не удаляются. Рекомендуется перезагрузка Windows.",
                ),
                Confirm::EnableAutoRestart => (
                    "Включить автоперезапуск при зависании?",
                    "ВНИМАНИЕ: при подозрении на зависание очереди (задания без прогресса ~90 с) или застревании службы Spooler будет перезапущен автоматически. Текущая печать может прерваться. Ложные срабатывания возможны на медленных принтерах. Нужны права администратора. Можно отключить галочкой в любой момент.",
                ),
                Confirm::EnableAutostart => (
                    "Автозапуск с правами администратора?",
                    "Будет создана задача Планировщика Windows: при вашем входе SpoolCtl стартует в трее уже с повышенными правами (без повторного UAC), чтобы автоперезапуск Spooler работал после reboot. Нужна учётка из группы «Администраторы». Включение — один раз из окна администратора. Старая запись HKCU\\Run будет убрана.",
                ),
                Confirm::CloseOrTray => unreachable!(),
            };

            egui::Window::new(RichText::new(title).size(18.0).color(p.text).strong())
                .collapsible(false)
                .resizable(false)
                .anchor(egui::Align2::CENTER_CENTER, [0.0, 0.0])
                .frame(egui::Frame::window(ui.style()).fill(p.bg))
                .show(ui.ctx(), |ui| {
                    ui.label(RichText::new(body).size(16.0).color(p.text));
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        if themed_button_sized(
                            ui,
                            [120.0, 34.0],
                            "Отмена",
                            "Закрыть это окно без изменений.",
                            p.soft_btn,
                            p.text,
                            true,
                        )
                        .clicked()
                        {
                            self.confirm = None;
                        }
                        if themed_button_sized(
                            ui,
                            [160.0, 34.0],
                            "Да, выполнить",
                            "Подтвердить действие.",
                            p.accent,
                            p.on_accent,
                            true,
                        )
                        .clicked()
                        {
                            self.confirm = None;
                            match confirm {
                                Confirm::Job(job) => self.start_job(job),
                                Confirm::DisableNcd => {
                                    match ncd::disable_autosetup() {
                                        Ok(detail) => {
                                            log::info(&format!("NcdAutoSetup: {detail}"));
                                            self.message = detail;
                                            self.message_is_error = false;
                                        }
                                        Err(error) => {
                                            log::error(&format!("NcdAutoSetup: {error}"));
                                            self.message = error;
                                            self.message_is_error = true;
                                        }
                                    }
                                    self.refresh_ncd_summary();
                                    self.refresh_log_view();
                                }
                                Confirm::EnableAutoRestart => {
                                    self.watchdog.settings.watch_enabled = true;
                                    self.watchdog.settings.auto_restart = true;
                                    self.watchdog.save_settings();
                                    log::info(
                                        "Сторож зависания: автоперезапуск включён (явное согласие)",
                                    );
                                    self.message =
                                        "Автоперезапуск при зависании включён. Слежение тоже включено."
                                            .to_owned();
                                    self.message_is_error = false;
                                    self.refresh_log_view();
                                }
                                Confirm::EnableAutostart => {
                                    match autostart::enable() {
                                        Ok(()) => {
                                            self.autostart_enabled = true;
                                            log::info(
                                                "Автозапуск: задача Планировщика /RL HIGHEST (явное согласие)",
                                            );
                                            self.message =
                                                "Автозапуск включён: после входа — в трее с правами администратора."
                                                    .to_owned();
                                            self.message_is_error = false;
                                        }
                                        Err(error) => {
                                            self.autostart_enabled = false;
                                            log::error(&format!("Автозапуск: {error}"));
                                            self.message = error;
                                            self.message_is_error = true;
                                        }
                                    }
                                    self.refresh_log_view();
                                }
                                Confirm::CloseOrTray => unreachable!(),
                            }
                        }
                    });
                });
        }
    }
}

const BUTTON_FONT_SIZE: f32 = 16.0;
/// Soft rounded corners for all action / util / dialog buttons.
const BUTTON_CORNER: f32 = 10.0;

#[derive(Clone, Copy)]
enum ServiceIcon {
    Refresh,
    Start,
    Stop,
    Restart,
    Clear,
    ClearRestart,
}

enum ActionClick {
    Job(Job),
    Confirm(Job),
}

fn jobs_phrase_ui(n: usize) -> String {
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

fn themed_button_sized(
    ui: &mut egui::Ui,
    size: [f32; 2],
    label: &str,
    tip: &str,
    fill: Color32,
    text_color: Color32,
    bold: bool,
) -> egui::Response {
    let mut text = RichText::new(label)
        .font(FontId::proportional(BUTTON_FONT_SIZE))
        .color(text_color);
    if bold {
        text = text.strong();
    }
    let tip_color = if ui.visuals().dark_mode {
        Color32::from_rgb(235, 244, 255)
    } else {
        Color32::from_rgb(18, 52, 86)
    };
    ui.add_sized(
        size,
        egui::Button::new(text)
            .fill(fill)
            .corner_radius(BUTTON_CORNER)
            .wrap_mode(egui::TextWrapMode::Wrap),
    )
    .on_hover_text(
        RichText::new(tip)
            .font(FontId::proportional(BUTTON_FONT_SIZE))
            .color(tip_color),
    )
}

/// Equal footer buttons: fixed box, single-line truncate, same baseline.
fn themed_button_bar(
    ui: &mut egui::Ui,
    size: [f32; 2],
    label: &str,
    tip: &str,
    fill: Color32,
    text_color: Color32,
) -> bool {
    let tip_color = if ui.visuals().dark_mode {
        Color32::from_rgb(235, 244, 255)
    } else {
        Color32::from_rgb(18, 52, 86)
    };
    themed_button_bar_resp(ui, size, label, fill, text_color)
        .on_hover_text(
            RichText::new(tip)
                .font(FontId::proportional(BUTTON_FONT_SIZE))
                .color(tip_color),
        )
        .clicked()
}

fn themed_button_bar_resp(
    ui: &mut egui::Ui,
    size: [f32; 2],
    label: &str,
    fill: Color32,
    text_color: Color32,
) -> egui::Response {
    let text = RichText::new(label)
        .font(FontId::proportional(14.0))
        .color(text_color)
        .strong();
    ui.add_sized(
        size,
        egui::Button::new(text)
            .fill(fill)
            .corner_radius(BUTTON_CORNER)
            .wrap_mode(egui::TextWrapMode::Truncate),
    )
}

fn typewriter_prefix(text: &str, n_chars: usize) -> &str {
    match text.char_indices().nth(n_chars) {
        Some((idx, _)) => &text[..idx],
        None => text,
    }
}

/// Compact service action: icon on top, short label below.
fn themed_action_tile(
    ui: &mut egui::Ui,
    size: egui::Vec2,
    icon: ServiceIcon,
    label: &str,
    tip: &str,
    fill: Color32,
    text_color: Color32,
) -> bool {
    let tip_color = if ui.visuals().dark_mode {
        Color32::from_rgb(235, 244, 255)
    } else {
        Color32::from_rgb(18, 52, 86)
    };
    let (rect, response) = ui.allocate_exact_size(size, egui::Sense::click());
    let visuals = ui.style().interact(&response);
    let bg = if response.hovered() || response.has_focus() {
        visuals.bg_fill
    } else {
        fill
    };
    let radius = egui::CornerRadius::same(BUTTON_CORNER as u8);
    ui.painter().rect_filled(rect, radius, bg);
    ui.painter().rect_stroke(
        rect,
        radius,
        egui::Stroke::new(1.0, visuals.bg_stroke.color),
        egui::StrokeKind::Inside,
    );

    let icon_rect = egui::Rect::from_center_size(
        egui::pos2(rect.center().x, rect.top() + size.y * 0.34),
        egui::vec2(size.x * 0.42, size.y * 0.34),
    );
    paint_service_icon(ui.painter(), icon_rect, icon, text_color);

    ui.painter().text(
        egui::pos2(rect.center().x, rect.bottom() - 14.0),
        egui::Align2::CENTER_CENTER,
        label,
        FontId::proportional(13.0),
        text_color,
    );

    response
        .on_hover_text(
            RichText::new(tip)
                .font(FontId::proportional(BUTTON_FONT_SIZE))
                .color(tip_color),
        )
        .clicked()
}

fn paint_service_icon(
    painter: &egui::Painter,
    rect: egui::Rect,
    icon: ServiceIcon,
    color: Color32,
) {
    let c = rect.center();
    let s = rect.width().min(rect.height()) * 0.5;
    let stroke = egui::Stroke::new(2.2, color);

    match icon {
        ServiceIcon::Refresh => {
            let r = s * 0.72;
            painter.circle_stroke(c, r, stroke);
            let tip = c + egui::vec2(r * 0.15, -r);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    tip + egui::vec2(-5.0, 2.0),
                    tip + egui::vec2(5.0, 2.0),
                    tip + egui::vec2(0.0, -6.0),
                ],
                color,
                egui::Stroke::NONE,
            ));
        }
        ServiceIcon::Start => {
            painter.add(egui::Shape::convex_polygon(
                vec![
                    c + egui::vec2(-s * 0.45, -s * 0.7),
                    c + egui::vec2(s * 0.75, 0.0),
                    c + egui::vec2(-s * 0.45, s * 0.7),
                ],
                color,
                egui::Stroke::NONE,
            ));
        }
        ServiceIcon::Stop => {
            painter.rect_filled(
                egui::Rect::from_center_size(c, egui::vec2(s * 1.15, s * 1.15)),
                2.0,
                color,
            );
        }
        ServiceIcon::Restart => {
            let r = s * 0.7;
            painter.circle_stroke(c, r, stroke);
            painter.add(egui::Shape::convex_polygon(
                vec![
                    c + egui::vec2(r * 0.2, -r),
                    c + egui::vec2(r * 0.95, -r * 0.35),
                    c + egui::vec2(r * 0.05, -r * 0.25),
                ],
                color,
                egui::Stroke::NONE,
            ));
            painter.line_segment(
                [
                    c + egui::vec2(-r * 0.15, r * 0.85),
                    c + egui::vec2(r * 0.55, r * 0.35),
                ],
                stroke,
            );
        }
        ServiceIcon::Clear => {
            painter.line_segment(
                [
                    c + egui::vec2(-s * 0.55, -s * 0.55),
                    c + egui::vec2(s * 0.55, -s * 0.55),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    c + egui::vec2(-s * 0.2, -s * 0.75),
                    c + egui::vec2(s * 0.2, -s * 0.75),
                ],
                stroke,
            );
            painter.rect_stroke(
                egui::Rect::from_min_max(
                    c + egui::vec2(-s * 0.45, -s * 0.4),
                    c + egui::vec2(s * 0.45, s * 0.75),
                ),
                2.0,
                stroke,
                egui::StrokeKind::Outside,
            );
            painter.line_segment(
                [c + egui::vec2(0.0, -s * 0.15), c + egui::vec2(0.0, s * 0.5)],
                stroke,
            );
        }
        ServiceIcon::ClearRestart => {
            painter.line_segment(
                [
                    c + egui::vec2(-s * 0.85, -s * 0.55),
                    c + egui::vec2(-s * 0.15, s * 0.15),
                ],
                stroke,
            );
            painter.line_segment(
                [
                    c + egui::vec2(-s * 0.15, -s * 0.55),
                    c + egui::vec2(-s * 0.85, s * 0.15),
                ],
                stroke,
            );
            painter.add(egui::Shape::convex_polygon(
                vec![
                    c + egui::vec2(s * 0.05, -s * 0.45),
                    c + egui::vec2(s * 0.9, s * 0.15),
                    c + egui::vec2(s * 0.05, s * 0.75),
                ],
                color,
                egui::Stroke::NONE,
            ));
        }
    }
}

fn apply_fonts(style: &mut egui::Style) {
    style
        .text_styles
        .insert(TextStyle::Heading, FontId::proportional(26.0));
    style
        .text_styles
        .insert(TextStyle::Body, FontId::proportional(16.0));
    style
        .text_styles
        .insert(TextStyle::Button, FontId::proportional(16.0));
    style
        .text_styles
        .insert(TextStyle::Small, FontId::proportional(15.0));
    style
        .text_styles
        .insert(TextStyle::Monospace, FontId::monospace(15.0));
    style.spacing.button_padding = egui::vec2(12.0, 8.0);
    style.spacing.item_spacing = egui::vec2(10.0, 8.0);
    // Show tip soon enough to be useful, but not instantly on every pass.
    style.interaction.tooltip_delay = 0.35;
}

fn soft_visuals(dark: bool) -> Visuals {
    let p = Palette::for_dark(dark);
    let mut visuals = if dark {
        Visuals::dark()
    } else {
        Visuals::light()
    };
    visuals.dark_mode = dark;
    visuals.window_fill = p.tip_bg;
    visuals.panel_fill = p.panel;
    visuals.override_text_color = Some(p.text);
    visuals.widgets.noninteractive.bg_fill = p.panel;
    visuals.widgets.noninteractive.fg_stroke.color = p.text;
    visuals.widgets.noninteractive.corner_radius = egui::CornerRadius::same(BUTTON_CORNER as u8);
    visuals.widgets.inactive.bg_fill = p.soft_btn;
    visuals.widgets.inactive.weak_bg_fill = p.soft_btn;
    visuals.widgets.inactive.fg_stroke.color = p.text;
    visuals.widgets.inactive.corner_radius = egui::CornerRadius::same(BUTTON_CORNER as u8);
    visuals.widgets.hovered.bg_fill = p.accent_hover;
    visuals.widgets.hovered.weak_bg_fill = p.accent_hover;
    visuals.widgets.hovered.fg_stroke.color = p.on_accent;
    visuals.widgets.hovered.corner_radius = egui::CornerRadius::same(BUTTON_CORNER as u8);
    visuals.widgets.active.bg_fill = p.accent_active;
    visuals.widgets.active.weak_bg_fill = p.accent_active;
    visuals.widgets.active.fg_stroke.color = p.on_accent;
    visuals.widgets.active.corner_radius = egui::CornerRadius::same(BUTTON_CORNER as u8);
    visuals.widgets.open.corner_radius = egui::CornerRadius::same(BUTTON_CORNER as u8);
    visuals.window_corner_radius = egui::CornerRadius::same(12);
    visuals.menu_corner_radius = egui::CornerRadius::same(8);
    visuals.selection.bg_fill = p.accent.gamma_multiply(0.45);
    visuals.hyperlink_color = p.accent;
    visuals.faint_bg_color = p.bg;
    visuals.extreme_bg_color = p.soft_btn;
    // Tooltip / popup contrast for both themes.
    visuals.popup_shadow = egui::Shadow {
        offset: [0, 4],
        blur: 12,
        spread: 0,
        color: Color32::from_black_alpha(if dark { 160 } else { 60 }),
    };
    visuals.warn_fg_color = p.accent;
    visuals.error_fg_color = p.error;
    // Ensure tooltip text stays readable if widgets inherit stroke colors.
    visuals.widgets.open.fg_stroke.color = p.tip_text;
    visuals.widgets.open.bg_fill = p.tip_bg;
    visuals
}

fn apply_theme(ctx: &egui::Context) {
    // Follow Windows light/dark setting.
    ctx.set_theme(ThemePreference::System);

    for theme in [Theme::Light, Theme::Dark] {
        let dark = theme == Theme::Dark;
        let mut style = (*ctx.style_of(theme)).clone();
        apply_fonts(&mut style);
        style.visuals = soft_visuals(dark);
        ctx.set_style_of(theme, style);
    }
}

fn format_status(status: &SpoolerStatus) -> String {
    status.state.as_ru_service_phrase().to_owned()
}

fn done_message(label: &str) -> String {
    let participle = match label {
        "Обновление" => "выполнено",
        "Остановка" | "Очистка очереди" | "Очистка и перезапуск" => {
            "выполнена"
        }
        _ => "выполнен", // Запуск, Перезапуск
    };
    format!("{label} {participle}.")
}

pub fn run() -> eframe::Result {
    run_inner(false, false, None)
}

/// Start GUI visible after UAC relaunch (`--show`), optionally at `--pos=x,y`.
pub fn run_after_elevate(pos: Option<(f32, f32)>) -> eframe::Result {
    run_inner(false, true, pos)
}

/// Start GUI and immediately hide to tray (Windows autostart / `--tray`).
pub fn run_start_in_tray() -> eframe::Result {
    run_inner(true, false, None)
}

fn run_inner(
    start_in_tray: bool,
    force_show_window: bool,
    window_pos: Option<(f32, f32)>,
) -> eframe::Result {
    match run_with_renderer(
        eframe::Renderer::Glow,
        start_in_tray,
        force_show_window,
        window_pos,
    ) {
        Ok(()) => Ok(()),
        Err(glow_err) => {
            log::warn(&format!(
                "OpenGL (Glow) недоступен: {glow_err}; пробуем DirectX/Vulkan (wgpu)"
            ));
            match run_with_renderer(
                eframe::Renderer::Wgpu,
                start_in_tray,
                force_show_window,
                window_pos,
            ) {
                Ok(()) => Ok(()),
                Err(wgpu_err) => {
                    log::error(&format!("wgpu тоже не запустился: {wgpu_err}"));
                    Err(wgpu_err)
                }
            }
        }
    }
}

fn run_with_renderer(
    renderer: eframe::Renderer,
    start_in_tray: bool,
    force_show_window: bool,
    window_pos: Option<(f32, f32)>,
) -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/printer-icon.png")).ok();

    let elevated = elevate::is_elevated();
    let title = if elevated {
        format!("SpoolCtl {APP_VERSION} (администратор)")
    } else {
        format!("SpoolCtl {APP_VERSION}")
    };

    // Compact start: header + watchdog + two action rows; rest scrolls.
    let start_h = if elevated { 620.0 } else { 660.0 };

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1080.0, start_h])
        .with_min_inner_size([900.0, 560.0])
        .with_title(title);
    if let Some((x, y)) = window_pos {
        viewport = viewport.with_position([x, y]);
        log::info(&format!("Позиция окна после UAC: ({x},{y})"));
    }
    if let Some(icon) = icon {
        viewport = viewport.with_icon(icon);
    }

    let renderer_name = match renderer {
        eframe::Renderer::Glow => "Glow/OpenGL",
        eframe::Renderer::Wgpu => "wgpu/DirectX",
    };
    log::info(&format!("Инициализация GUI ({renderer_name})"));

    let options = eframe::NativeOptions {
        viewport,
        renderer,
        ..Default::default()
    };

    eframe::run_native(
        "SpoolCtl",
        options,
        Box::new(move |cc| Ok(Box::new(SpoolCtlApp::new(cc, start_in_tray, force_show_window)))),
    )
}
