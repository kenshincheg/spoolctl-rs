//! Print Spooler service helpers via Win32 Service Control Manager.
//! Uses APIs available since Windows Vista/7.

use std::time::{Duration, Instant};

use windows::{
    Win32::{
        Foundation::{ERROR_ACCESS_DENIED, ERROR_SERVICE_DOES_NOT_EXIST},
        System::Services::{
            CloseServiceHandle, ControlService, OpenSCManagerW, OpenServiceW, QueryServiceStatus,
            SC_HANDLE, SC_MANAGER_CONNECT, SERVICE_CONTROL_STOP, SERVICE_QUERY_STATUS,
            SERVICE_START, SERVICE_STATUS, SERVICE_STATUS_CURRENT_STATE, SERVICE_STOP,
            StartServiceW,
        },
    },
    core::PCWSTR,
};

const SPOOLER_SERVICE_NAME: &str = "Spooler";

/// Default timeout for stop/start/restart waits.
pub const DEFAULT_CONTROL_TIMEOUT: Duration = Duration::from_secs(60);

/// Known Spooler states for display and control logic.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    Stopped,
    StartPending,
    StopPending,
    Running,
    ContinuePending,
    PausePending,
    Paused,
    Other(u32),
}

impl ServiceState {
    pub fn from_raw(value: SERVICE_STATUS_CURRENT_STATE) -> Self {
        match value.0 {
            1 => Self::Stopped,
            2 => Self::StartPending,
            3 => Self::StopPending,
            4 => Self::Running,
            5 => Self::ContinuePending,
            6 => Self::PausePending,
            7 => Self::Paused,
            other => Self::Other(other),
        }
    }

    pub fn as_ru_str(self) -> &'static str {
        match self {
            Self::Stopped => "остановлена",
            Self::StartPending => "запускается",
            Self::StopPending => "останавливается",
            Self::Running => "работает",
            Self::ContinuePending => "возобновляется",
            Self::PausePending => "приостанавливается",
            Self::Paused => "приостановлена",
            Self::Other(_) => "неизвестное состояние",
        }
    }

    /// Full phrase for UI, e.g. «служба печати работает».
    pub fn as_ru_service_phrase(self) -> &'static str {
        match self {
            Self::Stopped => "служба печати остановлена",
            Self::StartPending => "служба печати запускается",
            Self::StopPending => "служба печати останавливается",
            Self::Running => "служба печати работает",
            Self::ContinuePending => "служба печати возобновляется",
            Self::PausePending => "служба печати приостанавливается",
            Self::Paused => "служба печати приостановлена",
            Self::Other(_) => "служба печати в неизвестном состоянии",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpoolerStatus {
    pub state: ServiceState,
    pub win32_exit_code: u32,
    pub service_exit_code: u32,
    pub check_point: u32,
    pub wait_hint: Duration,
}

struct ServiceHandle {
    handle: SC_HANDLE,
}

impl ServiceHandle {
    fn open_manager() -> Result<Self, String> {
        // SAFETY: null machine/database opens the local SCM; rights are connect-only.
        let handle = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) }
            .map_err(|err| map_scm_error("Не удалось открыть Service Control Manager", err))?;
        Ok(Self { handle })
    }

    fn open_spooler(manager: &Self, access: u32) -> Result<Self, String> {
        let name = to_wide(SPOOLER_SERVICE_NAME);
        // SAFETY: manager handle is valid for this process; name is a NUL-terminated wide string.
        let handle = unsafe { OpenServiceW(manager.handle, PCWSTR(name.as_ptr()), access) }
            .map_err(|err| {
                if err.code().0 as u32 == ERROR_SERVICE_DOES_NOT_EXIST.0 {
                    "Служба печати Spooler не найдена на этой системе.".to_owned()
                } else {
                    map_scm_error("Не удалось открыть службу Spooler", err)
                }
            })?;
        Ok(Self { handle })
    }

    fn query_status(&self) -> Result<SpoolerStatus, String> {
        let mut status = SERVICE_STATUS::default();
        // SAFETY: service handle is valid; status is fully written by QueryServiceStatus on success.
        unsafe { QueryServiceStatus(self.handle, &mut status) }
            .map_err(|err| map_scm_error("Не удалось прочитать статус Spooler", err))?;

        Ok(SpoolerStatus {
            state: ServiceState::from_raw(status.dwCurrentState),
            win32_exit_code: status.dwWin32ExitCode,
            service_exit_code: status.dwServiceSpecificExitCode,
            check_point: status.dwCheckPoint,
            wait_hint: Duration::from_millis(u64::from(status.dwWaitHint)),
        })
    }
}

impl Drop for ServiceHandle {
    fn drop(&mut self) {
        // SAFETY: handle was opened by OpenSCManagerW/OpenServiceW in this process.
        let _ = unsafe { CloseServiceHandle(self.handle) };
    }
}

/// Reads the current Spooler service status. Does not change system state.
pub fn query_spooler_status() -> Result<SpoolerStatus, String> {
    let manager = ServiceHandle::open_manager()?;
    let service = ServiceHandle::open_spooler(&manager, SERVICE_QUERY_STATUS)?;
    service.query_status()
}

