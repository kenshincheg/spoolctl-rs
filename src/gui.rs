//! Simple egui window for Spooler status and control (Win10/11).

use std::sync::mpsc::{self, Receiver};
use std::thread;
use std::time::{Duration, Instant};

use eframe::egui::{self, Color32, FontId, RichText, TextStyle, Theme, ThemePreference, Visuals};

use crate::elevate;
use crate::log;
use crate::ncd;
use crate::queue::{self, ClearReport, PrinterEntry};
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
            // Soft daylight blues (kept from previous look).
            Self {
                bg: Color32::from_rgb(230, 242, 255),
                panel: Color32::from_rgb(214, 234, 252),
                text: Color32::from_rgb(18, 52, 86),
                accent: Color32::from_rgb(37, 99, 160),
                accent_hover: Color32::from_rgb(56, 130, 200),
                accent_active: Color32::from_rgb(25, 80, 140),
                on_accent: Color32::from_rgb(245, 250, 255),
                soft_btn: Color32::from_rgb(190, 220, 245),
                util_btn: Color32::from_rgb(56, 142, 100),
                on_util: Color32::from_rgb(245, 255, 248),
                error: Color32::from_rgb(170, 35, 45),
                ok: Color32::from_rgb(20, 110, 60),
                tip_bg: Color32::from_rgb(245, 250, 255),
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
}

enum JobResult {
    Done {
        label: &'static str,
        status: SpoolerStatus,
        report: Option<ClearReport>,
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
    /// Last known Spooler state for the hang watchdog.
    last_service_state: Option<spooler::ServiceState>,
    tray: Option<AppTray>,
    /// Real exit (from tray «Выход»); otherwise close hides to tray.
    allow_exit: bool,
    window_in_tray: bool,
}

impl SpoolCtlApp {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
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
            last_service_state: None,
            tray: None,
            allow_exit: false,
            window_in_tray: false,
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
        app.refresh_log_view();
        app.apply_status_query();
        app
    }

    fn refresh_ncd_summary(&mut self) {
        self.ncd_summary = ncd::query_status().summary_ru();
    }

    fn refresh_log_view(&mut self) {
        self.log_lines = log::read_tail(12);
    }

    fn refresh_queue_line(&mut self) {
        let snap = queue::queue_snapshot();
        self.queue_line = snap.line_ru();
        self.printers = snap.printers;
    }

    fn apply_status(&mut self, status: &SpoolerStatus) {
        self.status_line = format_status(status);
        self.prompt_start = status.state == spooler::ServiceState::Stopped;
        self.last_service_state = Some(status.state);
        self.refresh_queue_line();
    }

