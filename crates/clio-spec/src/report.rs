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
use crate::model::EcuDef;

/// Something in the file that the parser did not read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ParseWarning {
    /// An element the parser does not read at this place (its subtree is skipped).
    UnknownElement { path: String },
    /// An attribute the parser does not read on this element.
    UnknownAttribute { path: String, name: String },
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
        ],
    ),
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

pub(crate) fn report(doc: &roxmltree::Document) -> ParseReport {
    let root = doc.root_element();
    let mut warnings = Vec::new();
    // The parser searches the whole document for these (`descendants(root, ..)`).
    walk(root, &segment(root), DOCUMENT_SCOPE, &mut warnings);
    ParseReport { warnings }
}

impl EcuDef {
    /// Like [`from_xml_bytes`](Self::from_xml_bytes), and also report every element and
    /// attribute the parser did not read (misspellings, unsupported DDT2000 features,
    /// the screen layout sections).
    pub fn from_xml_bytes_with_report(bytes: &[u8]) -> Result<(EcuDef, ParseReport), SpecError> {
        crate::parse::parse_bytes(bytes, report)
    }
}
