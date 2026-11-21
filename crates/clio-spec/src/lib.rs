//! DDT2000 ECU definition XML -> typed model, plus data decode/encode.
//!
//! Pure library: no I/O beyond the bytes passed in. Semantics follow the ddt4all reference
//! decoder; every deviation and ambiguity is listed in `docs/spec-semantics.md`.
//!
//! ```no_run
//! # fn main() -> Result<(), clio_spec::SpecError> {
//! let xml: &[u8] = b"...";
//! let ecu = clio_spec::EcuDef::from_xml_bytes(xml)?;
//! let req = ecu.request("P2 - Ctrl entrees").expect("request");
//! let response: &[u8] = &[0x73, 0x51 /* ... */];
//! for item in req.decode_checked(&ecu, response)? {
//!     println!("{} = {:?} {:?}", item.name, item.display, item.unit);
//! }
//! # Ok(()) }
//! ```

mod addressing;
mod codec;
mod error;
mod model;
mod parse;
mod report;
mod xml;

pub use addressing::{AddrFunction, AddrProtocol, Addressing, LocalizedName};
pub use codec::{DecodedItem, Raw, Value};
pub use error::SpecError;
pub use model::*;
pub use report::{ParseReport, ParseWarning};