    /// Quiet status poll for the idle timer (does not touch the message line).
    fn quiet_status_refresh(&mut self) {
        if let Ok(status) = spooler::query_spooler_status() {
            self.apply_status(&status);
        } else {
            self.refresh_queue_line();
        }
        self.last_auto_status = Instant::now();
        self.run_watchdog_tick();
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

    fn apply_status_query(&mut self) {
        match spooler::query_spooler_status() {
            Ok(status) => {
                self.apply_status(&status);
                self.message = "Статус обновлён.".to_owned();
                self.message_is_error = false;
            }
            Err(error) => {
                self.status_line = "служба печати недоступна".to_owned();
                self.refresh_queue_line();
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
        self.busy = true;
        self.message_is_error = false;
        self.message = match job {
            Job::Refresh => "Обновление статуса…",
            Job::Stop => "Остановка Spooler…",
            Job::Start => "Запуск Spooler…",
            Job::Restart => "Перезапуск Spooler…",
            Job::ClearOnly => "Очистка очереди…",
            Job::FixClear => "Очистка очереди и перезапуск…",
        }
        .to_owned();
        log::info(&format!("{}: начало", self.message.trim_end_matches('…')));

        let (tx, rx) = mpsc::channel();
        self.result_rx = Some(rx);

        thread::spawn(move || {
            let timeout = spooler::DEFAULT_CONTROL_TIMEOUT;
            let result = match job {
                Job::Refresh => match spooler::query_spooler_status() {
                    Ok(status) => JobResult::Done {
                        label: "Обновление",
                        status,
                        report: None,
                    },
                    Err(error) => JobResult::Failed {
                        label: "Обновление",
                        error,
                    },
                },
                Job::Stop => match spooler::stop_spooler(timeout) {
                    Ok(status) => JobResult::Done {
                        label: "Остановка",
                        status,
                        report: None,
                    },
                    Err(error) => JobResult::Failed {
                        label: "Остановка",
                        error,
                    },
                },
                Job::Start => match spooler::start_spooler(timeout) {
                    Ok(status) => JobResult::Done {
                        label: "Запуск",
                        status,
                        report: None,
                    },
                    Err(error) => JobResult::Failed {
                        label: "Запуск",
                        error,
                    },
                },
                Job::Restart => match spooler::restart_spooler(timeout) {
                    Ok(status) => JobResult::Done {
                        label: "Перезапуск",
                        status,
                        report: None,
                    },
                    Err(error) => JobResult::Failed {
                        label: "Перезапуск",
                        error,
                    },
                },
                Job::ClearOnly => match queue::clear_queue(timeout) {
                    Ok((report, status)) => JobResult::Done {
                        label: "Очистка очереди",
                        status,
                        report: Some(report),
                    },
                    Err(error) => JobResult::Failed {
                        label: "Очистка очереди",
                        error,
                    },
                },
                Job::FixClear => match queue::fix_queue(timeout) {
                    Ok((report, status)) => JobResult::Done {
                        label: "Очистка и перезапуск",
                        status,
                        report: Some(report),
                    },
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
            }) => {
                self.apply_status(&status);
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

    fn request_admin_relaunch(&mut self) {
        log::info("Запрос запуска от имени администратора");
        // Release single-instance mutex first; otherwise the elevated copy sees
        // this process still holding it, "activates" this closing window, and exits.
        single_instance::release_gui();
        match elevate::relaunch_elevated() {
            Ok(()) => {
                log::info("UAC принят, закрытие текущего окна");
                std::process::exit(0);
            }
            Err(error) => {
                log::error(&error);
                // UAC cancelled or failed — take the GUI slot again if possible.
                let _ = single_instance::try_acquire_gui();
                self.message = error;
                self.message_is_error = true;
                self.refresh_log_view();
            }
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
        self.poll_tray(ctx);

        if ctx.input(|i| i.viewport().close_requested()) {
            if AppTray::take_allow_exit() {
                self.allow_exit = true;
            }
            if self.allow_exit || self.tray.is_none() {
                // Let the window close and the process exit.
            } else {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                self.hide_to_tray();
            }
        }

        self.poll_job(ctx);
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
                    }
                });

                ui.add_space(6.0);
                ui.label(
                    RichText::new(&self.status_line)
                        .size(20.0)
                        .color(p.accent)
                        .strong(),
                );
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
                    if !self.elevated
                        && themed_button(
                            ui,
                            "Запустить от имени администратора",
                            "Откроет окно UAC и перезапустит SpoolCtl с правами администратора. Нужно для остановки и перезапуска службы.",
                            p.accent,
                            p.on_accent,
                            40.0,
                            true,
                        )
                        .clicked()
                    {
                        self.request_admin_relaunch();
                    }
                    if !self.elevated {
                        ui.add_space(10.0);
                    }

                    if themed_button(
                        ui,
                        "Обновить статус службы",
                        "Запрашивает текущее состояние службы печати Spooler без изменений.",
                        p.soft_btn,
                        p.text,
                        40.0,
                        true,
                    )
                    .clicked()
                    {
                        self.start_job(Job::Refresh);
                    }

                    ui.add_space(8.0);
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
                    if themed_button(
                        ui,
                        "Запустить службу",
                        start_tip,
                        start_fill,
                        start_text,
                        40.0,
                        true,
                    )
                    .clicked()
                    {
                        self.start_job(Job::Start);
                    }

                    ui.add_space(8.0);
                    if themed_button(
                        ui,
                        "Остановить службу",
                        "Останавливает службу Spooler. Печать станет недоступна, пока службу не запустят снова.",
                        p.soft_btn,
                        p.text,
                        40.0,
                        true,
                    )
                    .clicked()
                    {
                        self.confirm = Some(Confirm::Job(Job::Stop));
                    }

                    ui.add_space(8.0);
                    if themed_button(
                        ui,
                        "Перезапустить службу",
                        "Останавливает и снова запускает службу печати. Помогает при зависании очереди. Нужны права администратора.",
                        p.accent_active,
                        p.on_accent,
                        42.0,
                        true,
                    )
                    .clicked()
                    {
                        self.confirm = Some(Confirm::Job(Job::Restart));
                    }

                    ui.add_space(8.0);
                    if themed_button(
                        ui,
                        "Очистить очередь печати",
                        "Останавливает Spooler и удаляет файлы заданий в spool\\PRINTERS. Службу нужно запустить отдельно.",
                        p.soft_btn,
                        p.text,
                        44.0,
                        true,
                    )
                    .clicked()
                    {
                        self.confirm = Some(Confirm::Job(Job::ClearOnly));
                    }

                    ui.add_space(8.0);
                    if themed_button(
                        ui,
                        "Очистить очередь печати и перезапустить службу",
                        "Останавливает Spooler, удаляет файлы заданий в spool\\PRINTERS, затем запускает службу снова. Удалит текущие задания печати.",
                        p.accent,
                        p.on_accent,
                        48.0,
                        true,
                    )
                    .clicked()
                    {
                        self.confirm = Some(Confirm::Job(Job::FixClear));
                    }
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
                            }
                        }
                    });
                });
        }
    }
}

