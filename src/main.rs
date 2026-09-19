#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod autostart;
mod elevate;
mod gui;
mod host;
mod log;
mod ncd;
mod os;
mod queue;
mod single_instance;
mod spooler;
mod tray;
mod watchdog;

use std::env;
use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::time::Duration;

use windows::{
    Win32::{
        System::Console::{ATTACH_PARENT_PROCESS, AllocConsole, AttachConsole},
        UI::WindowsAndMessaging::{MB_ICONINFORMATION, MB_OK, MessageBoxW},
    },
    core::PCWSTR,
};

fn main() {
    let args: Vec<String> = env::args().skip(1).collect();
    if args.is_empty() || is_gui_flag(&args) {
        let start_in_tray = is_tray_launch(&args);
        let after_elevate = is_show_launch(&args);
        if !gui_supported() {
            notify_gui_unsupported();
            return;
        }
        let acquired = if after_elevate {
            single_instance::try_acquire_gui_after_relaunch()
        } else {
            single_instance::try_acquire_gui()
        };
        if !acquired {
            log::info("GUI уже запущен — активировано существующее окно");
            return;
        }
        log::info(if start_in_tray {
            "Запуск GUI в трее"
        } else if after_elevate {
            "Запуск GUI после UAC (окно на передний план)"
        } else {
            "Запуск GUI"
        });
        let result = if start_in_tray {
            gui::run_start_in_tray()
        } else if after_elevate {
            gui::run_after_elevate()
        } else {
            gui::run()
        };
        if let Err(error) = result {
            log::error(&format!("Ошибка GUI: {error}"));
            single_instance::release_gui();
            notify_gui_graphics_failed(&error.to_string());
            std::process::exit(1);
        }
        return;
    }

    ensure_cli_console();
    log::info(&format!("CLI: {}", args.join(" ")));
    run_cli(&args);
}

fn is_gui_flag(args: &[String]) -> bool {
    is_tray_launch(args) || is_show_launch(args)
}

fn is_tray_launch(args: &[String]) -> bool {
    matches!(
        args.first().map(String::as_str),
        Some("--tray" | "tray" | "/tray")
    )
}

fn is_show_launch(args: &[String]) -> bool {
    matches!(
        args.first().map(String::as_str),
        Some("--show" | "show" | "/show")
    )
}

fn gui_supported() -> bool {
    os::current().is_none_or(|ver| ver.is_windows_10_or_newer())
}

fn notify_gui_unsupported() {
    let ver = os::current()
        .map(|v| v.display())
        .unwrap_or_else(|| "неизвестна".to_owned());
    log::warn(&format!(
        "GUI недоступен на этой ОС ({ver}); используйте CLI"
    ));
    let text = format!(
        "Графический интерфейс SpoolCtl рассчитан на Windows 10 / 11.\n\n\
         Обнаружена ОС: {ver}\n\n\
         На Windows 7 используйте командную строку:\n\
         SpoolCtl.exe status\n\
         SpoolCtl.exe restart\n\
         SpoolCtl.exe fix\n\n\
         Или папку Windows7-CLI (status.cmd / restart.cmd / fix.cmd)."
    );
    show_message_box("SpoolCtl", &text);
}

fn notify_gui_graphics_failed(detail: &str) {
    let text = format!(
        "Не удалось открыть окно SpoolCtl (графика).\n\n\
         {detail}\n\n\
         Обновите драйвер видеокарты или используйте командную строку:\n\
         SpoolCtl.exe status\n\
         SpoolCtl.exe restart\n\
         SpoolCtl.exe fix\n\n\
         Журнал: папка logs рядом с EXE."
    );
    show_message_box("SpoolCtl — ошибка GUI", &text);
}

