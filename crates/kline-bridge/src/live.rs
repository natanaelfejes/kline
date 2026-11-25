//! Decoding of the polled frames without an ECU definition (the firmware has no XML).
//!
//! Byte positions and scales are from the car's captures of 2026-11-22, cross-checked there
//! against the definitions' decoding: Sirius `21AA`/`21A9` in SIRIUS-ANALYSIS.md ("21AA
//! payload", "21A9 payload"), BMT `21A3`/`21A0`/`21A2` in BMT-ANALYSIS.md section 6. `d[i]` is
//! data byte `i` after the `61 lid` header. The door bits are NOT VERIFIED on the car (no door
//! was opened in a capture); their position is the BMT definition's (see [`BmtFrame::Doors`]).

use crate::gatt::LockState;

/// Fields from Sirius `61 AA ..`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SiriusAA {
    pub rpm: u16,
    pub inj_us: u16,
    pub fuel_mlh: u16,
}

/// Fields from Sirius `61 A9 ..`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SiriusA9 {
    pub rpm: u16,
    pub batt_cv: u16,
    pub map_mbar: u16,
    pub coolant_c: i8,
    pub iat_c: i8,
    pub speed_kmh: u8,
}

/// Fields from the BMT parked frames.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BmtFrame {
    /// `61 A3`: DCODCOND, d0 bit 7, 1 = unlocked (BMT-ANALYSIS: polarity inferred).
    Lock(LockState),
    /// `61 A0`: door bits in d2, 0x02 front, 0x04 rear. The definition places them at
    /// FirstByte 5 (1-based over the reply from `61`), BitOffset 6 and 5 (from the MSB), that
    /// is data byte 2; the same convention puts the indicator at d3 0x08, which the 2026-11-22
    /// capture confirmed (BMT-ANALYSIS: "A0 d3.3" toggling at 1.46 Hz). An earlier note
    /// ("byte 5") was read as d5, which is constant 0 in every capture. Polarity (1 = open) is
    /// the definition's; NOT VERIFIED on the car.
    Doors { front_open: bool, rear_open: bool },
    /// `61 A2`: filtered battery voltage d4 x 0.077 V.
    Battery { batt_cv: u16 },
}

fn data(payload: &[u8], lid: u8, min: usize) -> Option<&[u8]> {
    match payload {
        [0x61, l, d @ ..] if *l == lid && d.len() >= min => Some(d),
        _ => None,
    }
}

fn be16(d: &[u8], i: usize) -> u16 {
    u16::from_be_bytes([d[i], d[i + 1]])
}

/// Temperature `raw * 0.625 - 40` in whole degrees, clamped to i8.
fn temp(raw: u8) -> i8 {
    let tenths = i32::from(raw) * 625 / 100 - 400;
    (tenths.div_euclid(10)).clamp(-128, 127) as i8
}

/// Decode a positive `21 AA` reply payload.
pub fn sirius_aa(payload: &[u8]) -> Option<SiriusAA> {
    let d = data(payload, 0xAA, 12)?;
    Some(SiriusAA {
        rpm: be16(d, 0),
        // u16[5:7] x 0.004 ms = 4 us per bit.
        inj_us: be16(d, 5).saturating_mul(4),
        // CONSLH u16[10:12] x 0.01 L/h = 10 mL/h per bit.
        fuel_mlh: be16(d, 10).saturating_mul(10),
    })
}

/// Decode a positive `21 A9` reply payload.
pub fn sirius_a9(payload: &[u8]) -> Option<SiriusA9> {
    let d = data(payload, 0xA9, 10)?;
    Some(SiriusA9 {
        rpm: be16(d, 0),
        // Vbat = b2 x 0.032 + 8 V -> centivolts.
        batt_cv: (u32::from(d[2]) * 32 / 10 + 800) as u16,
        // MAP = b3 x 3.7 + 103 mbar.
        map_mbar: (u32::from(d[3]) * 37 / 10 + 103) as u16,
        coolant_c: temp(d[5]),
        iat_c: temp(d[6]),
        // Speed b9, 1 km/h per bit.
        speed_kmh: d[9],
    })
}

/// Decode a positive BMT parked-frame reply payload.
pub fn bmt(payload: &[u8]) -> Option<BmtFrame> {
    match payload.get(1)? {
        0xA3 => {
            let d = data(payload, 0xA3, 1)?;
            Some(BmtFrame::Lock(if d[0] & 0x80 != 0 {
                LockState::Unlocked
            } else {
                LockState::Locked
            }))
        }
        0xA0 => {
            let d = data(payload, 0xA0, 3)?;
            Some(BmtFrame::Doors {
                front_open: d[2] & 0x02 != 0,
                rear_open: d[2] & 0x04 != 0,
            })
        }
        0xA2 => {
            let d = data(payload, 0xA2, 5)?;
            Some(BmtFrame::Battery {
                batt_cv: (u32::from(d[4]) * 77 / 10) as u16,
            })
        }
        _ => None,
    }
}

