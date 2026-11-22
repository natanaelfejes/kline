//! Parse warnings: elements and attributes the parser does not read.
//!
//! The parser looks elements up by name and ignores everything else, so a misspelled attribute
//! (`Stepp="2"`) would silently fall back to a default. [`EcuDef::from_xml_bytes_with_report`]
//! walks the document against the tables below (what `parse.rs` actually reads) and reports the
//! rest. An unknown element is reported once; below it only what the parser still finds at
//! depth (the `RECURSIVE` names) is checked. `parse.rs` searches `Request` and `Data` contents
//! recursively, so a nested element it reads is never reported as unread.

use roxmltree::Node;
use serde::Serialize;

use crate::error::SpecError;
use crate::model::{EcuDef, Semantics};

/// Something in the file that the parser did not read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParseWarning {
    /// An element the parser does not read at this place (its subtree is skipped).
    UnknownElement { path: String },
    /// An attribute the parser does not read on this element.
    UnknownAttribute { path: String, name: String },
    /// [`Semantics::Ddt2000`]: an attribute is absent (or not a VB boolean) and the DDT2000
    /// default was used (`DataItem@FirstByte` 1, `Received@MinBytes` 1, `Bits@signed` /
    /// `Bytes@ascii` false).
    Defaulted {
        path: String,
        name: String,
        value: String,
    },
}

/// Warnings collected while parsing, in document order.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize)]
pub struct ParseReport {
    pub warnings: Vec<ParseWarning>,
}

/// Children of these elements are free-form names (vehicle codes, access levels).
const ANY_CHILDREN: &[&str] = &["Projects", "DenyAccess"];

/// (element, attributes read, child elements read). Text content is not checked.
const SCHEMA: &[(&str, &[&str], &[&str])] = &[
    ("xml", &[], &["Target"]),
    (
        "Target",
        &["Name"],
        &[
            "Function",
            "Description",
            "Projects",
            "PowerSupply",
            "K",
            "CAN",
            "AutoIdents",
            "Datas",
            "Requests",
            "Failures",
            "Devices",
        ],
    ),
    ("Failures", &["Name", "Id"], &[]),
    ("Devices", &[], &["Device"]),
    ("Device", &["Name", "Type"], &["DeviceData"]),
    ("DeviceData", &["Name", "FailureFlag"], &[]),
    ("Function", &["Name", "Address"], &[]),
    ("Projects", &[], &[]),
    ("DenyAccess", &[], &[]),
    ("Description", &[], &[]),
    ("PowerSupply", &[], &["Ground", "Vbat", "AfterContact"]),
    ("Ground", &[], &[]),
    ("Vbat", &[], &[]),
    ("AfterContact", &[], &[]),
    (
        "K",
        &["BaudRate"],
        &[
            "ISO8",
            "KWP",
            "InterByte",
            "ReplyToRequest",
            "RequestToReply",
            "TesterPresent",
            "OnError",
        ],
    ),
    ("ISO8", &[], &["L", "KW1", "KW2", "FirstRequest"]),
    ("L", &[], &[]),
    ("KW1", &["Value"], &[]),
    ("KW2", &["Value", "Timeout"], &[]),
    ("FirstRequest", &["Delay"], &[]),
    ("KWP", &["Multipoint"], &["ISO8", "FastInit", "KW2_"]),
    ("FastInit", &[], &["KW1", "KW2"]),
    ("KW2_", &["Delay"], &[]),
    ("InterByte", &["Delay"], &[]),
    ("ReplyToRequest", &["Delay"], &[]),
    ("RequestToReply", &["Timeout"], &[]),
    ("TesterPresent", &["Delay"], &[]),
    ("OnError", &["Delay"], &[]),
    ("CAN", &["BaudRate"], &["SendId", "ReceiveId"]),
    ("SendId", &[], &["CANId"]),
    ("ReceiveId", &[], &["CANId"]),
    ("CANId", &["Value"], &[]),
    ("AutoIdents", &[], &["AutoIdent"]),
    (
        "AutoIdent",
        &["DiagVersion", "Supplier", "Soft", "Version"],
        &[],
    ),
    ("Datas", &[], &["Data"]),
    (
        "Data",
        &["Name"],
        &["Description", "Comment", "Mnemonic", "Bytes", "Bits"],
    ),
    ("Comment", &[], &[]),
    ("Mnemonic", &[], &[]),
    ("Bytes", &["count", "ascii"], &[]),
    ("Bits", &["count", "signed"], &["Scaled", "Binary", "List"]),
    ("Binary", &[], &[]),
    (
        "Scaled",
        &["Step", "Offset", "DivideBy", "Format", "Unit"],
        &[],
    ),
    ("List", &[], &["Item"]),
    ("Item", &["Value", "Text"], &[]),
    ("Requests", &["Endian"], &["Request"]),
    (
        "Request",
        &["Name"],
        &[
            "ManuelSend",
            "DenyAccess",
            "DossierMaintenabilite",
            "Sent",
            "Received",
            "ShiftBytesCount",
        ],
    ),
    ("ManuelSend", &[], &[]),
    ("DossierMaintenabilite", &[], &[]),
    ("ShiftBytesCount", &[], &[]),
    ("Sent", &[], &["SentBytes", "VariableLength", "DataItem"]),
    ("SentBytes", &[], &[]),
    ("VariableLength", &[], &[]),
    (
        "Received",
        &["MinBytes"],
        &["ReplyBytes", "DataItem", "ShiftBytesCount"],
    ),
    ("ReplyBytes", &[], &[]),
    (
        "DataItem",
        &["Name", "Ref", "FirstByte", "BitOffset", "Endian"],
        &[],
    ),
];

