//! DDT2000-specific bit extraction and display (the author's DDT2000 behaviour notes).
//!
//! Sources: DiagLibs2.dll `LittleEndian` init/get 0x1100cf00 / 0x1100d9c0 and `Let`
//! 0x1100d560, `ByteArray.Text` 0x11008f00; KWP2000Diag4.dll `DiagItem.Text` 0x2306f520.

/// DDT2000 little-endian extraction: start byte `FirstByte + BitOffset \ 8`, off =
/// `BitOffset mod 8`, n = `(off + bits - 1) \ 8 + 1` bytes read least significant first
/// (`b1` is the LSB), shifted right by `off` (counted from the LSB) and masked to `bits`.
/// Returns `None` when the frame is too short. `bits` must be 1..=32 and the span at most 4
/// (enforced at load).
pub(crate) fn le_get(frame: &[u8], first_byte: u32, bit_offset: u32, bits: u32) -> Option<u64> {
    let (start, off, n) = span(first_byte, bit_offset, bits)?;
    let bytes = frame.get(start..start.checked_add(n)?)?;
    let v = bytes
        .iter()
        .rev()
        .fold(0u64, |a, &b| (a << 8) | u64::from(b));
    Some((v >> off) & mask(bits))
}

/// Inverse of [`le_get`]: writes `value` (already range-checked to `bits`) and keeps the other
/// bits of the span. `None` if the span is outside the frame.
pub(crate) fn le_set(
    frame: &mut [u8],
    first_byte: u32,
    bit_offset: u32,
    bits: u32,
    value: u64,
) -> Option<()> {
    let (start, off, n) = span(first_byte, bit_offset, bits)?;
    let bytes = frame.get_mut(start..start.checked_add(n)?)?;
    let mut v = bytes
        .iter()
        .rev()
        .fold(0u64, |a, &b| (a << 8) | u64::from(b));
    let m = mask(bits) << off;
    v = (v & !m) | ((value << off) & m);
    for (i, b) in bytes.iter_mut().enumerate() {
        *b = (v >> (8 * i)) as u8;
    }
    Some(())
}

fn span(first_byte: u32, bit_offset: u32, bits: u32) -> Option<(usize, u32, usize)> {
    if first_byte == 0 || bits == 0 || bits > 32 {
        return None;
    }
    let start = (first_byte - 1 + bit_offset / 8) as usize;
    let off = bit_offset % 8;
    let n = ((off + bits - 1) / 8 + 1) as usize;
    Some((start, off, n))
}

pub(crate) fn mask(bits: u32) -> u64 {
    if bits >= 64 {
        u64::MAX
    } else {
        (1u64 << bits) - 1
    }
}

/// DDT2000 sign handling (RE: DiagLibs2 0x1100d9c0 / 0x1100e660, asm 0x1100d1fd..d22d):
/// mask = 2^bits - 1; if the field's top bit is set the value becomes `NOT mask OR v`, i.e. two's
/// complement at the field width, for any width.
pub(crate) fn sign_extend(v: u64, bits: u32) -> i64 {
    if bits == 0 || bits >= 64 {
        return v as i64;
    }
    if v >> (bits - 1) & 1 == 1 {
        (v | !mask(bits)) as i64
    } else {
        v as i64
    }
}

/// VB6 `Hex()` of a Long (uppercase, no padding; negatives as 32-bit two's complement), as used
/// for the `##HEX##` text of a value missing from the List (RE: KWP2000Diag4 0x2306f520,
/// Ordinal_573 on a VT_I4).
pub(crate) fn vb_hex(v: i64) -> String {
    if (i64::from(i32::MIN)..=i64::from(u32::MAX)).contains(&v) {
        format!("{:X}", v as u32)
    } else {
        format!("{v:X}")
    }
}

/// Uppercase hex, two digits per byte, no separator (RE: DiagLibs2 `ByteArray.Text`
/// 0x11008f00: `"0" & Hex(b)` below 0x10).
pub(crate) fn hex_upper(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect()
}

/// `Chr(b)` per byte in the ANSI code page (cp1252 assumed), nothing dropped
/// (RE: KWP2000Diag4 0x2306f520, Ordinal_608).
pub(crate) fn cp1252(b: &[u8]) -> String {
    let (s, _, _) = encoding_rs::WINDOWS_1252.decode(b);
    s.into_owned()
}

/// `__vbaFpI4`: x87 `fistp` with the default rounding mode (round half to even), saturating.
pub(crate) fn fpi4(x: f64) -> i64 {
    let r = x.round_ties_even();
    r.clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i64
}

