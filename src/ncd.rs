//! Disable Windows «Network Connected Devices Auto-Setup» (NcdAutoSetup).
//! Matches the Settings toggle that auto-discovers printers via WSD/IPP.

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;

use windows::{
    Win32::{
        Foundation::{ERROR_SERVICE_DOES_NOT_EXIST, WIN32_ERROR},
        System::{
            Registry::{
                HKEY_LOCAL_MACHINE, KEY_READ, KEY_WRITE, REG_DWORD, REG_OPTION_NON_VOLATILE,
                RegCloseKey, RegCreateKeyExW, RegOpenKeyExW, RegQueryValueExW, RegSetValueExW,
            },
            Services::{
                ChangeServiceConfigW, CloseServiceHandle, ControlService, ENUM_SERVICE_TYPE,
                OpenSCManagerW, OpenServiceW, QueryServiceStatus, SC_HANDLE, SC_MANAGER_CONNECT,
                SERVICE_CHANGE_CONFIG, SERVICE_CONTROL_STOP, SERVICE_DISABLED, SERVICE_ERROR,
                SERVICE_NO_CHANGE, SERVICE_QUERY_CONFIG, SERVICE_QUERY_STATUS, SERVICE_STATUS,
                SERVICE_STOP,
            },
        },
    },
    core::PCWSTR,
};

const SERVICE_NAME: &str = "NcdAutoSetup";
const REG_ROOT: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\NcdAutoSetup";
const REG_PRIVATE: &str = r"SOFTWARE\Microsoft\Windows\CurrentVersion\NcdAutoSetup\Private";

/// Snapshot of auto-setup related settings (best-effort).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NcdStatus {
    /// `AutoSetup` under `...\NcdAutoSetup\Private` (None = value missing).
    pub private_autosetup: Option<u32>,
    /// `GlobalAutoSetup` under `...\NcdAutoSetup` (None = value missing).
    pub global_autosetup: Option<u32>,
    /// Service start type if the service exists.
    pub service_disabled: Option<bool>,
    pub service_running: Option<bool>,
}

impl NcdStatus {
    pub fn summary_ru(&self) -> String {
        let private = match self.private_autosetup {
            Some(0) => "выкл.",
            Some(_) => "вкл.",
            None => "не задано (обычно вкл.)",
        };
        let service = match (self.service_disabled, self.service_running) {
            (Some(true), _) => "служба отключена",
            (Some(false), Some(true)) => "служба работает",
            (Some(false), Some(false)) => "служба остановлена",
            _ => "служба неизвестна",
        };
        format!("Автонастройка устройств: {private}; {service}")
    }
}

/// Read current registry / service state (does not require admin for query of existing values,
/// but missing keys are treated as unknown/enabled).
pub fn query_status() -> NcdStatus {
    NcdStatus {
        private_autosetup: read_dword(REG_PRIVATE, "AutoSetup"),
        global_autosetup: read_dword(REG_ROOT, "GlobalAutoSetup"),
        service_disabled: service_is_disabled().ok(),
        service_running: service_is_running().ok(),
    }
}

/// Disable automatic setup of network-connected devices.
/// Requires administrator rights. Reboot is recommended afterward.
pub fn disable_autosetup() -> Result<String, String> {
    set_dword(REG_PRIVATE, "AutoSetup", 0)?;
    // Broader kill-switch used by some deployments (optional but harmless).
    let _ = set_dword(REG_ROOT, "GlobalAutoSetup", 0);

    let mut notes = Vec::new();
    notes.push("Реестр: AutoSetup=0 (Private)".to_owned());
    if read_dword(REG_ROOT, "GlobalAutoSetup") == Some(0) {
        notes.push("Реестр: GlobalAutoSetup=0".to_owned());
    }

    match stop_and_disable_service() {
        Ok(msg) => notes.push(msg),
        Err(error) => notes.push(format!("Служба NcdAutoSetup: {error}")),
    }

    notes.push("Рекомендуется перезагрузка Windows.".to_owned());
    Ok(notes.join(" "))
}

fn to_wide(value: &str) -> Vec<u16> {
    OsStr::new(value)
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

fn read_dword(subkey: &str, name: &str) -> Option<u32> {
    let subkey_w = to_wide(subkey);
    let name_w = to_wide(name);
    let mut key = Default::default();
    // SAFETY: NUL-terminated registry path.
    let open = unsafe {
        RegOpenKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey_w.as_ptr()),
            Some(0),
            KEY_READ,
            &mut key,
        )
    };
    if open != WIN32_ERROR(0) {
        return None;
    }
    let mut ty = REG_DWORD;
    let mut data = [0u8; 4];
    let mut size = 4u32;
    // SAFETY: key opened above; buffer sized for DWORD.
    let query = unsafe {
        RegQueryValueExW(
            key,
            PCWSTR(name_w.as_ptr()),
            None,
            Some(&mut ty),
            Some(data.as_mut_ptr()),
            Some(&mut size),
        )
    };
    let _ = unsafe { RegCloseKey(key) };
    if query != WIN32_ERROR(0) || size < 4 {
        return None;
    }
    Some(u32::from_le_bytes(data))
}

