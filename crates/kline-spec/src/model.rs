//! Typed model of a DDT2000 ECU definition file. See docs/spec-semantics.md.

use serde::{Serialize, Serializer};

/// The DDT2000 ECU namespace used by every `<Target>` element.
pub const ECU_NAMESPACE: &str = "http://www-diag.renault.com/2002/ECU";

/// A byte string that serializes as an uppercase hex string (`"3351"`).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HexBytes(pub Vec<u8>);

impl HexBytes {
    /// Parse an even-length hex string; ASCII whitespace at either end is ignored.
    pub fn from_hex(s: &str) -> Result<Self, crate::SpecError> {
        crate::codec::parse_hex(s).map(HexBytes)
    }

    /// Uppercase hex, no separators.
    pub fn to_hex(&self) -> String {
        crate::codec::to_hex_upper(&self.0)
    }
}

pub(crate) fn ser_hex<S: Serializer>(b: &[u8], s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&crate::codec::to_hex_upper(b))
}

impl Serialize for HexBytes {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&self.to_hex())
    }
}

/// Byte order of multi-byte data items.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
pub enum Endian {
    #[default]
    Big,
    Little,
}

/// Which program's interpretation of the XML to follow (the author's DDT2000 behaviour notes).
///
/// - `Ddt2000` (default): DDT2000 itself, as reverse engineered from KWP2000Diag4.dll and
///   DiagLibs2.dll (signed via VB `CBool`, field-width sign extension, LSB-based little endian,
///   List before Scaled, VB `Format()` display, ...).
/// - `Ddt4all`: the ddt4all Python decoder (commit `e109cd2`), kept for
///   the author's ddt4all crosscheck script and docs/spec-semantics.md.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Semantics {
    #[default]
    Ddt2000,
    Ddt4all,
}

/// `<Failures Name Id>`: the failure model of the definition (RE: DDT2000Libxml / KWP2000Diag4
/// strings): 0 none, 1 failure flags (Sirius 32), 2 DTC read with after-sales frames, 3 DTC.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Failures {
    pub name: String,
    pub id: u32,
}

impl Failures {
    /// `Id="1"`: faults are flag items named by `<Devices>` (DiagFailures "Reading flags").
    pub fn is_flags(&self) -> bool {
        self.id == 1
    }
}

/// `<Devices><Device Name Type>`: one component and the data items that report its faults.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Device {
    pub name: String,
    /// `Device@Type` (meaning not decoded).
    pub device_type: Option<String>,
    pub data: Vec<DeviceData>,
}

/// `<DeviceData Name FailureFlag>`. RE: KWP2000Diag4 FUN_230263d0 sets
/// `IsFailureFlag = IIf(@FailureFlag = "1")` on the data item of that name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DeviceData {
    pub name: String,
    pub failure_flag: bool,
}

/// One parsed ECU definition (`<Target>`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct EcuDef {
    /// Interpretation the definition was parsed with and decodes with.
    pub semantics: Semantics,
    /// `Target@Name`, e.g. "Safir 2".
    pub name: String,
    /// `<Function Name Address>`: diagnostic function and K-line address.
    pub function: Option<Function>,
    /// Target-level `<Description>` text.
    pub description: Option<String>,
    /// Element names under `<Projects>` (vehicle codes such as `x65`).
    pub projects: Vec<String>,
    pub power_supply: Option<PowerSupply>,
    /// `<K>`: K-line protocol parameters.
    pub k: Option<KLine>,
    /// `<CAN>`: parsed when present, unused by this project.
    pub can: Option<Can>,
    pub auto_idents: Vec<AutoIdent>,
    /// `Requests@Endian`, default byte order for all data items (ddt4all: last one wins).
    pub endian: Endian,
    /// All `<Data>` definitions in document order (a repeated name replaces the earlier entry
    /// in place, like ddt4all's dict).
    pub datas: Vec<DataDef>,
    /// All `<Request>` definitions in document order (same duplicate rule as `datas`).
    pub requests: Vec<Request>,
    /// `<Failures>`.
    pub failures: Option<Failures>,
    /// `<Devices>`, in document order.
    pub devices: Vec<Device>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Function {
    pub name: String,
    /// ECU diagnostic address (e.g. 122 = 0x7A for injection, 38 = 0x26 for UCH/BMT).
    pub address: u8,
}

