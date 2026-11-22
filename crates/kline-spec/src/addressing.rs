//! Vehicle addressing file (`<Functions>`, e.g. `x65_addressing.xml`): which diagnostic
//! functions exist at which address and which protocols they support.

use roxmltree::{Document, Node};
use serde::Serialize;

use crate::error::SpecError;
use crate::xml::{attr_flag, attr_parse, child, children, decode_bytes, invalid_value, text};

const XML_NS: &str = "http://www.w3.org/XML/1998/namespace";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Addressing {
    /// `<Tool Address>`: tester address (241 = 0xF1).
    pub tool_address: Option<u8>,
    /// Top-level `<ProtocolList>`: all protocol codes known to the file.
    pub protocols: Vec<AddrProtocol>,
    pub functions: Vec<AddrFunction>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AddrProtocol {
    pub code: u32,
    pub init_mode: Option<u32>,
    pub multipoint: Option<bool>,
    pub name: String,
    /// `<Search/>` present: DDT2000 probes this protocol when scanning.
    pub search: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LocalizedName {
    pub lang: Option<String>,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct AddrFunction {
    pub address: u8,
    pub name: String,
    pub names: Vec<LocalizedName>,
    /// `<XId>` / `<RId>`: CAN request/response identifiers.
    pub xid: Option<u32>,
    pub rid: Option<u32>,
    /// `<ISO8>`: alternative 5-baud init address, when given.
    pub iso8_address: Option<u8>,
    pub protocols: Vec<AddrProtocol>,
}

fn child_number<T: std::str::FromStr>(n: Node, name: &str) -> Result<Option<T>, SpecError> {
    match child(n, name) {
        Some(c) => {
            let t = text(c);
            t.trim()
                .parse()
                .map(Some)
                .map_err(|_| invalid_value(c, "text", &t))
        }
        None => Ok(None),
    }
}

fn protocols(n: Node) -> Result<Vec<AddrProtocol>, SpecError> {
    let Some(list) = child(n, "ProtocolList") else {
        return Ok(Vec::new());
    };
    children(list, "Protocol")
        .map(|p| {
            Ok(AddrProtocol {
                code: attr_parse::<u32>(p, "Code")?
                    .ok_or_else(|| invalid_value(p, "attribute Code", ""))?,
                init_mode: attr_parse::<u32>(p, "InitMode")?,
                multipoint: attr_flag(p, "MultiPoint")?,
                name: p.attribute("Name").unwrap_or_default().to_string(),
                search: child(p, "Search").is_some(),
            })
        })
        .collect()
}

impl Addressing {
    pub fn from_xml_bytes(bytes: &[u8]) -> Result<Addressing, SpecError> {
        let text_ = decode_bytes(bytes)?;
        let doc = Document::parse(&text_)?;
        let root = doc.root_element();
        if root.tag_name().name() != "Functions" {
            return Err(SpecError::WrongDocument(format!(
                "root is <{}>, expected <Functions>",
                root.tag_name().name()
            )));
        }
        let tool_address = match child(root, "Tool") {
            Some(t) => attr_parse::<u8>(t, "Address")?,
            None => None,
        };
        let functions = children(root, "Function")
            .map(|f| {
                Ok(AddrFunction {
                    address: attr_parse::<u8>(f, "Address")?
                        .ok_or_else(|| invalid_value(f, "attribute Address", ""))?,
                    name: f.attribute("Name").unwrap_or_default().to_string(),
                    names: children(f, "Name")
                        .map(|n| LocalizedName {
                            lang: n.attribute((XML_NS, "lang")).map(str::to_string),
                            text: text(n),
                        })
                        .collect(),
                    xid: child_number(f, "XId")?,
                    rid: child_number(f, "RId")?,
                    iso8_address: child_number(f, "ISO8")?,
                    protocols: protocols(f)?,
                })
            })
            .collect::<Result<Vec<_>, SpecError>>()?;
        Ok(Addressing {
            tool_address,
            protocols: protocols(root)?,
            functions,
        })
    }

    /// Function at a diagnostic address.
    pub fn function(&self, address: u8) -> Option<&AddrFunction> {
        self.functions.iter().find(|f| f.address == address)
    }
}
