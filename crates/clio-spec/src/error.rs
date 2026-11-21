use thiserror::Error;

/// Everything that can go wrong while parsing a definition or coding a value.
#[derive(Debug, Error)]
pub enum SpecError {
    /// The XML declaration names an encoding that `encoding_rs` does not know.
    #[error("unknown XML encoding label {0:?}")]
    UnknownEncoding(String),
    /// The bytes are not valid in the declared (or BOM-detected) encoding.
    #[error("input is not valid {0}")]
    Encoding(&'static str),
    /// The text is not well-formed XML.
    #[error("XML parse error: {0}")]
    Xml(#[from] roxmltree::Error),
    /// Well-formed XML, but not the expected document type.
    #[error("not a DDT2000 definition: {0}")]
    WrongDocument(String),
    /// An attribute or element value could not be interpreted.
    #[error("invalid value {value:?} for {what} in <{element}>")]
    InvalidValue {
        element: String,
        what: String,
        value: String,
    },
    /// A hex string (SentBytes, ReplyBytes, user input) is malformed.
    #[error("invalid hex string {0:?}")]
    InvalidHex(String),
    /// A DataItem names a Data that does not exist in the definition.
    #[error("unknown data {0:?}")]
    UnknownData(String),
    /// A request input names a data item that the request does not send.
    #[error("request {request:?} has no sent data item {item:?}")]
    UnknownSentItem { request: String, item: String },
    /// A value cannot be encoded for this data definition.
    #[error("cannot encode value for {data:?}: {reason}")]
    Encode { data: String, reason: String },
    /// A number is not on the `Step`/`DivideBy` grid of a scaled data
    /// ([`DataDef::encode_exact`](crate::DataDef::encode_exact)).
    #[error(
        "cannot encode {value} exactly for {data:?}: not on the Step grid (nearest {nearest})"
    )]
    OffGrid {
        data: String,
        value: f64,
        /// The nearest value that can be encoded (what `encode` would round to).
        nearest: f64,
    },
    /// A `<DataItem>` without a `FirstByte` attribute.
    #[error("<DataItem Name={item:?}> has no FirstByte")]
    MissingFirstByte { item: String },
    /// The ECU answered with a negative response (`7F <sid> <nrc>`).
    #[error("negative response to service {sid:#04X}: NRC {nrc:#04X}")]
    NegativeResponse { sid: u8, nrc: u8 },
    /// The ECU answered `7F <got> <nrc>` for a different service than the one sent.
    #[error(
        "negative response for service {got:#04X} (NRC {nrc:#04X}), but service {expected:#04X} was sent"
    )]
    NegativeResponseForOtherSid { expected: u8, got: u8, nrc: u8 },
    /// The first response byte is not the positive response SID (request SID + 0x40).
    #[error("unexpected response SID {got:#04X}, expected {expected:#04X}")]
    UnexpectedSid { expected: u8, got: u8 },
    /// The request has no SID with a positive response (empty `SentBytes`, or SID >= 0xC0).
    #[error("request {request:?} has no positive response SID")]
    NoPositiveSid { request: String },
    /// A response byte that echoes the request (before the first received data item) differs.
    #[error("response byte {position} is {got:#04X}, request echo expects {expected:#04X}")]
    ReplyMismatch {
        /// 1-based byte position in the response.
        position: usize,
        expected: u8,
        got: u8,
    },
    /// The response is shorter than `Received@MinBytes` (or than a negative response / the echo).
    #[error("response has {len} bytes, at least {min} expected")]
    TooShort { min: usize, len: usize },
    /// A data item does not fit inside the byte buffer it is written to.
    #[error("data item {item:?} (bytes {first_byte}..{end}) does not fit in a {len}-byte frame")]
    OutOfFrame {
        item: String,
        first_byte: u32,
        end: usize,
        len: usize,
    },
}
