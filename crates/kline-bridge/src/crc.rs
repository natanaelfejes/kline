//! CRC-16/CCITT-FALSE (poly 0x1021, init 0xFFFF, no reflection, no final XOR), the frame check
//! of every GATT frame (docs/bridge-protocol.md "Frame"). Check value of `"123456789"`: 0x29B1
//! (the standard catalogue value for this CRC).

/// CRC-16/CCITT-FALSE of `data`.
pub fn crc16(data: &[u8]) -> u16 {
    let mut crc: u16 = 0xFFFF;
    for &b in data {
        crc ^= u16::from(b) << 8;
        for _ in 0..8 {
            crc = if crc & 0x8000 != 0 {
                (crc << 1) ^ 0x1021
            } else {
                crc << 1
            };
        }
    }
    crc
}

#[cfg(test)]
mod tests {
    use super::crc16;

    #[test]
    fn catalogue_check_value() {
        assert_eq!(crc16(b"123456789"), 0x29B1);
        assert_eq!(crc16(b""), 0xFFFF);
    }
}