/// `Format="bin"` (RE: KWP2000Diag4 0x2306f520 case `"bin"`): the scaled value through
/// `__vbaFpI4`, then one character per bit of the item, `"1"` when `v And 1`, prepended while
/// `v = v / 2` (integer division), so the most significant bit is on the left.
pub(crate) fn bin_string(value: f64, bits: u32) -> String {
    let mut v = fpi4(value);
    let mut s = String::new();
    for _ in 0..bits {
        s.insert(0, if v & 1 != 0 { '1' } else { '0' });
        v /= 2;
    }
    s
}

/// A finite decimal number: `digits` (no leading zeros unless the value is 0) times
/// 10^(`point` - digits.len()), i.e. `point` digits before the decimal point.
struct Dec {
    neg: bool,
    digits: Vec<u8>,
    point: i32,
}

impl Dec {
    /// The value with 15 significant digits (the precision VB's Double formatting works with).
    fn from_f64(x: f64) -> Dec {
        let s = format!("{:.14e}", x.abs());
        let (m, e) = s.split_once('e').expect("{:e} has an exponent");
        let e: i32 = e.parse().expect("exponent");
        let digits: Vec<u8> = m
            .bytes()
            .filter(u8::is_ascii_digit)
            .map(|c| c - b'0')
            .collect();
        let mut d = Dec {
            neg: x.is_sign_negative() && x != 0.0,
            digits,
            point: e + 1,
        };
        d.trim();
        d
    }

    fn trim(&mut self) {
        while self.digits.len() > 1 && self.digits.last() == Some(&0) {
            self.digits.pop();
        }
        while self.digits.len() > 1 && self.digits[0] == 0 {
            self.digits.remove(0);
            self.point -= 1;
        }
        if self.digits == [0] {
            self.point = 1;
            self.neg = false;
        }
    }

    /// Round to `decimals` digits after the point, half away from zero (VB `Format`).
    fn round(&mut self, decimals: i32) {
        let keep = self.point + decimals;
        if keep < 0 {
            *self = Dec {
                neg: false,
                digits: vec![0],
                point: 1,
            };
            return;
        }
        let keep = keep as usize;
        if keep >= self.digits.len() {
            return;
        }
        let up = self.digits[keep] >= 5;
        self.digits.truncate(keep);
        if up {
            let mut i = keep;
            loop {
                if i == 0 {
                    self.digits.insert(0, 1);
                    self.point += 1;
                    break;
                }
                i -= 1;
                if self.digits[i] == 9 {
                    self.digits[i] = 0;
                } else {
                    self.digits[i] += 1;
                    break;
                }
            }
        }
        if self.digits.is_empty() {
            self.digits.push(0);
        }
        self.trim();
    }

    fn digit(&self, pos: i32) -> u8 {
        // pos 0 = first digit before the point counted from the left of `digits`.
        if pos < 0 || pos as usize >= self.digits.len() {
            0
        } else {
            self.digits[pos as usize]
        }
    }

    fn int_digits(&self) -> String {
        if self.point <= 0 {
            return "0".into();
        }
        (0..self.point)
            .map(|i| char::from(b'0' + self.digit(i)))
            .collect()
    }

    fn frac_digits(&self, n: i32) -> String {
        (0..n)
            .map(|i| char::from(b'0' + self.digit(self.point + i)))
            .collect()
    }
}

/// VB6 `Format(value)` with an empty format: like `CStr` of a Double, up to 15 significant
/// digits, exponent form (`1E-05`, `1E+15`) outside 1e-4 ..< 1e15. NOT VERIFIED against
/// OLEAUT32 for the exponent thresholds.
pub(crate) fn vb_general(x: f64) -> String {
    if !x.is_finite() {
        return format!("{x}");
    }
    let d = Dec::from_f64(x);
    let sign = if d.neg { "-" } else { "" };
    if d.digits == [0] {
        return "0".into();
    }
    let exp = d.point - 1;
    if !(-4..15).contains(&exp) {
        let mut m = String::new();
        m.push(char::from(b'0' + d.digits[0]));
        if d.digits.len() > 1 {
            m.push('.');
            m.extend(d.digits[1..].iter().map(|c| char::from(b'0' + c)));
        }
        return format!(
            "{sign}{m}E{}{:02}",
            if exp < 0 { '-' } else { '+' },
            exp.abs()
        );
    }
    let int = d.int_digits();
    let frac_len = (d.digits.len() as i32 - d.point).max(0);
    if frac_len == 0 {
        format!("{sign}{int}")
    } else {
        format!("{sign}{int}.{}", d.frac_digits(frac_len))
    }
}