/// `<PowerSupply>`: which supply lines the definition lists.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PowerSupply {
    pub ground: bool,
    pub vbat: bool,
    pub after_contact: bool,
}

/// `<K>` section. Time values are the raw XML numbers; DDT2000 is assumed to use
/// milliseconds (not verified, see docs/spec-semantics.md).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct KLine {
    /// `K@BaudRate`.
    pub baud_rate: Option<u32>,
    /// `<K><ISO8>`: plain ISO 9141 5-baud init (Safir).
    pub iso8: Option<Iso8>,
    /// `<K><KWP>`: KWP2000, with either a 5-baud (`ISO8`) or a fast init.
    pub kwp: Option<Kwp>,
    /// `InterByte@Delay`.
    pub inter_byte_delay: Option<u32>,
    /// `ReplyToRequest@Delay`.
    pub reply_to_request_delay: Option<u32>,
    /// `RequestToReply@Timeout`.
    pub request_to_reply_timeout: Option<u32>,
    /// `TesterPresent@Delay`.
    pub tester_present_delay: Option<u32>,
    /// `OnError@Delay`.
    pub on_error_delay: Option<u32>,
}

/// 5-baud init parameters (`<ISO8>`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Iso8 {
    /// `<L/>` present: the L line is used during init.
    pub l_line: bool,
    pub kw1: Option<u8>,
    pub kw2: Option<u8>,
    /// `KW2@Timeout`.
    pub kw2_timeout: Option<u32>,
    /// `FirstRequest@Delay`.
    pub first_request_delay: Option<u32>,
}

/// `<KWP>` parameters.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Kwp {
    /// `KWP@Multipoint`.
    pub multipoint: Option<bool>,
    pub iso8: Option<Iso8>,
    pub fast_init: Option<FastInit>,
    /// `KW2_@Delay`.
    pub kw2_delay: Option<u32>,
}

/// `<FastInit>` key bytes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct FastInit {
    pub kw1: Option<u8>,
    pub kw2: Option<u8>,
}

/// The init/protocol family a `<K>` section describes (ddt4all `ecu_protocol` + `fastinit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum KProtocol {
    /// `<K><ISO8>`: ISO 9141 with 5-baud init.
    Iso8,
    /// `<K><KWP><ISO8>`: KWP2000 with 5-baud init.
    KwpSlowInit,
    /// `<K><KWP><FastInit>`: KWP2000 with fast init.
    KwpFastInit,
    /// Neither form found.
    Unknown,
}

impl KLine {
    /// Protocol family, following ddt4all's precedence (KWP before ISO8, FastInit before ISO8).
    pub fn protocol(&self) -> KProtocol {
        match (&self.kwp, &self.iso8) {
            (Some(kwp), _) if kwp.fast_init.is_some() => KProtocol::KwpFastInit,
            (Some(kwp), _) if kwp.iso8.is_some() => KProtocol::KwpSlowInit,
            (Some(_), _) => KProtocol::Unknown,
            (None, Some(_)) => KProtocol::Iso8,
            (None, None) => KProtocol::Unknown,
        }
    }

    /// The expected key bytes (KW1, KW2) for the selected init, if declared.
    pub fn key_words(&self) -> Option<(u8, u8)> {
        let (kw1, kw2) = match self.protocol() {
            KProtocol::KwpFastInit => {
                let f = self.kwp.as_ref()?.fast_init.as_ref()?;
                (f.kw1, f.kw2)
            }
            KProtocol::KwpSlowInit => {
                let i = self.kwp.as_ref()?.iso8.as_ref()?;
                (i.kw1, i.kw2)
            }
            KProtocol::Iso8 => {
                let i = self.iso8.as_ref()?;
                (i.kw1, i.kw2)
            }
            KProtocol::Unknown => return None,
        };
        Some((kw1?, kw2?))
    }
}

/// `<CAN>` section (parsed, unused).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Can {
    pub baud_rate: Option<u32>,
    /// `SendId/CANId@Value`.
    pub send_id: Option<u32>,
    /// `ReceiveId/CANId@Value`.
    pub receive_id: Option<u32>,
}

/// `<AutoIdent>`: identification tuple used to auto-select a definition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AutoIdent {
    pub diag_version: String,
    pub supplier: String,
    pub soft: String,
    pub version: String,
}

