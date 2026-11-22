//! Finding the FTDI KKL cable.

use serde_json::{json, Value};

/// FTDI USB vendor id.
pub const FTDI_VID: u16 = 0x0403;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortInfo {
    pub path: String,
    pub usb: Option<(u16, u16)>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub serial: Option<String>,
}

impl PortInfo {
    pub fn to_json(&self) -> Value {
        json!({
            "path": self.path,
            "candidate": is_candidate(self),
            "vid": self.usb.map(|u| format!("{:04x}", u.0)),
            "pid": self.usb.map(|u| format!("{:04x}", u.1)),
            "manufacturer": self.manufacturer,
            "product": self.product,
            "serial": self.serial,
        })
    }
}

/// All serial ports the OS lists.
pub fn list() -> std::io::Result<Vec<PortInfo>> {
    let ports = serialport::available_ports().map_err(std::io::Error::from)?;
    Ok(ports
        .into_iter()
        .map(|p| {
            let (usb, manufacturer, product, serial) = match p.port_type {
                serialport::SerialPortType::UsbPort(u) => (
                    Some((u.vid, u.pid)),
                    u.manufacturer,
                    u.product,
                    u.serial_number,
                ),
                _ => (None, None, None, None),
            };
            PortInfo {
                path: p.port_name,
                usb,
                manufacturer,
                product,
                serial,
            }
        })
        .collect())
}

/// A port that looks like the KKL cable (docs/platforms.md): macOS `/dev/cu.usbserial-*`
/// (AppleUSBFTDI), a Linux `/dev/ttyUSB*` with the FTDI vendor id (ftdi_sio), or
/// a Windows `COMn` with the FTDI vendor id (FTDI VCP driver).
pub fn is_candidate(p: &PortInfo) -> bool {
    let ftdi = p.usb.is_some_and(|u| u.0 == FTDI_VID);
    p.path.starts_with("/dev/cu.usbserial-")
        || (p.path.starts_with("/dev/ttyUSB") && ftdi)
        || (is_windows_com(&p.path) && ftdi)
}

/// `COMn` or `\\.\COMn` (any case). `serialport` prefixes `\\.\` itself, so `COM10` and
/// above open without the user typing the device-namespace form.
pub fn is_windows_com(path: &str) -> bool {
    let p = path.strip_prefix(r"\\.\").unwrap_or(path);
    p.len() > 3 && p[..3].eq_ignore_ascii_case("com") && p[3..].bytes().all(|b| b.is_ascii_digit())
}

/// An explicit `--port` must not be a macOS callin device `/dev/tty.*`: opening it waits for
/// carrier detect (DCD), which a KKL cable never raises. Use the callout `/dev/cu.*` twin.
pub fn check_explicit_port(path: &str) -> Result<(), String> {
    match path.strip_prefix("/dev/tty.") {
        Some(rest) => Err(format!(
            "{path} is a macOS callin device (blocks until carrier detect); use /dev/cu.{rest}"
        )),
        None => Ok(()),
    }
}

/// The one candidate, or why there is none.
pub fn auto_detect(ports: &[PortInfo]) -> Result<String, String> {
    let c: Vec<&PortInfo> = ports.iter().filter(|p| is_candidate(p)).collect();
    match c.as_slice() {
        [one] => Ok(one.path.clone()),
        [] => Err("no KKL cable found (no /dev/cu.usbserial-*, FTDI /dev/ttyUSB* or FTDI COMn); plug it in or pass --port".into()),
        many => Err(format!(
            "several cables found ({}); pass --port",
            many.iter()
                .map(|p| p.path.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        )),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn tty_callin_devices_are_refused() {
        assert_eq!(
            super::check_explicit_port("/dev/tty.usbserial-A5"),
            Err("/dev/tty.usbserial-A5 is a macOS callin device (blocks until carrier detect); use /dev/cu.usbserial-A5".into())
        );
        assert!(super::check_explicit_port("/dev/cu.usbserial-A5").is_ok());
        assert!(super::check_explicit_port("/dev/ttyUSB0").is_ok());
    }

    use super::*;

    fn p(path: &str, vid: Option<u16>) -> PortInfo {
        PortInfo {
            path: path.into(),
            usb: vid.map(|v| (v, 0x6001)),
            manufacturer: None,
            product: None,
            serial: None,
        }
    }

    #[test]
    fn detects_exactly_one_macos_ftdi_port() {
        let ports = [
            p("/dev/cu.Bluetooth-Incoming-Port", None),
            p("/dev/tty.usbserial-A50285BI", Some(FTDI_VID)),
            p("/dev/cu.usbserial-A50285BI", Some(FTDI_VID)),
        ];
        assert_eq!(
            auto_detect(&ports),
            Ok("/dev/cu.usbserial-A50285BI".to_string())
        );
    }

    #[test]
    fn detects_ftdi_ports_on_linux_and_windows_only() {
        assert_eq!(
            auto_detect(&[p("/dev/ttyS0", None), p("/dev/ttyUSB0", Some(FTDI_VID))]),
            Ok("/dev/ttyUSB0".to_string())
        );
        // Windows: COM1 is the motherboard UART, COM12 an FTDI (COM10+ needs \\.\ inside
        // serialport, which adds it).
        assert_eq!(
            auto_detect(&[
                p("COM1", None),
                p("COM12", Some(FTDI_VID)),
                p("COM3", Some(0x10c4))
            ]),
            Ok("COM12".to_string())
        );
        assert!(is_windows_com(r"\\.\COM12") && is_windows_com("com3"));
        assert!(!is_windows_com("COM") && !is_windows_com("COMX") && !is_windows_com("/dev/COM1"));
        assert!(check_explicit_port(r"\\.\COM12").is_ok() && check_explicit_port("COM3").is_ok());
    }

    #[test]
    fn none_or_several_is_an_error() {
        assert!(auto_detect(&[p("/dev/cu.Bluetooth", None)]).is_err());
        let two = [
            p("/dev/cu.usbserial-A", Some(FTDI_VID)),
            p("/dev/cu.usbserial-B", Some(FTDI_VID)),
        ];
        assert!(auto_detect(&two).unwrap_err().contains("several"));
    }
}