fn show_message_box(title: &str, text: &str) {
    let title_w = to_wide(OsStr::new(title));
    let body_w = to_wide(OsStr::new(text));
    // SAFETY: NUL-terminated wide strings for MessageBoxW.
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(body_w.as_ptr()),
            PCWSTR(title_w.as_ptr()),
            MB_OK | MB_ICONINFORMATION,
        );
    }
}

fn to_wide(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn ensure_cli_console() {
    // Prefer the parent terminal (PowerShell/cmd); otherwise allocate one.
    // SAFETY: attaching/allocating a console for this process only.
    let attached = unsafe { AttachConsole(ATTACH_PARENT_PROCESS) }.is_ok();
    if !attached {
        let _ = unsafe { AllocConsole() };
    }
}

fn run_cli(args: &[String]) {
    let (host, rest) = match parse_host_args(args) {
        Ok(value) => value,
        Err(error) => {
            eprintln!("{error}");
            std::process::exit(2);
        }
    };
    if rest.is_empty() {
        eprintln!("Укажите команду после --host (например: status).");
        eprintln!("Используйте: spoolctl help");
        std::process::exit(2);
    }
    let command = rest[0].as_str();
    match command {
        "status" | "--status" => run_status(&host),
        "stop" => run_control(&host, "Остановка", spooler::stop_spooler_on),
        "start" => run_control(&host, "Запуск", spooler::start_spooler_on),
        "restart" => run_control(&host, "Перезапуск", spooler::restart_spooler_on),
        "clear-queue" => run_clear(&host, false),
        "fix" => run_clear(&host, true),
        "version" | "--version" | "-V" => print_version(),
        "help" | "--help" | "-h" => print_help(),
        other => {
            eprintln!("Неизвестная команда: {other}");
            eprintln!("Используйте: spoolctl help");
            std::process::exit(2);
        }
    }
}

/// Splits `--host NAME` (or `/host`) from the rest of CLI args.
fn parse_host_args(args: &[String]) -> Result<(host::Host, Vec<String>), String> {
    let mut host = host::Host::local();
    let mut rest = Vec::new();
    let mut i = 0;
    while i < args.len() {
        let a = args[i].as_str();
        if a == "--host" || a == "/host" || a == "-H" {
            let Some(value) = args.get(i + 1) else {
                return Err("После --host нужно имя ПК или IP.".to_owned());
            };
            host = host::Host::parse(value)?;
            i += 2;
            continue;
        }
        if let Some(value) = a.strip_prefix("--host=") {
            host = host::Host::parse(value)?;
            i += 1;
            continue;
        }
        rest.push(args[i].clone());
        i += 1;
    }
    Ok((host, rest))
}

fn run_status(host: &host::Host) {
    match spooler::query_spooler_status_on(host) {
        Ok(status) => {
            log::info(&format!(
                "Статус ({}): {}",
                host.label_ru(),
                status.state.as_ru_str()
            ));
            print_status(host, &status);
        }
        Err(error) => {
            log::error(&error);
            eprintln!("Ошибка: {error}");
            std::process::exit(1);
        }
    }
}

fn run_control(
    host: &host::Host,
    label: &str,
    action: fn(&host::Host, Duration) -> Result<spooler::SpoolerStatus, String>,
) {
    println!(
        "{label} службы печати (Spooler) на {}...",
        host.label_ru()
    );
    log::info(&format!("{label} ({}): начало", host.label_ru()));
    match action(host, spooler::DEFAULT_CONTROL_TIMEOUT) {
        Ok(status) => {
            log::info(&format!(
                "{label} ({}): ок ({})",
                host.label_ru(),
                status.state.as_ru_str()
            ));
            println!("{}", done_message_cli(label));
            print_status(host, &status);
        }
        Err(error) => {
            log::error(&format!("{label}: {error}"));
            eprintln!("Ошибка: {error}");
            std::process::exit(1);
        }
    }
}

fn done_message_cli(label: &str) -> String {
    let participle = match label {
        "Остановка" => "выполнена",
        "Запуск" | "Перезапуск" => "выполнен",
        _ => "выполнено",
    };
    format!("{label} {participle}.")
}

fn run_clear(host: &host::Host, restart_after: bool) {
    let label = if restart_after {
        "Очистка очереди и перезапуск"
    } else {
        "Очистка очереди"
    };
    println!("{label} на {}...", host.label_ru());
    log::info(&format!("{label} ({}): начало", host.label_ru()));
    let timeout = spooler::DEFAULT_CONTROL_TIMEOUT;
    let result = if restart_after {
        queue::fix_queue_on(host, timeout)
    } else {
        queue::clear_queue_on(host, timeout)
    };
    match result {
        Ok((report, status)) => {
            let summary = report.summary_ru();
            if report.failed.is_empty() {
                log::info(&format!(
                    "{label} ({}): {summary}; служба «{}»",
                    host.label_ru(),
                    status.state.as_ru_str()
                ));
            } else {
                log::warn(&format!(
                    "{label} ({}): {summary}; служба «{}»",
                    host.label_ru(),
                    status.state.as_ru_str()
                ));
            }
            println!("{summary}");
            if !report.failed.is_empty() {
                eprintln!("Предупреждение: часть файлов не удалена.");
            }
            print_status(host, &status);
            if !restart_after {
                println!(
                    "Spooler оставлен остановленным. Запуск: spoolctl --host {} start",
                    if host.is_local() {
                        "<этот ПК>".to_owned()
                    } else {
                        host.name().to_owned()
                    }
                );
            }
        }
        Err(error) => {
            log::error(&format!("{label}: {error}"));
            eprintln!("Ошибка: {error}");
            std::process::exit(1);
        }
    }
}

fn print_status(host: &host::Host, status: &spooler::SpoolerStatus) {
    println!(
        "ПК: {} | Служба печати (Spooler): {}",
        host.label_ru(),
        status.state.as_ru_str()
    );
    let snap = queue::queue_snapshot_on(host);
    println!("{}", snap.line_ru());
    if snap.printers.is_empty() {
        println!("Принтеры: нет или список недоступен");
    } else {
        println!("Принтеры:");
        for printer in &snap.printers {
            println!("  - {}", printer.line_ru());
        }
    }
    if matches!(
        status.state,
        spooler::ServiceState::StartPending
            | spooler::ServiceState::StopPending
            | spooler::ServiceState::ContinuePending
            | spooler::ServiceState::PausePending
    ) {
        println!(
            "Ожидание: ~{} мс (checkpoint {})",
            status.wait_hint.as_millis(),
            status.check_point
        );
    }
}

fn print_version() {
    println!("SpoolCtl {}", env!("CARGO_PKG_VERSION"));
}

fn print_help() {
    println!(
        "SpoolCtl {} — управление службой печати Windows",
        env!("CARGO_PKG_VERSION")
    );
    println!();
    println!("Использование:");
    println!("  spoolctl              открыть графический интерфейс");
    println!("  spoolctl --tray       GUI сразу в системный трей");
    println!("  spoolctl status       показать статус Spooler");
    println!("  spoolctl --host ПК status   статус на удалённом ПК (без агента)");
    println!("  spoolctl stop         остановить Spooler (нужен администратор)");
    println!("  spoolctl start        запустить Spooler (нужен администратор)");
    println!("  spoolctl restart      перезапустить Spooler (нужен администратор)");
    println!("  spoolctl clear-queue  остановить и очистить очередь (без запуска)");
    println!("  spoolctl fix          очистить очередь и перезапустить Spooler");
    println!("  spoolctl version      показать версию");
    println!("  spoolctl help         эта справка");
    println!();
    println!("Удалёнка: docs\\REMOTE.md — SCM + ADMIN$, агент на том ПК не нужен.");
    println!();
    println!("Журнал: рядом с EXE в logs\\spoolctl.log (или %LOCALAPPDATA%\\SpoolCtl\\logs).");
}
