#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod elevate;
mod gui;
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
    if args.is_empty() {
        if !gui_supported() {
            notify_gui_unsupported();
            return;
        }
        if !single_instance::try_acquire_gui() {
            log::info("GUI уже запущен — активировано существующее окно");
            return;
        }
        log::info("Запуск GUI");
        if let Err(error) = gui::run() {
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
    let command = args[0].as_str();
    match command {
        "status" | "--status" => run_status(),
        "stop" => run_control("Остановка", spooler::stop_spooler),
        "start" => run_control("Запуск", spooler::start_spooler),
        "restart" => run_control("Перезапуск", spooler::restart_spooler),
        "clear-queue" => run_clear(false),
        "fix" => run_clear(true),
        "version" | "--version" | "-V" => print_version(),
        "help" | "--help" | "-h" => print_help(),
        other => {
            eprintln!("Неизвестная команда: {other}");
            eprintln!("Используйте: spoolctl help");
            std::process::exit(2);
        }
    }
}

fn run_status() {
    match spooler::query_spooler_status() {
        Ok(status) => {
            log::info(&format!("Статус: {}", status.state.as_ru_str()));
            print_status(&status);
        }
        Err(error) => {
            log::error(&error);
            eprintln!("Ошибка: {error}");
            std::process::exit(1);
        }
    }
}

fn run_control(label: &str, action: fn(Duration) -> Result<spooler::SpoolerStatus, String>) {
    println!("{label} службы печати (Spooler)...");
    log::info(&format!("{label}: начало"));
    match action(spooler::DEFAULT_CONTROL_TIMEOUT) {
        Ok(status) => {
            log::info(&format!("{label}: ок ({})", status.state.as_ru_str()));
            println!("{}", done_message_cli(label));
            print_status(&status);
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

fn run_clear(restart_after: bool) {
    let label = if restart_after {
        "Очистка очереди и перезапуск"
    } else {
        "Очистка очереди"
    };
    println!("{label}...");
    log::info(&format!("{label}: начало"));
    let timeout = spooler::DEFAULT_CONTROL_TIMEOUT;
    let result = if restart_after {
        queue::fix_queue(timeout)
    } else {
        queue::clear_queue(timeout)
    };
    match result {
        Ok((report, status)) => {
            let summary = report.summary_ru();
            if report.failed.is_empty() {
                log::info(&format!(
                    "{label}: {summary}; служба «{}»",
                    status.state.as_ru_str()
                ));
            } else {
                log::warn(&format!(
                    "{label}: {summary}; служба «{}»",
                    status.state.as_ru_str()
                ));
            }
            println!("{summary}");
            if !report.failed.is_empty() {
                eprintln!("Предупреждение: часть файлов не удалена.");
            }
            print_status(&status);
            if !restart_after {
                println!("Spooler оставлен остановленным. Запуск: spoolctl start");
            }
        }
        Err(error) => {
            log::error(&format!("{label}: {error}"));
            eprintln!("Ошибка: {error}");
            std::process::exit(1);
        }
    }
}

fn print_status(status: &spooler::SpoolerStatus) {
    println!("Служба печати (Spooler): {}", status.state.as_ru_str());
    let snap = queue::queue_snapshot();
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
    println!("  spoolctl status       показать статус Spooler");
    println!("  spoolctl stop         остановить Spooler (нужен администратор)");
    println!("  spoolctl start        запустить Spooler (нужен администратор)");
    println!("  spoolctl restart      перезапустить Spooler (нужен администратор)");
    println!("  spoolctl clear-queue  остановить и очистить очередь (без запуска)");
    println!("  spoolctl fix          очистить очередь и перезапустить Spooler");
    println!("  spoolctl version      показать версию");
    println!("  spoolctl help         эта справка");
    println!();
    println!("Журнал: рядом с EXE в logs\\spoolctl.log (или %LOCALAPPDATA%\\SpoolCtl\\logs).");
}
