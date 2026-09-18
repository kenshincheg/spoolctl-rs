//! OS version helpers (Win7 vs Win10+) via RtlGetVersion.

use windows::Win32::System::SystemInformation::OSVERSIONINFOW;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OsVersion {
    pub major: u32,
    pub minor: u32,
    pub build: u32,
}

impl OsVersion {
    pub fn is_windows_10_or_newer(self) -> bool {
        self.major >= 10
    }

    pub fn display(self) -> String {
        format!("{}.{}.{}", self.major, self.minor, self.build)
    }
}

pub fn current() -> Option<OsVersion> {
    let mut info = OSVERSIONINFOW {
        dwOSVersionInfoSize: u32::try_from(std::mem::size_of::<OSVERSIONINFOW>()).ok()?,
        ..Default::default()
    };
    // SAFETY: info size is set; RtlGetVersion fills the struct on success (STATUS_SUCCESS = 0).
    let status = unsafe { RtlGetVersion(&mut info) };
    if status != 0 {
        return None;
    }
    Some(OsVersion {
        major: info.dwMajorVersion,
        minor: info.dwMinorVersion,
        build: info.dwBuildNumber,
    })
}

unsafe extern "system" {
    fn RtlGetVersion(lp_version_information: *mut OSVERSIONINFOW) -> i32;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_machine_reports_a_version() {
        let ver = current().expect("RtlGetVersion");
        assert!(ver.major >= 6);
        // Win7+ for this project target: 6.1 or major >= 10.
        assert!(ver.major > 6 || (ver.major == 6 && ver.minor >= 1) || ver.major >= 10);
    }
}
