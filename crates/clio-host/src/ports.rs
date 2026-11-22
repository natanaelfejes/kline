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

/// A port that looks like the KKL cable: macOS `/dev/cu.usbserial-*` (AppleUSBFTDI,
/// the design notes), or (convenience, not the target platform) a Linux `/dev/ttyUSB*` FTDI.
pub fn is_candidate(p: &PortInfo) -> bool {
    p.path.starts_with("/dev/cu.usbserial-")
        || (p.path.starts_with("/dev/ttyUSB") && p.usb.is_some_and(|u| u.0 == FTDI_VID))
}

/// The one candidate, or why there is none.
pub fn auto_detect(ports: &[PortInfo]) -> Result<String, String> {
    let c: Vec<&PortInfo> = ports.iter().filter(|p| is_candidate(p)).collect();
    match c.as_slice() {
        [one] => Ok(one.path.clone()),
        [] => Err("no KKL cable found (no /dev/cu.usbserial-*); plug it in or pass --port".into()),
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
    fn none_or_several_is_an_error() {
        assert!(auto_detect(&[p("/dev/cu.Bluetooth", None)]).is_err());
        let two = [
            p("/dev/cu.usbserial-A", Some(FTDI_VID)),
            p("/dev/cu.usbserial-B", Some(FTDI_VID)),
        ];
        assert!(auto_detect(&two).unwrap_err().contains("several"));
    }
}