/// VB6 `Format(value, fmt)` for the numeric formats DDT2000 definitions use (`#0.00`, `#0`,
/// `00`, `00.00000`): `0` is a required digit, `#` an optional one, the first `.` the decimal
/// point; the value is rounded half away from zero to the number of `0`/`#` after the point,
/// then `#` trailing zeros are dropped. Other characters are copied as literals (before the
/// first placeholder / after the last one); `,` grouping and `;` sections are not supported (no
/// definition uses them). The decimal point is always `.` (deliberately locale-independent;
/// DDT2000 follows the Windows locale). An empty format is [`vb_general`].
pub(crate) fn vb_format(x: f64, fmt: &str) -> String {
    if fmt.is_empty() || !x.is_finite() {
        return vb_general(x);
    }
    let is_ph = |c: char| c == '0' || c == '#';
    let first = fmt.find(|c: char| is_ph(c) || c == '.');
    let last = fmt.rfind(|c: char| is_ph(c) || c == '.');
    let (Some(first), Some(last)) = (first, last) else {
        return fmt.to_string();
    };
    let prefix = &fmt[..first];
    let body = &fmt[first..=last];
    let suffix = &fmt[last + 1..];
    let (int_f, frac_f) = match body.split_once('.') {
        Some((i, f)) => (i, f),
        None => (body, ""),
    };
    let min_int = int_f.chars().filter(|&c| c == '0').count();
    let frac_zero = frac_f.chars().filter(|&c| c == '0').count() as i32;
    let frac_all = frac_f.chars().filter(|&c| is_ph(c)).count() as i32;
    let mut d = Dec::from_f64(x);
    d.round(frac_all);
    let mut int = d.int_digits();
    if int == "0" && min_int == 0 {
        int.clear();
    }
    while int.len() < min_int {
        int.insert(0, '0');
    }
    let mut frac = d.frac_digits(frac_all);
    while frac.len() as i32 > frac_zero && frac.ends_with('0') {
        frac.pop();
    }
    let sign = if d.neg { "-" } else { "" };
    let num = if body.contains('.') && (!frac.is_empty() || frac_zero > 0) {
        format!("{int}.{frac}")
    } else if body.contains('.') {
        // VB keeps the point when the format has one but no digit is left after it.
        format!("{int}.")
    } else {
        int
    };
    format!("{prefix}{sign}{num}{suffix}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn vb_format_cases() {
        assert_eq!(vb_format(3.0, "00"), "03");
        assert_eq!(vb_format(1032.4, "#0"), "1032");
        assert_eq!(vb_format(1032.5, "#0"), "1033");
        assert_eq!(vb_format(-2.5, "#0"), "-3");
        assert_eq!(vb_format(12.345678, "00.00000"), "12.34568");
        assert_eq!(vb_format(1.5, "00.00000"), "01.50000");
        assert_eq!(vb_format(-39.375, "#0.00"), "-39.38");
        assert_eq!(vb_format(-38.125, "#0.00"), "-38.13");
        assert_eq!(vb_format(0.0, "#0.00"), "0.00");
        assert_eq!(vb_format(2.675, "#0.00"), "2.68");
        assert_eq!(vb_format(9.999, "#0.00"), "10.00");
        assert_eq!(vb_format(0.5, "#.##"), ".5");
    }

    #[test]
    fn vb_general_cases() {
        assert_eq!(vb_general(1032.4), "1032.4");
        assert_eq!(vb_general(0.1 + 0.2), "0.3");
        assert_eq!(vb_general(-7.0), "-7");
        assert_eq!(vb_general(0.0001), "0.0001");
        assert_eq!(vb_general(0.00001), "1E-05");
        assert_eq!(vb_general(1e15), "1E+15");
    }

    #[test]
    fn bin_and_hex() {
        assert_eq!(bin_string(5.0, 8), "00000101");
        // Truncating `v / 2` as in the decompiled loop: -1 -> "1" then 0s (not two's complement).
        assert_eq!(bin_string(-1.0, 4), "0001");
        assert_eq!(vb_hex(255), "FF");
        assert_eq!(vb_hex(-1), "FFFFFFFF");
        assert_eq!(sign_extend(0x8, 4), -8);
        assert_eq!(sign_extend(0x7, 4), 7);
        assert_eq!(sign_extend(0xFFF, 12), -1);
    }

    #[test]
    fn le_matches_ddt2000_not_ddt4all() {
        // 12 bits of 34 12: DDT2000 0x234 (ddt4all would give 0x1204).
        assert_eq!(le_get(&[0x34, 0x12], 1, 0, 12), Some(0x234));
        let mut f = [0u8; 3];
        le_set(&mut f, 1, 4, 12, 0xABC).unwrap();
        assert_eq!(le_get(&f, 1, 4, 12), Some(0xABC));
        assert_eq!(f, [0xC0, 0xAB, 0x00]);
    }
}
