//! Local or remote PC target for Spooler / queue operations.
//!
//! Remote uses Windows built-in mechanisms (no agent on the other PC):
//! - Service Control Manager: `OpenSCManagerW(\\\\host, …)`
//! - Queue files: `\\\\host\\ADMIN$\\System32\\spool\\PRINTERS`
//! - Printers list: `EnumPrinters` with server name
//!
//! Requirements on the remote PC: admin rights from this session, firewall
//! allowing Remote Service Management + File and Printer Sharing (ADMIN$).

use std::path::PathBuf;

/// Target computer. Empty name = this PC.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Host {
    name: String,
}

impl Host {
    pub fn local() -> Self {
        Self {
            name: String::new(),
        }
    }

    /// Parse user input: `PC01`, `\\PC01`, `192.168.1.10`, FQDN.
    pub fn parse(raw: &str) -> Result<Self, String> {
        let trimmed = raw.trim();
        if trimmed.is_empty() {
            return Ok(Self::local());
        }
        let name = trimmed
            .trim_start_matches('\\')
            .trim_end_matches('\\')
            .trim();
        if name.is_empty() {
            return Ok(Self::local());
        }
        if name.contains(['/', '\\', ':', '*', '?', '"', '<', '>', '|']) {
            return Err(
                "Недопустимые символы в имени ПК. Укажите имя хоста или IPv4 без пути."
                    .to_owned(),
            );
        }
        if name.len() > 255 {
            return Err("Слишком длинное имя ПК.".to_owned());
        }
        Ok(Self {
            name: name.to_owned(),
        })
    }

    pub fn is_local(&self) -> bool {
        self.name.is_empty()
    }

    /// Host name without `\\`, or empty for local.
    pub fn name(&self) -> &str {
        &self.name
    }

    pub fn label_ru(&self) -> String {
        if self.is_local() {
            "этот ПК".to_owned()
        } else {
            self.name.clone()
        }
    }

    /// Wide string for `OpenSCManagerW` machine name (`\\HOST`), or `None` = local.
    pub fn scm_machine_wide(&self) -> Option<Vec<u16>> {
        if self.is_local() {
            None
        } else {
            Some(to_wide(&format!(r"\\{}", self.name)))
        }
    }

    /// Wide string for `EnumPrintersW` server name, or `None` = local.
    pub fn winspool_server_wide(&self) -> Option<Vec<u16>> {
        self.scm_machine_wide()
    }

    /// Name passed to `OpenPrinterW` (adds `\\host\` when remote).
    pub fn printer_open_name(&self, printer: &str) -> String {
        if self.is_local() {
            printer.to_owned()
        } else {
            format!(r"\\{}\{}", self.name, printer)
        }
    }

    /// Local `%SystemRoot%\…\PRINTERS` or `\\host\ADMIN$\System32\spool\PRINTERS`.
    pub fn printers_queue_dir(&self) -> Result<PathBuf, String> {
        if self.is_local() {
            let system_root = std::env::var_os("SystemRoot")
                .or_else(|| std::env::var_os("windir"))
                .ok_or_else(|| "Не задана переменная SystemRoot.".to_owned())?;
            Ok(PathBuf::from(system_root)
                .join("System32")
                .join("spool")
                .join("PRINTERS"))
        } else {
            Ok(PathBuf::from(format!(
                r"\\{}\ADMIN$\System32\spool\PRINTERS",
                self.name
            )))
        }
    }
}

fn to_wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_strips_slashes_and_accepts_ip() {
        assert!(Host::parse("").unwrap().is_local());
        assert_eq!(Host::parse(r"\\PC01").unwrap().name(), "PC01");
        assert_eq!(Host::parse("192.168.0.5").unwrap().name(), "192.168.0.5");
    }

    #[test]
    fn parse_rejects_path_chars() {
assert!(Host::parse(r"PC01\share").is_err());
        assert!(Host::parse("http://x").is_err());
    }

    #[test]
    fn remote_queue_uses_admin_share() {
        let h = Host::parse("OFFICE-PC").unwrap();
        let p = h.printers_queue_dir().unwrap();
        assert!(p.to_string_lossy().contains(r"\\OFFICE-PC\ADMIN$"));
    }
}