fn schema(element: &str) -> Option<(&'static [&'static str], &'static [&'static str])> {
    SCHEMA
        .iter()
        .find(|(e, _, _)| *e == element)
        .map(|(_, a, c)| (*a, *c))
}

fn segment(n: Node) -> String {
    let name = n.tag_name().name();
    match n.attribute("Name") {
        Some(v) => format!("{name}[@Name={v:?}]"),
        None => name.to_string(),
    }
}

/// Names the parser looks up with `descendants`/`descendant` (any depth) below an element, in
/// addition to the direct children listed in [`SCHEMA`]. The scope is inherited by everything
/// below the element, as in `parse.rs`.
/// Searched in the whole document.
const DOCUMENT_SCOPE: &[&str] = &["Requests", "Data"];

const RECURSIVE: &[(&str, &[&str])] = &[
    ("Requests", &["Request"]),
    (
        "Request",
        &[
            "Sent",
            "Received",
            "ManuelSend",
            "DenyAccess",
            "DossierMaintenabilite",
            "ReplyBytes",
            "ShiftBytesCount",
        ],
    ),
    ("Sent", &["SentBytes", "VariableLength", "DataItem"]),
    ("Received", &["DataItem"]),
    (
        "Data",
        &[
            "Description",
            "Comment",
            "Mnemonic",
            "Bytes",
            "Bits",
            "List",
        ],
    ),
    ("Bits", &["Binary", "Scaled"]),
    ("List", &["Item"]),
];

