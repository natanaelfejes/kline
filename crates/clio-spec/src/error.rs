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
    /// A data item does not fit inside the byte buffer it is written to.
    #[error("data item {item:?} (bytes {first_byte}..{end}) does not fit in a {len}-byte frame")]
    OutOfFrame {
        item: String,
        first_byte: u32,
        end: usize,
        len: usize,
    },
}
