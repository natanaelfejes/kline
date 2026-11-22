//! The simulator's own read/dangerous classification of request payloads.
//!
//! Written from SAFETY.md rules 2 and 3, independently of `kline_core::safety`: if the core
//! ever lets a write through, the ECU write log catches it here.

use crate::journal::WriteKind;

/// BMT (UCH) address; `21 A4` there is the transponder *status* read (a read, exactly 2 bytes;
/// anything after it is treated as an immobilizer write).
pub const BMT_ADDRESS: u8 = 0x26;

/// `None` for a Tier 0 read / protocol frame (SAFETY.md rule 2), else why it is dangerous.
pub fn classify(iso8: bool, ecu_address: u8, payload: &[u8]) -> Option<WriteKind> {
    let Some(&sid) = payload.first() else {
        return Some(WriteKind::Unknown);
    };
    let len = payload.len();
    let p1 = payload.get(1).copied();
    if iso8 {
        return match sid {
            0x21 if len == 1 => None,
            0x33 if len == 2 && matches!(p1, Some(0x50 | 0x51 | 0x52 | 0x54)) => None,
            0x36 | 0x3F if len == 1 => None,
            0x31..=0x33 => Some(WriteKind::Routine),
            other => Some(dangerous_sid(other)),
        };
    }
    match sid {
        0x81 | 0x82 if len == 1 => None,
        0x3E if len <= 2 => None,
        0x10 if len == 2 && matches!(p1, Some(0x81 | 0xA0)) => None,
        0x10 => Some(WriteKind::SessionChange),
        0x1A if len == 2 => None,
        0x17 | 0x18 if (2..=4).contains(&len) => None,
        0x21 if len == 2 => None,
        0x21 if ecu_address == BMT_ADDRESS && p1 == Some(0xA4) => Some(WriteKind::Immobilizer),
        0x31..=0x33 => Some(WriteKind::Routine),
        0x21 | 0x1A | 0x17 | 0x18 | 0x3E => Some(WriteKind::Unknown),
        other => Some(dangerous_sid(other)),
    }
}

fn dangerous_sid(sid: u8) -> WriteKind {
    match sid {
        0x11 | 0x20 => WriteKind::Reset,
        0x14 => WriteKind::ClearDtc,
        0x27 => WriteKind::Security,
        0x2E | 0x3B | 0x3D | 0x40 | 0x41 => WriteKind::Write,
        0x2F | 0x30 | 0x61 => WriteKind::IoControl,
        0x34..=0x37 => WriteKind::Programming,
        _ => WriteKind::Unknown,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_writes() {
        assert_eq!(classify(false, 0x7A, &[0x21, 0xA9]), None);
        assert_eq!(classify(false, 0x7A, &[0x10, 0xA0]), None);
        assert_eq!(classify(false, 0x7A, &[0x21, 0xA4]), None);
        // Transponder status read is Tier 0; with extra bytes it is an immobilizer write.
        assert_eq!(classify(false, 0x26, &[0x21, 0xA4]), None);
        assert_eq!(
            classify(false, 0x26, &[0x21, 0xA4, 0x01]),
            Some(WriteKind::Immobilizer)
        );
        assert_eq!(
            classify(false, 0x26, &[0x21, 0xA1, 0x01]),
            Some(WriteKind::Unknown)
        );
        assert_eq!(
            classify(true, 0x7A, &[0x33, 0x53]),
            Some(WriteKind::Routine)
        );
        assert_eq!(
            classify(false, 0x26, &[0x31, 0xC7, 0, 0, 0]),
            Some(WriteKind::Routine)
        );
        assert_eq!(
            classify(false, 0x7A, &[0x10, 0x85]),
            Some(WriteKind::SessionChange)
        );
        assert_eq!(
            classify(false, 0x7A, &[0x3B, 0xD0, 0x10]),
            Some(WriteKind::Write)
        );
        assert_eq!(classify(true, 0x7A, &[0x33, 0x51]), None);
        assert_eq!(classify(true, 0x7A, &[0x20, 0x00]), Some(WriteKind::Reset));
        assert_eq!(
            classify(true, 0x7A, &[0x61, 0x02, 0x01]),
            Some(WriteKind::IoControl)
        );
    }
}