const BUTTON_FONT_SIZE: f32 = 16.0;

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

fn themed_button(
    ui: &mut egui::Ui,
    label: &str,
    tip: &str,
    fill: Color32,
    text_color: Color32,
    height: f32,
    bold: bool,
) -> egui::Response {
    themed_button_sized(
        ui,
        [ui.available_width(), height],
        label,
        tip,
        fill,
        text_color,
        bold,
    )
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
    let text = RichText::new(label)
        .font(FontId::proportional(14.0))
        .color(text_color)
        .strong();
    ui.add_sized(
        size,
        egui::Button::new(text)
            .fill(fill)
            .wrap_mode(egui::TextWrapMode::Truncate),
    )
    .on_hover_text(
        RichText::new(tip)
            .font(FontId::proportional(BUTTON_FONT_SIZE))
            .color(tip_color),
    )
    .clicked()
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
    visuals.widgets.inactive.bg_fill = p.soft_btn;
    visuals.widgets.inactive.weak_bg_fill = p.soft_btn;
    visuals.widgets.inactive.fg_stroke.color = p.text;
    visuals.widgets.hovered.bg_fill = p.accent_hover;
    visuals.widgets.hovered.weak_bg_fill = p.accent_hover;
    visuals.widgets.hovered.fg_stroke.color = p.on_accent;
    visuals.widgets.active.bg_fill = p.accent_active;
    visuals.widgets.active.weak_bg_fill = p.accent_active;
    visuals.widgets.active.fg_stroke.color = p.on_accent;
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
    // Prefer OpenGL (Glow); many Win10 PCs with broken/outdated GL drivers need DirectX via wgpu.
    match run_with_renderer(eframe::Renderer::Glow) {
        Ok(()) => Ok(()),
        Err(glow_err) => {
            log::warn(&format!(
                "OpenGL (Glow) недоступен: {glow_err}; пробуем DirectX/Vulkan (wgpu)"
            ));
            match run_with_renderer(eframe::Renderer::Wgpu) {
                Ok(()) => Ok(()),
                Err(wgpu_err) => {
                    log::error(&format!("wgpu тоже не запустился: {wgpu_err}"));
                    Err(wgpu_err)
                }
            }
        }
    }
}

fn run_with_renderer(renderer: eframe::Renderer) -> eframe::Result {
    let icon = eframe::icon_data::from_png_bytes(include_bytes!("../assets/printer-icon.png")).ok();

    let elevated = elevate::is_elevated();
    let title = if elevated {
        format!("SpoolCtl {APP_VERSION} (администратор)")
    } else {
        format!("SpoolCtl {APP_VERSION}")
    };

    // Compact start height: up to «Очистить очередь… и перезапустить»; остальное — скролл.
    let start_h = if elevated { 640.0 } else { 700.0 };

    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1080.0, start_h])
        .with_min_inner_size([900.0, 560.0])
        .with_title(title);
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
        Box::new(|cc| Ok(Box::new(SpoolCtlApp::new(cc)))),
    )
}