/// Stops the Spooler service and waits until it is stopped (or already stopped).
pub fn stop_spooler(timeout: Duration) -> Result<SpoolerStatus, String> {
    let manager = ServiceHandle::open_manager()?;
    let service = ServiceHandle::open_spooler(&manager, SERVICE_STOP | SERVICE_QUERY_STATUS)?;

    let status = service.query_status()?;
    if status.state == ServiceState::Stopped {
        return Ok(status);
    }

    let mut control_status = SERVICE_STATUS::default();
    // SAFETY: service opened with SERVICE_STOP; ControlService writes SERVICE_STATUS on success.
    unsafe { ControlService(service.handle, SERVICE_CONTROL_STOP, &mut control_status) }
        .map_err(|err| map_scm_error("Не удалось остановить Spooler", err))?;

    wait_until(&service, ServiceState::Stopped, timeout)
}

/// Starts the Spooler service and waits until it is running (or already running).
pub fn start_spooler(timeout: Duration) -> Result<SpoolerStatus, String> {
    let manager = ServiceHandle::open_manager()?;
    let service = ServiceHandle::open_spooler(&manager, SERVICE_START | SERVICE_QUERY_STATUS)?;

    let status = service.query_status()?;
    if status.state == ServiceState::Running {
        return Ok(status);
    }
    if status.state == ServiceState::StartPending {
        return wait_until(&service, ServiceState::Running, timeout);
    }
    if status.state != ServiceState::Stopped && status.state != ServiceState::Paused {
        return Err(format!(
            "Нельзя запустить Spooler из состояния «{}».",
            status.state.as_ru_str()
        ));
    }

    // SAFETY: service opened with SERVICE_START; no start args for Spooler.
    unsafe { StartServiceW(service.handle, None) }
        .map_err(|err| map_scm_error("Не удалось запустить Spooler", err))?;

    wait_until(&service, ServiceState::Running, timeout)
}

/// Restarts Spooler: stop (if needed) then start, waiting for each target state.
pub fn restart_spooler(timeout: Duration) -> Result<SpoolerStatus, String> {
    let half = timeout / 2;
    let stop_timeout = if half.is_zero() { timeout } else { half };
    let start_timeout = timeout
        .saturating_sub(stop_timeout)
        .max(Duration::from_secs(1));

    let after_stop = stop_spooler(stop_timeout)?;
    if after_stop.state != ServiceState::Stopped {
        return Err(format!(
            "После остановки ожидалось «остановлена», получено «{}».",
            after_stop.state.as_ru_str()
        ));
    }
    start_spooler(start_timeout)
}

fn wait_until(
    service: &ServiceHandle,
    target: ServiceState,
    timeout: Duration,
) -> Result<SpoolerStatus, String> {
    let deadline = Instant::now() + timeout;
    let mut status = service.query_status()?;
    if status.state == target {
        return Ok(status);
    }

    let mut last_checkpoint = status.check_point;
    let mut progress_deadline = Instant::now() + progress_window(status.wait_hint);

    loop {
        if Instant::now() >= deadline {
            return Err(format!(
                "Таймаут ожидания состояния «{}» (сейчас: «{}»).",
                target.as_ru_str(),
                status.state.as_ru_str()
            ));
        }

        std::thread::sleep(poll_interval(status.wait_hint));
        status = service.query_status()?;
        if status.state == target {
            return Ok(status);
        }

        if status.check_point > last_checkpoint {
            last_checkpoint = status.check_point;
            progress_deadline = Instant::now() + progress_window(status.wait_hint);
        } else if Instant::now() >= progress_deadline {
            return Err(format!(
                "Spooler не продвигается к состоянию «{}» (сейчас: «{}»).",
                target.as_ru_str(),
                status.state.as_ru_str()
            ));
        }
    }
}

/// Poll sleep derived from wait_hint (MSDN: hint/10, clamped 1–10 s).
fn poll_interval(wait_hint: Duration) -> Duration {
    let tenth = wait_hint / 10;
    tenth.clamp(Duration::from_secs(1), Duration::from_secs(10))
}

fn progress_window(wait_hint: Duration) -> Duration {
    wait_hint.max(Duration::from_secs(1))
}

fn map_scm_error(prefix: &str, err: windows::core::Error) -> String {
    if err.code().0 as u32 == ERROR_ACCESS_DENIED.0 {
        format!("{prefix}: отказано в доступе. Запустите от имени администратора.")
    } else {
        format!("{prefix}: {err}")
    }
}

fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn service_state_maps_known_codes() {
        assert_eq!(
            ServiceState::from_raw(SERVICE_STATUS_CURRENT_STATE(4)).as_ru_str(),
            "работает"
        );
        assert_eq!(
            ServiceState::from_raw(SERVICE_STATUS_CURRENT_STATE(1)).as_ru_str(),
            "остановлена"
        );
        assert_eq!(
            ServiceState::from_raw(SERVICE_STATUS_CURRENT_STATE(4)).as_ru_service_phrase(),
            "служба печати работает"
        );
    }

    #[test]
    fn poll_interval_clamps_to_one_ten_seconds() {
        assert_eq!(
            poll_interval(Duration::from_millis(0)),
            Duration::from_secs(1)
        );
        assert_eq!(
            poll_interval(Duration::from_secs(30)),
            Duration::from_secs(3)
        );
        assert_eq!(
            poll_interval(Duration::from_secs(200)),
            Duration::from_secs(10)
        );
    }

    #[test]
    fn spooler_status_query_runs_on_this_machine() {
        let status = query_spooler_status().expect("Spooler should exist on a desktop Windows");
        assert!(matches!(
            status.state,
            ServiceState::Running
                | ServiceState::Stopped
                | ServiceState::StartPending
                | ServiceState::StopPending
                | ServiceState::Paused
                | ServiceState::Other(_)
        ));
    }
}