fn set_dword(subkey: &str, name: &str, value: u32) -> Result<(), String> {
    let subkey_w = to_wide(subkey);
    let name_w = to_wide(name);
    let mut key = Default::default();
    let mut disposition = Default::default();
    // SAFETY: create/open HKLM subkey for write.
    let create = unsafe {
        RegCreateKeyExW(
            HKEY_LOCAL_MACHINE,
            PCWSTR(subkey_w.as_ptr()),
            None,
            windows::core::PCWSTR::null(),
            REG_OPTION_NON_VOLATILE,
            KEY_READ | KEY_WRITE,
            None,
            &mut key,
            Some(&mut disposition),
        )
    };
    if create != WIN32_ERROR(0) {
        return Err(format!(
            "Не удалось открыть реестр {subkey} (код {}). Нужны права администратора.",
            create.0
        ));
    }
    let bytes = value.to_le_bytes();
    // SAFETY: DWORD buffer for RegSetValueExW.
    let set =
        unsafe { RegSetValueExW(key, PCWSTR(name_w.as_ptr()), None, REG_DWORD, Some(&bytes)) };
    let _ = unsafe { RegCloseKey(key) };
    if set != WIN32_ERROR(0) {
        return Err(format!(
            "Не удалось записать {name} (код {}). Нужны права администратора.",
            set.0
        ));
    }
    Ok(())
}

fn open_service(access: u32) -> Result<SC_HANDLE, String> {
    let name_w = to_wide(SERVICE_NAME);
    // SAFETY: SCM connect for this process only.
    let scm = unsafe { OpenSCManagerW(PCWSTR::null(), PCWSTR::null(), SC_MANAGER_CONNECT) }
        .map_err(|e| format!("OpenSCManager: {e}"))?;
    // SAFETY: service name NUL-terminated.
    let service = unsafe { OpenServiceW(scm, PCWSTR(name_w.as_ptr()), access) };
    let _ = unsafe { CloseServiceHandle(scm) };
    match service {
        Ok(handle) => Ok(handle),
        Err(error) => {
            if error.code() == windows::core::HRESULT::from_win32(ERROR_SERVICE_DOES_NOT_EXIST.0) {
                Err("служба NcdAutoSetup не найдена на этой системе.".to_owned())
            } else {
                Err(format!("OpenService(NcdAutoSetup): {error}"))
            }
        }
    }
}

fn service_is_running() -> Result<bool, String> {
    let handle = open_service(SERVICE_QUERY_STATUS)?;
    let mut status = SERVICE_STATUS::default();
    // SAFETY: valid service handle.
    let result = unsafe { QueryServiceStatus(handle, &mut status) };
    let _ = unsafe { CloseServiceHandle(handle) };
    result.map_err(|e| format!("QueryServiceStatus: {e}"))?;
    Ok(status.dwCurrentState.0 == 4) // SERVICE_RUNNING
}

fn service_is_disabled() -> Result<bool, String> {
    use windows::Win32::Foundation::ERROR_INSUFFICIENT_BUFFER;
    use windows::Win32::System::Services::QueryServiceConfigW;
    use windows::core::HRESULT;

    let handle = open_service(SERVICE_QUERY_CONFIG)?;
    let mut needed = 0u32;
    let probe = unsafe { QueryServiceConfigW(handle, None, 0, &mut needed) };
    match probe {
        Ok(()) => {}
        Err(error) if error.code() == HRESULT::from_win32(ERROR_INSUFFICIENT_BUFFER.0) => {}
        Err(error) => {
            let _ = unsafe { CloseServiceHandle(handle) };
            return Err(format!("QueryServiceConfig: {error}"));
        }
    }
    if needed == 0 {
        let _ = unsafe { CloseServiceHandle(handle) };
        return Err("QueryServiceConfig: пустой буфер".to_owned());
    }
    let mut buffer = vec![0u8; needed as usize];
    let config_ptr =
        buffer.as_mut_ptr() as *mut windows::Win32::System::Services::QUERY_SERVICE_CONFIGW;
    let result = unsafe { QueryServiceConfigW(handle, Some(config_ptr), needed, &mut needed) };
    let disabled = if result.is_ok() {
        // SAFETY: buffer filled by QueryServiceConfigW.
        let config = unsafe { &*config_ptr };
        config.dwStartType == SERVICE_DISABLED
    } else {
        false
    };
    let _ = unsafe { CloseServiceHandle(handle) };
    result.map_err(|e| format!("QueryServiceConfig: {e}"))?;
    Ok(disabled)
}

fn stop_and_disable_service() -> Result<String, String> {
    let access = SERVICE_STOP | SERVICE_QUERY_STATUS | SERVICE_CHANGE_CONFIG | SERVICE_QUERY_CONFIG;
    let handle = open_service(access)?;

    let mut status = SERVICE_STATUS::default();
    let _ = unsafe { QueryServiceStatus(handle, &mut status) };
    if status.dwCurrentState.0 != 1 {
        // not STOPPED
        let mut stop_status = SERVICE_STATUS::default();
        let _ = unsafe { ControlService(handle, SERVICE_CONTROL_STOP, &mut stop_status) };
    }

    // SAFETY: change only start type to Disabled; other fields SERVICE_NO_CHANGE / null.
    let change = unsafe {
        ChangeServiceConfigW(
            handle,
            ENUM_SERVICE_TYPE(SERVICE_NO_CHANGE),
            SERVICE_DISABLED,
            SERVICE_ERROR(SERVICE_NO_CHANGE),
            PCWSTR::null(),
            PCWSTR::null(),
            None,
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
            PCWSTR::null(),
        )
    };
    let _ = unsafe { CloseServiceHandle(handle) };
    change.map_err(|e| format!("ChangeServiceConfig: {e}"))?;
    Ok("служба NcdAutoSetup остановлена и отключена.".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn summary_mentions_autosetup() {
        let status = NcdStatus {
            private_autosetup: Some(0),
            global_autosetup: Some(0),
            service_disabled: Some(true),
            service_running: Some(false),
        };
        let text = status.summary_ru();
        assert!(text.contains("выкл."));
        assert!(text.contains("отключена"));
    }

    #[test]
    fn query_status_runs() {
        // Should not panic on this machine; values may vary.
        let _ = query_status();
    }
}
