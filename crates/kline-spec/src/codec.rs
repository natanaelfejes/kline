//! Bit extraction and value conversion, matching ddt4all `EcuData.getHexValue`,
//! `getDisplayValue` and (for big endian) `setValue`. See docs/spec-semantics.md.

use serde::Serialize;

use crate::ddt2000;
use crate::error::SpecError;
use crate::model::{DataDef, DataItem, EcuDef, Endian, HexBytes, Request, Scaled, Semantics};

/// A decoded (or to-be-encoded) value.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Value {
    /// Result of a `<Scaled>` conversion.
    Number(f64),
    /// A `<List>` label, or the text of an ASCII `<Bytes>` item.
    Text(String),
    /// Hex text. DDT2000: a byte-array item (uppercase, `ByteArray.Text`) or a value missing
    /// from the List (VB `Hex()` of the value; the display adds `##..##`). ddt4all: an unscaled
    /// value with no matching list entry, lowercase hex of the raw bytes.
    Hex(String),
}

/// The raw bits of one item before conversion.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Raw {
    /// Numeric items up to 64 bits.
    Int(u64),
    /// ASCII items and items longer than 8 bytes; serialized as uppercase hex.
    Bytes(HexBytes),
}

/// One decoded data item of a response.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecodedItem {
    pub name: String,
    /// `None` when the response is too short for this item (ddt4all returns `None` too).
    pub raw: Option<Raw>,
    pub value: Option<Value>,
    /// The string DDT2000's `DiagItem.Text` (or, under [`Semantics::Ddt4all`], ddt4all's
    /// `getDisplayValue`) would show.
    pub display: Option<String>,
    pub unit: Option<String>,
}

impl DecodedItem {
    /// The raw integer of a numeric item (`None` for byte-array items, or when the frame was
    /// too short).
    pub fn raw_int(&self) -> Option<u64> {
        match self.raw {
            Some(Raw::Int(v)) => Some(v),
            _ => None,
        }
    }
}

pub(crate) fn parse_hex(s: &str) -> Result<Vec<u8>, SpecError> {
    let t = s.trim_matches(|c: char| c.is_ascii_whitespace());
    if t.len() % 2 != 0 || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(SpecError::InvalidHex(s.to_string()));
    }
    Ok((0..t.len())
        .step_by(2)
        .map(|i| u8::from_str_radix(&t[i..i + 2], 16).expect("validated hex"))
        .collect())
}

pub(crate) fn to_hex_upper(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02X}")).collect()
}