/// One `<Data>` definition: how to turn bits into a value.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DataDef {
    pub name: String,
    /// Interpretation used by [`DataDef::decode`] and friends (copied from [`EcuDef::semantics`]).
    pub semantics: Semantics,
    pub description: Option<String>,
    pub comment: Option<String>,
    pub mnemonic: Option<String>,
    /// Number of bits (default 8).
    pub bits_count: u32,
    /// Number of bytes as ddt4all computes it (drives signed handling).
    pub bytes_count: u32,
    /// Defined with `<Bytes>` rather than `<Bits>`.
    pub is_bytes: bool,
    /// A `<Bits>` element is present (DDT2000 picks the item kind from it, see
    /// [`DataDef::is_byte_array`]).
    pub has_bits: bool,
    /// `Bytes@ascii="1"`: decode as text.
    pub ascii: bool,
    /// `Bits@signed`: VB `CBool` under [`Semantics::Ddt2000`] (`"0"` is false), any non-empty
    /// value under [`Semantics::Ddt4all`].
    pub signed: bool,
    /// `<Binary>` present under `<Bits>`.
    pub binary: bool,
    /// `<Scaled>`: linear conversion. Absent means the raw value is shown as hex or a list label.
    pub scaled: Option<Scaled>,
    /// `<List><Item Value Text>` entries in document order.
    pub list: Vec<ListItem>,
}

/// `<Scaled>`: `value = (raw * step + offset) / divide_by`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Scaled {
    pub step: f64,
    pub offset: f64,
    pub divide_by: f64,
    /// DDT2000 display format, e.g. `#0.00`. Only the digits after the first `.` matter.
    pub format: Option<String>,
    pub unit: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ListItem {
    pub value: i64,
    pub text: String,
}

/// One `<Request>`.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Request {
    pub name: String,
    /// `<Sent><SentBytes>`: the request frame payload (service + parameters).
    pub sent_bytes: HexBytes,
    /// `<Sent><VariableLength/>` present.
    pub variable_length: bool,
    /// `<ManuelSend/>` present (DDT2000 spelling).
    pub manual_send: bool,
    /// Element names under `<DenyAccess>` (e.g. `Plant`, `AfterSales`), empty if absent.
    pub deny_access: Vec<String>,
    /// `<DossierMaintenabilite/>` present.
    pub dossier_maintenabilite: bool,
    /// `Received@MinBytes` (absent: 1 under DDT2000, 0 under ddt4all).
    pub min_bytes: u32,
    /// `<ReplyBytes>`: canned reply used by DDT2000's simulation mode.
    pub reply_bytes: Option<HexBytes>,
    /// `<ShiftBytesCount>`.
    pub shift_bytes_count: Option<u32>,
    /// Parameters written into `sent_bytes`.
    pub sent_items: Vec<DataItem>,
    /// Values read from the response.
    pub received_items: Vec<DataItem>,
}

/// A `<DataItem>`: where a `<Data>` sits inside a frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DataItem {
    pub name: String,
    /// 1-based byte index into the frame (response frames include the positive response SID).
    pub first_byte: u32,
    /// Bit offset; counted from the MSB for big endian, from the LSB for little endian (ddt4all).
    pub bit_offset: u32,
    /// `Ref="1"`.
    #[serde(rename = "ref")]
    pub reference: bool,
    /// `DataItem@Endian`, overriding [`EcuDef::endian`].
    pub endian: Option<Endian>,
}

impl EcuDef {
    /// Data definition by exact name.
    pub fn data(&self, name: &str) -> Option<&DataDef> {
        self.datas.iter().find(|d| d.name == name)
    }

    /// Request by name: exact match first, then case-insensitive (ddt4all `get_request`).
    pub fn request(&self, name: &str) -> Option<&Request> {
        self.requests.iter().find(|r| r.name == name).or_else(|| {
            let lower = name.to_lowercase();
            self.requests
                .iter()
                .find(|r| r.name.to_lowercase() == lower)
        })
    }

    /// All requests whose `SentBytes` equal the given hex string, in document order
    /// (several requests may share the same bytes, e.g. BMT `2180`).
    pub fn request_by_bytes(&self, hex: &str) -> Result<Vec<&Request>, crate::SpecError> {
        let bytes = HexBytes::from_hex(hex)?;
        Ok(self
            .requests
            .iter()
            .filter(|r| r.sent_bytes == bytes)
            .collect())
    }
}