fn scope_of(element: &str, inherited: &[&'static str]) -> Vec<&'static str> {
    let mut scope = inherited.to_vec();
    if let Some((_, names)) = RECURSIVE.iter().find(|(e, _)| *e == element) {
        scope.extend_from_slice(names);
    }
    scope
}

fn walk(n: Node, path: &str, inherited: &[&'static str], out: &mut Vec<ParseWarning>) {
    let name = n.tag_name().name();
    let Some((attrs, kids)) = schema(name) else {
        out.push(ParseWarning::UnknownElement {
            path: path.to_string(),
        });
        return;
    };
    for a in n.attributes() {
        // Namespaced attributes (xml:lang, xsi:...) are not DDT2000 data.
        if a.namespace().is_none() && !attrs.contains(&a.name()) {
            out.push(ParseWarning::UnknownAttribute {
                path: path.to_string(),
                name: a.name().to_string(),
            });
        }
    }
    if ANY_CHILDREN.contains(&name) {
        return;
    }
    let scope = scope_of(name, inherited);
    walk_children(n, path, kids, &scope, true, out);
}

/// Visit the element children of `n`. A child the parser reads (a direct child in `kids`, or a
/// name in `scope`) is walked. Any other child is reported once if `report_unknown`, and its
/// subtree is searched only for names in `scope`, which the parser still finds at depth.
fn walk_children(
    n: Node,
    path: &str,
    kids: &[&str],
    scope: &[&'static str],
    report_unknown: bool,
    out: &mut Vec<ParseWarning>,
) {
    for c in n.children().filter(|c| c.is_element()) {
        let child_path = format!("{path}/{}", segment(c));
        let cname = c.tag_name().name();
        if kids.contains(&cname) || scope.contains(&cname) {
            walk(c, &child_path, scope, out);
        } else {
            if report_unknown {
                out.push(ParseWarning::UnknownElement {
                    path: child_path.clone(),
                });
            }
            walk_children(c, &child_path, &[], scope, false, out);
        }
    }
}

fn path_of(n: Node) -> String {
    let mut parts: Vec<String> = n
        .ancestors()
        .filter(|a| a.is_element())
        .map(segment)
        .collect();
    parts.reverse();
    parts.join("/")
}

/// Defaults DDT2000 applies silently (`On Error Resume Next`), reported so a broken file is
/// still noticed.
fn ddt2000_defaults(root: Node, out: &mut Vec<ParseWarning>) {
    let mut warn = |n: Node, name: &str, value: &str| {
        out.push(ParseWarning::Defaulted {
            path: path_of(n),
            name: name.to_string(),
            value: value.to_string(),
        })
    };
    for n in root.descendants().filter(|n| n.is_element()) {
        match n.tag_name().name() {
            "DataItem" if crate::xml::attr(n, "FirstByte").is_none() => warn(n, "FirstByte", "1"),
            "Received" if crate::xml::attr(n, "MinBytes").is_none() => warn(n, "MinBytes", "1"),
            "Bits" | "Bytes" => {
                let a = if n.tag_name().name() == "Bits" {
                    "signed"
                } else {
                    "ascii"
                };
                if let Some(v) = n.attribute(a) {
                    if crate::xml::vb_cbool(Some(v)).is_none() {
                        warn(n, a, "false");
                    }
                }
            }
            _ => {}
        }
    }
}

pub(crate) fn report(doc: &roxmltree::Document, sem: Semantics) -> ParseReport {
    let root = doc.root_element();
    let mut warnings = Vec::new();
    // The parser searches the whole document for these (`descendants(root, ..)`).
    walk(root, &segment(root), DOCUMENT_SCOPE, &mut warnings);
    if sem == Semantics::Ddt2000 {
        ddt2000_defaults(root, &mut warnings);
    }
    ParseReport { warnings }
}

impl EcuDef {
    /// Like [`from_xml_bytes`](Self::from_xml_bytes), and also report every element and
    /// attribute the parser did not read (misspellings, unsupported DDT2000 features,
    /// the screen layout sections).
    /// Uses [`Semantics::Ddt2000`].
    pub fn from_xml_bytes_with_report(bytes: &[u8]) -> Result<(EcuDef, ParseReport), SpecError> {
        Self::from_xml_bytes_with_report_as(bytes, Semantics::Ddt2000)
    }

    /// [`from_xml_bytes_with_report`](Self::from_xml_bytes_with_report) with the given
    /// interpretation; DDT2000 defaults are reported as [`ParseWarning::Defaulted`].
    pub fn from_xml_bytes_with_report_as(
        bytes: &[u8],
        semantics: Semantics,
    ) -> Result<(EcuDef, ParseReport), SpecError> {
        crate::parse::parse_bytes(bytes, semantics, |d| report(d, semantics))
    }
}