fn to_hex_lower(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

fn get_bit_msb(bytes: &[u8], pos: usize) -> bool {
    bytes[pos / 8] >> (7 - pos % 8) & 1 == 1
}

fn set_bit_msb(bytes: &mut [u8], pos: usize, v: bool) {
    let mask = 1u8 << (7 - pos % 8);
    if v {
        bytes[pos / 8] |= mask
    } else {
        bytes[pos / 8] &= !mask
    }
}

/// Bit `pos` counted from the least significant bit of the whole big-endian span.
fn get_bit_lsb(bytes: &[u8], pos: usize) -> bool {
    bytes[bytes.len() - 1 - pos / 8] >> (pos % 8) & 1 == 1
}

fn set_bit_lsb(bytes: &mut [u8], pos: usize, v: bool) {
    let i = bytes.len() - 1 - pos / 8;
    let mask = 1u8 << (pos % 8);
    if v {
        bytes[i] |= mask
    } else {
        bytes[i] &= !mask
    }
}

/// Unsigned big-endian integer of the bytes, if it fits.
fn be_u128(b: &[u8]) -> Option<u128> {
    let start = b.iter().position(|&x| x != 0).unwrap_or(b.len());
    if b.len() - start > 16 {
        return None;
    }
    Some(b[start..].iter().fold(0u128, |a, &x| a << 8 | x as u128))
}

fn be_f64(b: &[u8]) -> f64 {
    match be_u128(b) {
        Some(v) => v as f64,
        None => b.iter().fold(0f64, |a, &x| a * 256.0 + x as f64),
    }
}

/// Python `-(v & 0x80) | (v & 0x7f)` (and the 16-bit variant): keeps only the low byte(s).
fn sign_extend(v: u128, width: u32) -> i128 {
    let sign = 1u128 << (width - 1);
    let low = v & (sign - 1);
    if v & sign != 0 {
        low as i128 - sign as i128
    } else {
        low as i128
    }
}

/// Python `str(float)` (repr) for a finite, non-integral value.
pub(crate) fn python_repr(x: f64) -> String {
    if !x.is_finite() {
        return if x.is_nan() {
            "nan".into()
        } else if x > 0.0 {
            "inf".into()
        } else {
            "-inf".into()
        };
    }
    let sci = format!("{x:e}"); // shortest round-trip digits, e.g. "-3.9375e1"
    let (mant, exp) = sci.split_once('e').expect("{:e} has an exponent");
    let exp: i32 = exp.parse().expect("{:e} exponent is an integer");
    let (neg, mant) = match mant.strip_prefix('-') {
        Some(m) => (true, m),
        None => (false, mant),
    };
    let digits: String = mant.chars().filter(|c| *c != '.').collect();
    let decpt = exp + 1; // value = 0.DIGITS * 10^decpt
    let body = if decpt <= -4 || decpt > 16 {
        let mut s = digits[..1].to_string();
        if digits.len() > 1 {
            s.push('.');
            s.push_str(&digits[1..]);
        }
        format!("{s}e{}{:02}", if exp < 0 { '-' } else { '+' }, exp.abs())
    } else if decpt <= 0 {
        format!("0.{}{}", "0".repeat((-decpt) as usize), digits)
    } else if decpt as usize >= digits.len() {
        format!("{}{}.0", digits, "0".repeat(decpt as usize - digits.len()))
    } else {
        format!(
            "{}.{}",
            &digits[..decpt as usize],
            &digits[decpt as usize..]
        )
    };
    if neg {
        format!("-{body}")
    } else {
        body
    }
}

/// ddt4all display of a scaled result: `'%.Nf'` when the format has a `.`, else the integer
/// if the value is integral, else Python `str(float)`.
pub(crate) fn display_scaled(res: f64, format: Option<&str>) -> String {
    if let Some(f) = format {
        if let Some((_, after)) = f.split_once('.') {
            let acc = after.split('.').next().unwrap_or("").chars().count();
            return format!("{res:.acc$}");
        }
    }
    if res.is_finite() && res.trunc() == res {
        if res == 0.0 {
            return "0".into(); // str(int(-0.0)) == "0"
        }
        return format!("{res:.0}");
    }
    python_repr(res)
}

/// Python `bytes.decode('utf-8', errors='ignore')`.
fn utf8_ignore(mut b: &[u8]) -> String {
    let mut out = String::new();
    loop {
        match std::str::from_utf8(b) {
            Ok(s) => {
                out.push_str(s);
                return out;
            }
            Err(e) => {
                let (ok, rest) = b.split_at(e.valid_up_to());
                out.push_str(std::str::from_utf8(ok).expect("valid prefix"));
                match e.error_len() {
                    Some(n) => b = &rest[n..],
                    None => return out,
                }
            }
        }
    }
}

fn encode_err(d: &DataDef, reason: impl Into<String>) -> SpecError {
    SpecError::Encode {
        data: d.name.clone(),
        reason: reason.into(),
    }
}

fn effective_endian(item: &DataItem, request_endian: Endian) -> Endian {
    item.endian.unwrap_or(request_endian)
}

const UNIT_SCALE: Scaled = Scaled {
    step: 1.0,
    offset: 0.0,
    divide_by: 1.0,
    format: None,
    unit: None,
};

impl DataDef {
    /// Length in bytes of the raw value (`ceil(bits_count / 8)`).
    pub fn raw_len(&self) -> usize {
        (self.bits_count as usize).div_ceil(8)
    }

    /// The item is a byte array (shown as text or hex, never scaled).
    ///
    /// DDT2000 (RE: KWP2000Diag4 FUN_2301e4d0): with `<Bits>`, only an unsigned item of 32 bits
    /// or more; without `<Bits>`, a `<Bytes>` item. ddt4all: ASCII items and items over 8 bytes
    /// (the [`Raw::Bytes`] cases).
    pub fn is_byte_array(&self) -> bool {
        match self.semantics {
            Semantics::Ddt2000 if self.has_bits => self.bits_count >= 32 && !self.signed,
            Semantics::Ddt2000 => self.is_bytes,
            Semantics::Ddt4all => self.ascii || self.raw_len() > 8,
        }
    }

    /// The linear conversion actually applied, if any.
    ///
    /// DDT2000: a numeric item without `<List>` and with more than 1 bit is always scaled, with
    /// Step 1 / Offset 0 / DivideBy 1 when `<Scaled>` is absent; a `<List>` wins over
    /// `<Scaled>` (RE: KWP2000Diag4 FUN_2301e4d0, list kind called without coefficients) and a
    /// 1-bit item is never scaled (RE: KWP2000Diag4 InitNumeric 0x2306e1a0, `1 < bits`).
    /// ddt4all: `<Scaled>` as written.
    pub fn scaling(&self) -> Option<&Scaled> {
        match self.semantics {
            Semantics::Ddt4all => self.scaled.as_ref(),
            Semantics::Ddt2000 => {
                if self.is_byte_array() || !self.list.is_empty() || self.bits_count <= 1 {
                    None
                } else {
                    Some(self.scaled.as_ref().unwrap_or(&UNIT_SCALE))
                }
            }
        }
    }

    /// Unit of a scaled value.
    pub fn unit(&self) -> Option<&str> {
        self.scaling().and_then(|s| s.unit.as_deref())
    }

    /// Width used for sign extension: ddt4all only handles 1- and 2-byte signed values at bit
    /// 7/15; DDT2000 sign-extends at the field width.
    fn sign_width(&self) -> Option<u32> {
        match (self.semantics, self.signed, self.bytes_count) {
            (Semantics::Ddt2000, true, _) if !self.is_byte_array() => Some(self.bits_count),
            (Semantics::Ddt2000, _, _) => None,
            (Semantics::Ddt4all, true, 1) => Some(8),
            (Semantics::Ddt4all, true, 2) => Some(16),
            _ => None,
        }
    }

    /// Cut this item's raw bytes out of a frame (ddt4all `getHexValue`). The result is the
    /// value as `raw_len()` big-endian bytes; little-endian items are byte-reversed.
    /// `None` when the frame is too short or the item position is invalid.
    pub fn extract(
        &self,
        frame: &[u8],
        item: &DataItem,
        request_endian: Endian,
    ) -> Option<Vec<u8>> {
        let bits = self.bits_count as usize;
        let off = item.bit_offset as usize;
        let n = self.raw_len();
        let span_len = (bits + off).div_ceil(8);
        if item.first_byte == 0 || bits == 0 {
            return None;
        }
        let start = item.first_byte as usize - 1;
        if self.semantics == Semantics::Ddt2000 {
            if self.is_byte_array() {
                return frame.get(start..start.checked_add(n)?).map(<[u8]>::to_vec);
            }
            if effective_endian(item, request_endian) == Endian::Little {
                let v = ddt2000::le_get(frame, item.first_byte, item.bit_offset, self.bits_count)?;
                return Some(v.to_be_bytes()[8 - n..].to_vec());
            }
        }
        let span = frame.get(start..start.checked_add(span_len)?)?;
        let mut out = vec![0u8; n];
        match effective_endian(item, request_endian) {
            Endian::Big => {
                // bits [off, off+bits) counted from the MSB of the span, right-aligned.
                let pad = n * 8 - bits;
                for i in 0..bits {
                    set_bit_msb(&mut out, pad + i, get_bit_msb(span, off + i));
                }
            }
            Endian::Little => {
                // (span as big-endian int >> off) & mask, then byte-reversed.
                for i in 0..bits {
                    set_bit_lsb(&mut out, i, get_bit_lsb(span, off + i));
                }
                out.reverse();
            }
        }
        Some(out)
    }

    /// Wrap raw bytes (as returned by [`extract`](Self::extract)) in a [`Raw`].
    pub fn raw_from_bytes(&self, raw: &[u8]) -> Raw {
        if self.is_byte_array() || raw.len() > 8 {
            Raw::Bytes(HexBytes(raw.to_vec()))
        } else {
            Raw::Int(raw.iter().fold(0u64, |a, &x| a << 8 | x as u64))
        }
    }

    /// `raw_len()` big-endian bytes of a [`Raw`].
    pub fn raw_to_bytes(&self, raw: &Raw) -> Result<Vec<u8>, SpecError> {
        let n = self.raw_len();
        match raw {
            Raw::Bytes(b) if b.0.len() == n => Ok(b.0.clone()),
            Raw::Bytes(b) => Err(encode_err(
                self,
                format!("{} raw bytes given, {n} expected", b.0.len()),
            )),
            Raw::Int(v) => {
                let full = v.to_be_bytes();
                let lead = full.len().saturating_sub(n);
                if full[..lead].iter().any(|&x| x != 0) {
                    return Err(encode_err(
                        self,
                        format!("raw {v} does not fit in {n} bytes"),
                    ));
                }
                let mut out = vec![0u8; n.saturating_sub(8)];
                out.extend_from_slice(&full[lead..]);
                Ok(out)
            }
        }
    }

    /// DDT2000 `DiagItem.Text` (RE: KWP2000Diag4 0x2306f520).
    fn interpret_ddt2000(&self, raw: &[u8]) -> Option<(Value, String)> {
        if self.is_byte_array() {
            if self.ascii {
                let s = ddt2000::cp1252(raw);
                return Some((Value::Text(s.clone()), s));
            }
            let h = ddt2000::hex_upper(raw);
            return Some((Value::Hex(h.clone()), h));
        }
        let u = raw.iter().fold(0u64, |a, &x| a << 8 | u64::from(x));
        let v = match self.sign_width() {
            Some(w) => ddt2000::sign_extend(u, w),
            None => u as i64,
        };
        if !self.list.is_empty() {
            return Some(match self.list.iter().rev().find(|i| i.value == v) {
                Some(item) => (Value::Text(item.text.clone()), item.text.clone()),
                None => {
                    let h = ddt2000::vb_hex(v);
                    let d = format!("##{h}##");
                    (Value::Hex(h), d)
                }
            });
        }
        let Some(sc) = self.scaling() else {
            // 1-bit item: CStr(value).
            return Some((Value::Number(v as f64), v.to_string()));
        };
        if sc.divide_by == 0.0 {
            // VB raises a division-by-zero error; no value.
            return None;
        }
        let res = (v as f64 * sc.step + sc.offset) / sc.divide_by;
        let text = match sc.format.as_deref() {
            Some("bin") => ddt2000::bin_string(res, self.bits_count),
            Some(f) => ddt2000::vb_format(res, f),
            None => ddt2000::vb_general(res),
        };
        Some((Value::Number(res), text))
    }

    /// Convert raw bytes to a value and its display string: DDT2000 `DiagItem.Text`, or
    /// ddt4all `getDisplayValue` under [`Semantics::Ddt4all`].
    /// Returns `None` only for a scaled item with `DivideBy` 0 (ddt4all returns `None` too).
    pub fn interpret(&self, raw: &[u8]) -> Option<(Value, String)> {
        if self.semantics == Semantics::Ddt2000 {
            return self.interpret_ddt2000(raw);
        }
        if self.ascii {
            let s = utf8_ignore(raw);
            return Some((Value::Text(s.clone()), s));
        }
        let unsigned = be_u128(raw);
        let signed: Option<i128> = match (unsigned, self.sign_width()) {
            (Some(v), Some(w)) => Some(sign_extend(v, w)),
            (Some(v), None) => i128::try_from(v).ok(),
            (None, _) => None,
        };
        match &self.scaled {
            None => {
                if let Some(v) = signed {
                    if let Some(item) = self.list.iter().rev().find(|i| i.value as i128 == v) {
                        return Some((Value::Text(item.text.clone()), item.text.clone()));
                    }
                }
                let h = to_hex_lower(raw);
                Some((Value::Hex(h.clone()), h))
            }
            Some(sc) => {
                if sc.divide_by == 0.0 {
                    return None;
                }
                let v = match signed {
                    Some(v) => v as f64,
                    None => be_f64(raw),
                };
                let res = (v * sc.step + sc.offset) / sc.divide_by;
                Some((
                    Value::Number(res),
                    display_scaled(res, sc.format.as_deref()),
                ))
            }
        }
    }

    /// Decode this data at `item`'s position in a frame.
    pub fn decode(&self, frame: &[u8], item: &DataItem, request_endian: Endian) -> DecodedItem {
        let raw = self.extract(frame, item, request_endian);
        let interp = raw.as_deref().and_then(|r| self.interpret(r));
        DecodedItem {
            name: item.name.clone(),
            raw: raw.as_deref().map(|r| self.raw_from_bytes(r)),
            value: interp.as_ref().map(|(v, _)| v.clone()),
            display: interp.map(|(_, d)| d),
            unit: self.unit().map(str::to_string),
        }
    }

    /// Range check and two's-complement mapping of an integer raw value (up to 64 bits).
    fn int_to_raw(&self, v: i128) -> Result<Raw, SpecError> {
        let bits = self.bits_count;
        if bits == 0 || bits > 64 {
            return Err(encode_err(self, "numeric encode needs 1..=64 bits"));
        }
        let (min, max) = match self.sign_width() {
            Some(w) if w == bits => (-(1i128 << (w - 1)), (1i128 << (w - 1)) - 1),
            // Sign bit outside the field: only values that decode back positive are allowed.
            Some(w) => (0, ((1i128 << bits) - 1).min((1i128 << (w - 1)) - 1)),
            None => (0, (1i128 << bits) - 1),
        };
        if v < min || v > max {
            return Err(encode_err(self, format!("raw {v} outside {min}..={max}")));
        }
        let u = if v < 0 { v + (1i128 << bits) } else { v } as u64;
        let bytes = u.to_be_bytes();
        Ok(self.raw_from_bytes(&bytes[8 - self.raw_len()..]))
    }

    /// Value -> raw. Inverse of [`interpret`](Self::interpret) for every raw value:
    /// `Number` for scaled data (rounded to the nearest raw step, ties to even under DDT2000), `Text` for list labels
    /// and ASCII data, `Hex` for raw hex input.
    pub fn encode(&self, value: &Value) -> Result<Raw, SpecError> {
        match value {
            Value::Text(s) if self.ascii && self.is_byte_array_for_text() => {
                if !s.is_ascii() {
                    return Err(encode_err(self, "ASCII data accepts only ASCII text"));
                }
                // ddt4all: ljust with spaces, then truncate to bytes_count.
                let mut b: Vec<u8> = s.bytes().take(self.bytes_count as usize).collect();
                b.resize(self.bytes_count as usize, b' ');
                if b.len() != self.raw_len() {
                    return Err(encode_err(self, "ASCII length does not match bit count"));
                }
                Ok(Raw::Bytes(HexBytes(b)))
            }
            Value::Text(label) => {
                let item = self
                    .list
                    .iter()
                    .rev()
                    .find(|i| &i.text == label)
                    .ok_or_else(|| encode_err(self, format!("no list item {label:?}")))?;
                self.int_to_raw(item.value as i128)
            }
            Value::Hex(h) => {
                let t = h.trim();
                let t = t.strip_prefix("0x").unwrap_or(t);
                if t.is_empty() || !t.bytes().all(|b| b.is_ascii_hexdigit()) {
                    return Err(SpecError::InvalidHex(h.clone()));
                }
                let width = self.raw_len() * 2;
                let digits = t.trim_start_matches('0');
                if digits.len() > width {
                    return Err(encode_err(
                        self,
                        format!("hex {h:?} wider than {width} digits"),
                    ));
                }
                let b = parse_hex(&format!("{digits:0>width$}"))?;
                let pad = b.len() * 8 - self.bits_count as usize;
                if (0..pad).any(|i| get_bit_msb(&b, i)) {
                    return Err(encode_err(
                        self,
                        format!("hex {h:?} wider than {} bits", self.bits_count),
                    ));
                }
                Ok(self.raw_from_bytes(&b))
            }
            Value::Number(x) => {
                if !x.is_finite() {
                    return Err(encode_err(self, "value is not finite"));
                }
                let r = match self.scaling() {
                    Some(sc) => {
                        if sc.step == 0.0 || sc.divide_by == 0.0 {
                            return Err(encode_err(self, "Step or DivideBy is 0"));
                        }
                        let r = (x * sc.divide_by - sc.offset) / sc.step;
                        // DDT2000: `__vbaFpI4` (round half to even), RE: DiagLibs2 ScaledValue
                        // Let 0x1100ec40. ddt4all mode keeps rounding half away from zero.
                        if self.semantics == Semantics::Ddt2000 {
                            r.round_ties_even()
                        } else {
                            r.round()
                        }
                    }
                    None => {
                        if x.trunc() != *x {
                            return Err(encode_err(self, "unscaled data needs an integer"));
                        }
                        *x
                    }
                };
                if r.abs() >= 2f64.powi(100) {
                    return Err(encode_err(self, "value out of range"));
                }
                self.int_to_raw(r as i128)
            }
        }
    }

    /// Like [`encode`](Self::encode), but a `Number` for scaled data must lie on the
    /// `Step`/`DivideBy` grid: `(x * DivideBy - Offset) / Step` may differ from an integer by
    /// at most [`GRID_TOLERANCE`](Self::GRID_TOLERANCE) steps (float noise), otherwise
    /// [`SpecError::OffGrid`]. `encode` would round such a value to the nearest step
    /// (1503 rpm with Step 2 becomes 1504).
    pub fn encode_exact(&self, value: &Value) -> Result<Raw, SpecError> {
        // Range first, so an out-of-range value is an Encode error and `nearest` of OffGrid is
        // always encodable.
        let raw = self.encode(value)?;
        if let (Value::Number(x), Some(sc)) = (value, self.scaling()) {
            if x.is_finite() && sc.step != 0.0 && sc.divide_by != 0.0 {
                let r = (x * sc.divide_by - sc.offset) / sc.step;
                if (r - r.round()).abs() > Self::GRID_TOLERANCE {
                    return Err(SpecError::OffGrid {
                        data: self.name.clone(),
                        value: *x,
                        nearest: (r.round() * sc.step + sc.offset) / sc.divide_by,
                    });
                }
            }
        }
        Ok(raw)
    }

    /// Largest distance from the grid, in steps, that [`encode_exact`](Self::encode_exact)
    /// accepts. Every decoded value of the car's definitions re-encodes with it
    /// (tests/roundtrip_prop.rs).
    pub const GRID_TOLERANCE: f64 = 1e-6;

    fn is_byte_array_for_text(&self) -> bool {
        self.semantics == Semantics::Ddt4all || self.is_byte_array()
    }

    /// Write a raw value into a frame at `item`'s position (inverse of
    /// [`extract`](Self::extract)). Bits outside the item are left untouched.
    pub fn insert(
        &self,
        raw: &Raw,
        frame: &mut [u8],
        item: &DataItem,
        request_endian: Endian,
    ) -> Result<(), SpecError> {
        let mut val = self.raw_to_bytes(raw)?;
        let bits = self.bits_count as usize;
        let off = item.bit_offset as usize;
        let span_len = (bits + off).div_ceil(8);
        let len = frame.len();
        let out_of_frame = |end| SpecError::OutOfFrame {
            item: item.name.clone(),
            first_byte: item.first_byte,
            end,
            len,
        };
        if item.first_byte == 0 || bits == 0 {
            return Err(out_of_frame(0));
        }
        let start = item.first_byte as usize - 1;
        if self.semantics == Semantics::Ddt2000 {
            if self.is_byte_array() {
                let end = start + val.len();
                frame
                    .get_mut(start..end)
                    .ok_or(out_of_frame(end))?
                    .copy_from_slice(&val);
                return Ok(());
            }
            if effective_endian(item, request_endian) == Endian::Little {
                let v = val.iter().fold(0u64, |a, &x| a << 8 | u64::from(x));
                if v > ddt2000::mask(self.bits_count) {
                    return Err(encode_err(self, "raw wider than bit count"));
                }
                let end = start
                    + ((item.bit_offset % 8 + self.bits_count - 1) / 8 + 1) as usize
                    + (item.bit_offset / 8) as usize;
                return ddt2000::le_set(
                    frame,
                    item.first_byte,
                    item.bit_offset,
                    self.bits_count,
                    v,
                )
                .ok_or(out_of_frame(end));
            }
        }
        let end = start + span_len;
        let span = frame.get_mut(start..end).ok_or(out_of_frame(end))?;
        let n = val.len();
        match effective_endian(item, request_endian) {
            Endian::Big => {
                let pad = n * 8 - bits;
                if (0..pad).any(|i| get_bit_msb(&val, i)) {
                    return Err(encode_err(self, "raw wider than bit count"));
                }
                for i in 0..bits {
                    set_bit_msb(span, off + i, get_bit_msb(&val, pad + i));
                }
            }
            Endian::Little => {
                val.reverse();
                if (bits..n * 8).any(|i| get_bit_lsb(&val, i)) {
                    return Err(encode_err(
                        self,
                        "little-endian raw does not fit the bit count",
                    ));
                }
                for i in 0..bits {
                    set_bit_lsb(span, off + i, get_bit_lsb(&val, i));
                }
            }
        }
        Ok(())
    }
}

impl Request {
    /// Decode every received data item from a response frame **without validating it**.
    ///
    /// The frame starts with the positive response SID (e.g. `0x73` for `0x33`). Nothing is
    /// checked: a negative response, a wrong SID or a frame shorter than `MinBytes` is decoded
    /// as if it were valid, and items the frame is too short for come back with
    /// `raw`/`value` = `None` (this is exactly ddt4all's `get_values_from_stream`).
    ///
    /// Use it only where the frame is known to be a positive response to this request, or to
    /// inspect arbitrary bytes on purpose: differential tests against ddt4all, replaying
    /// logged frames for analysis, decoding a canned `ReplyBytes`. Live ECU traffic goes through
    /// [`decode_checked`](Self::decode_checked).
    pub fn decode_unchecked(
        &self,
        ecu: &EcuDef,
        response: &[u8],
    ) -> Result<Vec<DecodedItem>, SpecError> {
        self.received_items
            .iter()
            .map(|item| {
                let data = ecu
                    .data(&item.name)
                    .ok_or_else(|| SpecError::UnknownData(item.name.clone()))?;
                Ok(data.decode(response, item, ecu.endian))
            })
            .collect()
    }

    /// Validate `response` as the reply to this request sent with its default `SentBytes`,
    /// then decode it. See [`decode_checked_against`](Self::decode_checked_against).
    pub fn decode_checked(
        &self,
        ecu: &EcuDef,
        response: &[u8],
    ) -> Result<Vec<DecodedItem>, SpecError> {
        self.decode_checked_against(ecu, &self.sent_bytes.0, response)
    }

    /// Validate `response` as the reply to the frame `sent` (e.g. from [`build`](Self::build)),
    /// then decode it. Checks, in this order:
    ///
    /// 1. empty response: [`SpecError::TooShort`];
    /// 2. `7F <sid> <nrc>`: [`SpecError::NegativeResponse`] if `sid` is `sent[0]` (or `sent` is
    ///    empty), else [`SpecError::NegativeResponseForOtherSid`]; shorter than 3 bytes:
    ///    [`SpecError::TruncatedNegativeResponse`];
    /// 3. first byte must be `sent[0] + 0x40`: [`SpecError::UnexpectedSid`]
    ///    ([`SpecError::NoPositiveSid`] if `sent` is empty or its SID is 0xC0 or above). For an
    ///    ISO8 definition (`<K><ISO8>`) the request SID itself is accepted too: the Bosch 5.3
    ///    ABS answers `51` with `51 05 ..` and `58` with `58 ..` (car capture 2026-11-23,
    ///    docs/protocol.md "Bosch 5.3 ABS"), and DDT2000's ISO8 reply parser does not check the
    ///    SID at all (RE: KWP2000Diag4 ISO8 parser FUN_2304ea50);
    /// 4. length must be at least `Received@MinBytes`: [`SpecError::TooShort`]
    ///    (see [`decode_checked_allow_short`](Self::decode_checked_allow_short) to relax this);
    /// 5. KWP2000 local-identifier services (`21`, `30`, `31`, `32`, `33`, `3B`) answer
    ///    `<SID+0x40> <local id> ...`, so response byte 2 must equal `sent[1]`
    ///    (`21 A9` is answered by `61 A9 ...`): [`SpecError::ReplyMismatch`]. For SID `21`
    ///    this is always checked (the Sirius 32 definitions even map byte 2 as an item,
    ///    `LocalIvinOK`, which would otherwise hide a reply to a different request). For the
    ///    other services it is checked only when no received item maps byte 2 (the S2000 `31`
    ///    requests do), because there the definition may use that byte for data.
    ///    Bytes after byte 2 are never compared with `sent`: positions before the first mapped
    ///    item are not an echo of the request parameters.
    ///
    /// `<ReplyBytes>` is **not** used: ddt4all uses it only as the canned answer of its
    /// simulation mode, and in our files it holds zeros where the ECU echoes a parameter
    /// (see docs/spec-semantics.md).
    pub fn decode_checked_against(
        &self,
        ecu: &EcuDef,
        sent: &[u8],
        response: &[u8],
    ) -> Result<Vec<DecodedItem>, SpecError> {
        self.check_response(is_iso8(ecu), sent, response, true)?;
        self.decode_unchecked(ecu, response)
    }

    /// Like [`decode_checked_against`](Self::decode_checked_against) with the request's default
    /// `SentBytes`, but a response shorter than `Received@MinBytes` is not an error: items the
    /// frame is too short for decode to `raw`/`value` = `None`.
    ///
    /// `MinBytes` is one number per request while a definition can span many ECU software
    /// versions (`AutoIdent`); an older ECU may legitimately answer with a shorter frame. The
    /// SID, negative-response and echo checks still apply (the echo byte must be present).
    pub fn decode_checked_allow_short(
        &self,
        ecu: &EcuDef,
        response: &[u8],
    ) -> Result<Vec<DecodedItem>, SpecError> {
        self.check_response(is_iso8(ecu), &self.sent_bytes.0, response, false)?;
        self.decode_unchecked(ecu, response)
    }

    fn check_response(
        &self,
        iso8: bool,
        sent: &[u8],
        response: &[u8],
        enforce_min_bytes: bool,
    ) -> Result<(), SpecError> {
        let len = response.len();
        let Some(&first) = response.first() else {
            return Err(SpecError::TooShort {
                min: (self.min_bytes as usize).max(1),
                len,
            });
        };
        if first == 0x7F {
            return match response {
                [_, sid, nrc, ..] => match sent.first() {
                    Some(&expected) if expected != *sid => {
                        Err(SpecError::NegativeResponseForOtherSid {
                            expected,
                            got: *sid,
                            nrc: *nrc,
                        })
                    }
                    _ => Err(SpecError::NegativeResponse {
                        sid: *sid,
                        nrc: *nrc,
                    }),
                },
                _ => Err(SpecError::TruncatedNegativeResponse { len }),
            };
        }
        let expected = sent
            .first()
            .and_then(|sid| sid.checked_add(0x40))
            .ok_or_else(|| SpecError::NoPositiveSid {
                request: self.name.clone(),
            })?;
        let iso8_echo = iso8 && sent.first() == Some(&first);
        if first != expected && !iso8_echo {
            return Err(SpecError::UnexpectedSid {
                expected,
                got: first,
            });
        }
        let min = self.min_bytes as usize;
        if enforce_min_bytes && len < min {
            return Err(SpecError::TooShort { min, len });
        }
        let byte2_mapped = self.received_items.iter().any(|i| i.first_byte == 2);
        let echoes_local_id = match sent[0] {
            0x21 => true,
            0x30..=0x33 | 0x3B => !byte2_mapped,
            _ => false,
        };
        if echoes_local_id && sent.len() >= 2 {
            match response.get(1) {
                None => return Err(SpecError::TooShort { min: 2, len }),
                Some(&got) if got != sent[1] => {
                    return Err(SpecError::ReplyMismatch {
                        position: 2,
                        expected: sent[1],
                        got,
                    })
                }
                Some(_) => {}
            }
        }
        Ok(())
    }

    /// Build the request frame: `SentBytes` with the given sent data items overwritten.
    pub fn build(&self, ecu: &EcuDef, inputs: &[(&str, Value)]) -> Result<Vec<u8>, SpecError> {
        let mut frame = self.sent_bytes.0.clone();
        for (name, value) in inputs {
            let item = self
                .sent_items
                .iter()
                .find(|i| i.name == *name)
                .ok_or_else(|| SpecError::UnknownSentItem {
                    request: self.name.clone(),
                    item: name.to_string(),
                })?;
            let data = ecu
                .data(name)
                .ok_or_else(|| SpecError::UnknownData(name.to_string()))?;
            let raw = data.encode(value)?;
            data.insert(&raw, &mut frame, item, ecu.endian)?;
        }
        Ok(frame)
    }
}

/// Whether `ecu` is an ISO8 definition (`<K><ISO8>` without `<KWP>`).
fn is_iso8(ecu: &EcuDef) -> bool {
    ecu.k
        .as_ref()
        .is_some_and(|k| k.protocol() == crate::model::KProtocol::Iso8)
}
