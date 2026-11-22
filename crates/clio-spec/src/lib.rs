//! DDT2000 ECU definition XML -> typed model, plus data decode/encode.
//!
//! Pure library: no I/O beyond the bytes passed in. Two interpretations ([`Semantics`]):
//! DDT2000 itself (default, the author's DDT2000 behaviour notes) and the ddt4all reference decoder
//! (docs/spec-semantics.md, used by the ddt4all crosscheck).
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
mod autoident;
mod codec;
mod ddt2000;
mod error;
mod failures;
mod model;
mod parse;
mod report;
mod xml;

pub use addressing::{AddrFunction, AddrProtocol, Addressing, LocalizedName};
pub use autoident::{best_match, normalize_space, Ident, MatchLevel};
pub use codec::{DecodedItem, Raw, Value};
pub use error::SpecError;
pub use failures::{
    ActiveFault, ClearChoice, ClearPlan, ClearSource, DDT2000_CLEAR_FALLBACK, DDT2000_CLEAR_NAMES,
    DDT2000_CLEAR_PREFIX,
};
pub use model::*;
pub use report::{ParseReport, ParseWarning};
