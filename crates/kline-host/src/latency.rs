//! The FTDI latency timer: how long the chip holds received bytes before sending a short USB
//! packet (default 16 ms). The ~KW2 answer of a 5-baud init must go out 25..50 ms after KW2
//! (W4, ISO 14230-2), so 16 ms on RX plus the host's own delays is the tight spot; DDT2000 users
//! set 1 ms. Per OS (docs/platforms.md "Latency timer"):
//!
//! - Linux (`ftdi_sio`): `/sys/bus/usb-serial/devices/ttyUSBn/latency_timer`, read here and
//!   written by `kline selftest --set-latency 1` (root, or a udev rule).
//! - Windows (FTDI VCP driver): Device Manager > Port > Advanced > Latency Timer, stored as the
//!   `LatencyTimer` value of the device's registry key. Not read by kline.
//! - macOS (AppleUSBFTDI): no user setting; measure with `kline selftest --echo-rtt`.

use std::io;
use std::path::{Path, PathBuf};

/// sysfs mount point on Linux.
pub const SYS_ROOT: &str = "/sys";
/// Valid range of the ftdi_sio latency timer (ms, the driver's limits).
pub const MIN_MS: u8 = 1;
pub const MAX_MS: u8 = 255;

/// The `latency_timer` attribute of a Linux `ttyUSBn` port under `sys_root`, following
/// symlinks such as `/dev/serial/by-id/...`. `None` when the port is not a `ttyUSB` device.
pub fn sysfs_path(port: &str, sys_root: &Path) -> Option<PathBuf> {
    let real = std::fs::canonicalize(port).unwrap_or_else(|_| PathBuf::from(port));
    let name = real.file_name()?.to_str()?;
    if !name.starts_with("ttyUSB") {
        return None;
    }
    Some(
        sys_root
            .join("bus/usb-serial/devices")
            .join(name)
            .join("latency_timer"),
    )
}

pub fn read(path: &Path) -> io::Result<u8> {
    let s = std::fs::read_to_string(path)?;
    s.trim()
        .parse()
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{s:?}: {e}")))
}

pub fn write(path: &Path, ms: u8) -> io::Result<()> {
    std::fs::write(path, format!("{ms}\n"))
}

/// How to make the setting stick or run it with privileges (shown when a write fails).
pub fn write_hint(path: &Path, ms: u8) -> String {
    format!(
        "run as root (sudo sh -c 'echo {ms} > {}') or install the udev rule in docs/platforms.md",
        path.display()
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ttyusb_ports_map_to_their_sysfs_attribute() {
        let root = Path::new("/fake/sys");
        assert_eq!(
            sysfs_path("/dev/ttyUSB3", root),
            Some(PathBuf::from(
                "/fake/sys/bus/usb-serial/devices/ttyUSB3/latency_timer"
            ))
        );
        assert_eq!(sysfs_path("/dev/ttyACM0", root), None);
        assert_eq!(sysfs_path("/dev/cu.usbserial-A5", root), None);
        assert_eq!(sysfs_path("COM4", root), None);
    }

    #[test]
    fn read_and_write_round_trip() {
        let d = std::env::temp_dir().join(format!("kline-lat-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let p = d.join("latency_timer");
        std::fs::write(&p, "16\n").unwrap();
        assert_eq!(read(&p).unwrap(), 16);
        write(&p, 1).unwrap();
        assert_eq!(read(&p).unwrap(), 1);
        std::fs::write(&p, "x").unwrap();
        assert!(read(&p).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }
}