/// The BMT's vehicle speed in a `21 A2` reply, km/h (data byte 3, the definition's position;
/// the scale of 1 km/h per bit is assumed, NOT VERIFIED on the car).
pub fn bmt_speed_kmh(payload: &[u8]) -> Option<u8> {
    (payload.get(1) == Some(&0xA2)).then_some(())?;
    data(payload, 0xA2, 4).map(|d| d[3])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// SIRIUS-ANALYSIS "21AA #0": `61AA 0435 80 8D 83 073D 62 00 00 00 8C`: 1077 rpm,
    /// 7.412 ms, 1.40 L/h.
    #[test]
    fn sirius_aa_capture_vector() {
        let p = [
            0x61, 0xAA, 0x04, 0x35, 0x80, 0x8D, 0x83, 0x07, 0x3D, 0x62, 0x00, 0x00, 0x00, 0x8C,
        ];
        let v = sirius_aa(&p).unwrap();
        assert_eq!(v.rpm, 1077);
        assert_eq!(v.inj_us, 7412);
        assert_eq!(v.fuel_mlh, 1400);
        assert_eq!(sirius_aa(&p[..13]), None);
    }

    /// SIRIUS-ANALYSIS "21A9 #0": `61A9 0435 C6 52 1B 5F 59 2A 2A 00 1B 05 5A`: 1077 rpm,
    /// 14.336 V, MAP 406.4, CLT 19.375, speed 0.
    #[test]
    fn sirius_a9_capture_vector() {
        let p = [
            0x61, 0xA9, 0x04, 0x35, 0xC6, 0x52, 0x1B, 0x5F, 0x59, 0x2A, 0x2A, 0x00, 0x1B, 0x05,
            0x5A,
        ];
        let v = sirius_a9(&p).unwrap();
        assert_eq!(v.rpm, 1077);
        assert_eq!(v.batt_cv, 1433);
        assert_eq!(v.map_mbar, 406);
        assert_eq!(v.coolant_c, 19);
        assert_eq!(v.speed_kmh, 0);
    }

    /// BMT-ANALYSIS "449.766 A3 d0 A0 to 20 (bit 7)": A0 unlocked, 20 locked.
    #[test]
    fn bmt_lock_bit() {
        assert_eq!(
            bmt(&[0x61, 0xA3, 0xA0, 0x01, 0x03]),
            Some(BmtFrame::Lock(LockState::Unlocked))
        );
        assert_eq!(
            bmt(&[0x61, 0xA3, 0x20, 0x00, 0x03]),
            Some(BmtFrame::Lock(LockState::Locked))
        );
        assert_eq!(bmt(&[0x61, 0xA3]), None);
        assert_eq!(bmt(&[0x7F, 0x21, 0x12]), None);
    }

    /// The 2026-11-22 BMT capture `61 A0 C0 00 00 02 00 00 00` (doors closed, lanterns on):
    /// closed. The doors are in d2; d3 bit 0x02 (lanterns) and d5 are not doors.
    #[test]
    fn bmt_door_bits_are_in_data_byte_2() {
        let closed = [0x61, 0xA0, 0xC0, 0x00, 0x00, 0x02, 0x00, 0x00, 0x00];
        assert_eq!(
            bmt(&closed),
            Some(BmtFrame::Doors {
                front_open: false,
                rear_open: false
            })
        );
        let front = [0x61, 0xA0, 0xC0, 0x00, 0x02, 0x02, 0x00, 0x00, 0x00];
        assert_eq!(
            bmt(&front),
            Some(BmtFrame::Doors {
                front_open: true,
                rear_open: false
            })
        );
        let rear = [0x61, 0xA0, 0xC0, 0x00, 0x04, 0x00, 0x00, 0x00, 0x00];
        assert_eq!(
            bmt(&rear),
            Some(BmtFrame::Doors {
                front_open: false,
                rear_open: true
            })
        );
        // Bits elsewhere (d5) are not doors.
        let d5 = [0x61, 0xA0, 0xC0, 0x00, 0x00, 0x00, 0x00, 0x06, 0x00];
        assert_eq!(
            bmt(&d5),
            Some(BmtFrame::Doors {
                front_open: false,
                rear_open: false
            })
        );
    }
}
