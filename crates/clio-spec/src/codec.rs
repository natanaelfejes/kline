//! Bit extraction and value conversion, matching ddt4all `EcuData.getHexValue`,
//! `getDisplayValue` and (for big endian) `setValue`. See docs/spec-semantics.md.

use serde::Serialize;

use crate::error::SpecError;
use crate::model::{DataDef, DataItem, EcuDef, Endian, HexBytes, Request};

/// A decoded (or to-be-encoded) value.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", content = "value", rename_all = "snake_case")]
pub enum Value {
    /// Result of a `<Scaled>` conversion.
    Number(f64),
    /// A `<List>` label, or the text of an ASCII `<Bytes>` item.
    Text(String),
    /// Unscaled value with no matching list entry: lowercase hex of the raw bytes, exactly as
    /// ddt4all displays it.
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
    /// The string ddt4all's `getDisplayValue` would show.
    pub display: Option<String>,
    pub unit: Option<String>,
}

impl DecodedItem {
    /// The raw integer of a numeric item (`None` for ASCII or wider than 8 bytes, or when the
    /// frame was too short). The display ignores `Format="bin"` / `Format="00"`
    /// (docs/spec-semantics.md), so a caller that wants binary or zero-padded output formats
    /// this value itself.
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

impl DataDef {
    /// Length in bytes of the raw value (`ceil(bits_count / 8)`).
    pub fn raw_len(&self) -> usize {
        (self.bits_count as usize).div_ceil(8)
    }

    /// Unit of a scaled value.
    pub fn unit(&self) -> Option<&str> {
        self.scaled.as_ref().and_then(|s| s.unit.as_deref())
    }

    /// Width used for sign extension: ddt4all only handles 1- and 2-byte signed values.
    fn sign_width(&self) -> Option<u32> {
        match (self.signed, self.bytes_count) {
            (true, 1) => Some(8),
            (true, 2) => Some(16),
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
        if self.ascii || raw.len() > 8 {
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

    /// Convert raw bytes to a value and its ddt4all display string (`getDisplayValue`).
    /// Returns `None` only for a scaled item with `DivideBy` 0 (ddt4all returns `None` too).
    pub fn interpret(&self, raw: &[u8]) -> Option<(Value, String)> {
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
    /// `Number` for scaled data (rounded to the nearest raw step), `Text` for list labels
    /// and ASCII data, `Hex` for raw hex input.
    pub fn encode(&self, value: &Value) -> Result<Raw, SpecError> {
        match value {
            Value::Text(s) if self.ascii => {
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
                let r = match &self.scaled {
                    Some(sc) => {
                        if sc.step == 0.0 || sc.divide_by == 0.0 {
                            return Err(encode_err(self, "Step or DivideBy is 0"));
                        }
                        ((x * sc.divide_by - sc.offset) / sc.step).round()
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
        if let (Value::Number(x), Some(sc)) = (value, &self.scaled) {
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
        self.encode(value)
    }

    /// Largest distance from the grid, in steps, that [`encode_exact`](Self::encode_exact)
    /// accepts. Every decoded value of the car's definitions re-encodes with it
    /// (tests/roundtrip_prop.rs).
    pub const GRID_TOLERANCE: f64 = 1e-6;

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
    /// 2. `7F <sid> <nrc>`: [`SpecError::NegativeResponse`] (shorter than 3 bytes: `TooShort`);
    /// 3. first byte must be `sent[0] + 0x40`: [`SpecError::UnexpectedSid`]
    ///    ([`SpecError::NoPositiveSid`] if `sent` is empty or its SID is 0xC0 or above);
    /// 4. length must be at least `Received@MinBytes`: [`SpecError::TooShort`];
    /// 5. the bytes after the SID and before the first received data item must echo `sent`
    ///    (e.g. the local identifier `A3` of `21 A3` in `61 A3 ...`): [`SpecError::ReplyMismatch`].
    ///    Only positions that exist in `sent` are compared. Requests without received items
    ///    skip this step.
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
        let len = response.len();
        let Some(&first) = response.first() else {
            return Err(SpecError::TooShort {
                min: (self.min_bytes as usize).max(1),
                len,
            });
        };
        if first == 0x7F {
            return match response {
                [_, sid, nrc, ..] => Err(SpecError::NegativeResponse {
                    sid: *sid,
                    nrc: *nrc,
                }),
                _ => Err(SpecError::TooShort { min: 3, len }),
            };
        }
        let expected = sent
            .first()
            .and_then(|sid| sid.checked_add(0x40))
            .ok_or_else(|| SpecError::NoPositiveSid {
                request: self.name.clone(),
            })?;
        if first != expected {
            return Err(SpecError::UnexpectedSid {
                expected,
                got: first,
            });
        }
        let min = self.min_bytes as usize;
        if len < min {
            return Err(SpecError::TooShort { min, len });
        }
        if let Some(first_item) = self.received_items.iter().map(|i| i.first_byte).min() {
            // 1-based positions 2 ..= echo_end
            let echo_end = sent.len().min(first_item as usize - 1);
            if len < echo_end {
                return Err(SpecError::TooShort { min: echo_end, len });
            }
            let echo = response.iter().zip(sent).enumerate().take(echo_end);
            if let Some((i, (&got, &expected))) = echo.skip(1).find(|(_, (g, e))| g != e) {
                return Err(SpecError::ReplyMismatch {
                    position: i + 1,
                    expected,
                    got,
                });
            }
        }
        self.decode_unchecked(ecu, response)
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
