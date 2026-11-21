//! `<Target>` XML -> [`EcuDef`]. Lookup rules mirror ddt4all `ecu_file.py`, `ecu_data.py`,
//! `ecu_request.py` and `data_item.py`; differences are listed in docs/spec-semantics.md.

use roxmltree::{Document, Node};

use crate::error::SpecError;
use crate::model::*;
use crate::xml::{
    attr, attr_f64, attr_flag, attr_parse, child, children, decode_bytes, descendant, descendants,
    invalid_value, text,
};

impl EcuDef {
    /// Parse a DDT2000 ECU definition from the raw file bytes (any encoding the XML
    /// declaration names).
    /// Elements and attributes the parser does not read are ignored; use
    /// [`from_xml_bytes_with_report`](Self::from_xml_bytes_with_report) to list them.
    pub fn from_xml_bytes(bytes: &[u8]) -> Result<EcuDef, SpecError> {
        parse_bytes(bytes, |_| ()).map(|(ecu, ())| ecu)
    }
}

/// Decode, parse the XML, build the model and run `extra` on the same document.
pub(crate) fn parse_bytes<T>(
    bytes: &[u8],
    extra: impl FnOnce(&Document) -> T,
) -> Result<(EcuDef, T), SpecError> {
    let text = decode_bytes(bytes)?;
    let opts = roxmltree::ParsingOptions {
        allow_dtd: false,
        ..Default::default()
    };
    let doc = Document::parse_with_options(&text, opts)?;
    let ecu = parse_doc(&doc)?;
    Ok((ecu, extra(&doc)))
}

fn find_target<'a, 'i>(doc: &'a Document<'i>) -> Result<Node<'a, 'i>, SpecError> {
    let root = doc.root_element();
    let target = if root.tag_name().name() == "Target" {
        Some(root)
    } else {
        child(root, "Target")
    };
    let target = target.ok_or_else(|| SpecError::WrongDocument("no <Target> element".into()))?;
    if target.tag_name().namespace() != Some(ECU_NAMESPACE) {
        return Err(SpecError::WrongDocument(format!(
            "<Target> namespace is {:?}, expected {ECU_NAMESPACE}",
            target.tag_name().namespace()
        )));
    }
    Ok(target)
}

fn parse_doc(doc: &Document) -> Result<EcuDef, SpecError> {
    let target = find_target(doc)?;
    let root = doc.root_element();

    let function = match child(target, "Function") {
        Some(f) => Some(Function {
            name: f.attribute("Name").unwrap_or_default().to_string(),
            address: attr_parse::<u8>(f, "Address")?
                .ok_or_else(|| invalid_value(f, "attribute Address", ""))?,
        }),
        None => None,
    };

    let projects = child(target, "Projects")
        .map(|p| {
            p.children()
                .filter(|n| n.is_element())
                .map(|n| n.tag_name().name().to_string())
                .collect()
        })
        .unwrap_or_default();

    let power_supply = child(target, "PowerSupply").map(|p| PowerSupply {
        ground: child(p, "Ground").is_some(),
        vbat: child(p, "Vbat").is_some(),
        after_contact: child(p, "AfterContact").is_some(),
    });

    let k = child(target, "K").map(parse_k).transpose()?;
    let can = child(target, "CAN").map(parse_can).transpose()?;

    let auto_idents = match child(target, "AutoIdents") {
        Some(a) => children(a, "AutoIdent")
            .map(|ai| AutoIdent {
                diag_version: ai.attribute("DiagVersion").unwrap_or_default().to_string(),
                supplier: ai.attribute("Supplier").unwrap_or_default().to_string(),
                soft: ai.attribute("Soft").unwrap_or_default().to_string(),
                version: ai.attribute("Version").unwrap_or_default().to_string(),
            })
            .collect(),
        None => Vec::new(),
    };

    // ddt4all: every <Requests> tag may set the global endianness; the last one wins.
    let mut endian = Endian::Big;
    let mut requests: Vec<Request> = Vec::new();
    for reqs in descendants(root, "Requests") {
        if let Some(e) = attr(reqs, "Endian") {
            endian = parse_endian(reqs, e)?;
        }
        for r in descendants(reqs, "Request") {
            upsert(&mut requests, parse_request(r)?, |x| &x.name);
        }
    }

    let mut datas: Vec<DataDef> = Vec::new();
    for d in descendants(root, "Data") {
        upsert(&mut datas, parse_data(d)?, |x| &x.name);
    }

    Ok(EcuDef {
        name: target.attribute("Name").unwrap_or_default().to_string(),
        function,
        description: child(target, "Description").map(text),
        projects,
        power_supply,
        k,
        can,
        auto_idents,
        endian,
        datas,
        requests,
    })
}

/// Insert, or replace an existing entry with the same key in place (Python dict semantics).
fn upsert<T>(list: &mut Vec<T>, item: T, key: impl Fn(&T) -> &String) {
    match list.iter().position(|x| key(x) == key(&item)) {
        Some(i) => list[i] = item,
        None => list.push(item),
    }
}

fn parse_endian(node: Node, value: &str) -> Result<Endian, SpecError> {
    match value {
        "Big" => Ok(Endian::Big),
        "Little" => Ok(Endian::Little),
        other => Err(invalid_value(node, "attribute Endian", other)),
    }
}

fn delay(parent: Node, element: &str, attribute: &str) -> Result<Option<u32>, SpecError> {
    match child(parent, element) {
        Some(n) => attr_parse::<u32>(n, attribute),
        None => Ok(None),
    }
}

fn key_word(parent: Node, element: &str) -> Result<Option<u8>, SpecError> {
    match child(parent, element) {
        Some(n) => attr_parse::<u8>(n, "Value"),
        None => Ok(None),
    }
}

fn parse_iso8(n: Node) -> Result<Iso8, SpecError> {
    Ok(Iso8 {
        l_line: child(n, "L").is_some(),
        kw1: key_word(n, "KW1")?,
        kw2: key_word(n, "KW2")?,
        kw2_timeout: delay(n, "KW2", "Timeout")?,
        first_request_delay: delay(n, "FirstRequest", "Delay")?,
    })
}

fn parse_k(k: Node) -> Result<KLine, SpecError> {
    let kwp = match child(k, "KWP") {
        Some(kwp) => Some(Kwp {
            multipoint: attr_flag(kwp, "Multipoint")?,
            iso8: child(kwp, "ISO8").map(parse_iso8).transpose()?,
            fast_init: match child(kwp, "FastInit") {
                Some(f) => Some(FastInit {
                    kw1: key_word(f, "KW1")?,
                    kw2: key_word(f, "KW2")?,
                }),
                None => None,
            },
            kw2_delay: delay(kwp, "KW2_", "Delay")?,
        }),
        None => None,
    };
    Ok(KLine {
        baud_rate: attr_parse::<u32>(k, "BaudRate")?,
        iso8: child(k, "ISO8").map(parse_iso8).transpose()?,
        kwp,
        inter_byte_delay: delay(k, "InterByte", "Delay")?,
        reply_to_request_delay: delay(k, "ReplyToRequest", "Delay")?,
        request_to_reply_timeout: delay(k, "RequestToReply", "Timeout")?,
        tester_present_delay: delay(k, "TesterPresent", "Delay")?,
        on_error_delay: delay(k, "OnError", "Delay")?,
    })
}

fn parse_can(can: Node) -> Result<Can, SpecError> {
    let id = |name: &str| -> Result<Option<u32>, SpecError> {
        match child(can, name).and_then(|n| child(n, "CANId")) {
            Some(c) => attr_parse::<u32>(c, "Value"),
            None => Ok(None),
        }
    };
    Ok(Can {
        baud_rate: attr_parse::<u32>(can, "BaudRate")?,
        send_id: id("SendId")?,
        receive_id: id("ReceiveId")?,
    })
}

fn parse_data_item(n: Node) -> Result<DataItem, SpecError> {
    Ok(DataItem {
        name: n.attribute("Name").unwrap_or_default().to_string(),
        // Present on all 2165 DataItems of our six files; a missing one is a broken file.
        first_byte: match attr_parse::<u32>(n, "FirstByte")? {
            // Positions are 1-based; none of our files has 0 (ddt4all would never decode it).
            Some(0) => return Err(invalid_value(n, "attribute FirstByte", "0")),
            Some(b) => b,
            None => {
                return Err(SpecError::MissingFirstByte {
                    item: n.attribute("Name").unwrap_or_default().to_string(),
                })
            }
        },
        // Absent on 1065 of 2165 DataItems: 0, as in ddt4all.
        bit_offset: attr_parse::<u32>(n, "BitOffset")?.unwrap_or(0),
        reference: n.attribute("Ref") == Some("1"),
        endian: attr(n, "Endian").map(|e| parse_endian(n, e)).transpose()?,
    })
}

fn parse_items(section: Option<Node>) -> Result<Vec<DataItem>, SpecError> {
    let mut items = Vec::new();
    if let Some(s) = section {
        for di in descendants(s, "DataItem") {
            upsert(&mut items, parse_data_item(di)?, |x| &x.name);
        }
    }
    Ok(items)
}

fn hex_text(n: Node) -> Result<HexBytes, SpecError> {
    HexBytes::from_hex(&text(n))
}

fn parse_request(r: Node) -> Result<Request, SpecError> {
    let sent = descendant(r, "Sent");
    let received = descendant(r, "Received");
    let sent_bytes = match sent.and_then(|s| descendant(s, "SentBytes")) {
        Some(sb) => hex_text(sb)?,
        None => HexBytes::default(),
    };
    Ok(Request {
        name: r.attribute("Name").unwrap_or_default().to_string(),
        sent_bytes,
        variable_length: sent.and_then(|s| descendant(s, "VariableLength")).is_some(),
        manual_send: descendant(r, "ManuelSend").is_some(),
        deny_access: descendant(r, "DenyAccess")
            .map(|d| {
                d.children()
                    .filter(|n| n.is_element())
                    .map(|n| n.tag_name().name().to_string())
                    .collect()
            })
            .unwrap_or_default(),
        dossier_maintenabilite: descendant(r, "DossierMaintenabilite").is_some(),
        min_bytes: match received {
            Some(rc) => attr_parse::<u32>(rc, "MinBytes")?.unwrap_or(0),
            None => 0,
        },
        reply_bytes: descendant(r, "ReplyBytes").map(hex_text).transpose()?,
        shift_bytes_count: match descendant(r, "ShiftBytesCount") {
            Some(n) => Some(
                text(n)
                    .trim()
                    .parse()
                    .map_err(|_| invalid_value(n, "text", &text(n)))?,
            ),
            None => None,
        },
        sent_items: parse_items(sent)?,
        received_items: parse_items(received)?,
    })
}

fn parse_data(d: Node) -> Result<DataDef, SpecError> {
    let mut def = DataDef {
        name: d.attribute("Name").unwrap_or_default().to_string(),
        description: descendant(d, "Description").map(text),
        comment: descendant(d, "Comment").map(text),
        mnemonic: descendant(d, "Mnemonic").map(text),
        bits_count: 8,
        bytes_count: 1,
        is_bytes: false,
        ascii: false,
        signed: false,
        binary: false,
        scaled: None,
        list: Vec::new(),
    };

    for list in descendants(d, "List") {
        for item in descendants(list, "Item") {
            let value = attr_parse::<i64>(item, "Value")?
                .ok_or_else(|| invalid_value(item, "attribute Value", ""))?;
            def.list.push(ListItem {
                value,
                text: item.attribute("Text").unwrap_or_default().to_string(),
            });
        }
    }

    // ddt4all processes <Bytes> first, then <Bits> (which may override the counts).
    if let Some(bytes) = descendant(d, "Bytes") {
        def.is_bytes = true;
        if let Some(count) = attr(bytes, "count") {
            let count = count.replace(',', ".");
            if count.contains('.') {
                let f: f64 = count
                    .trim()
                    .parse()
                    .map_err(|_| invalid_value(bytes, "attribute count", &count))?;
                if !(f.is_finite() && f >= 0.0) {
                    return Err(invalid_value(bytes, "attribute count", &count));
                }
                def.bytes_count = f.ceil() as u32;
                def.bits_count = (f * 8.0) as u32;
            } else {
                let n: u32 = count
                    .trim()
                    .parse()
                    .map_err(|_| invalid_value(bytes, "attribute count", &count))?;
                def.bytes_count = n;
                def.bits_count = n
                    .checked_mul(8)
                    .ok_or_else(|| invalid_value(bytes, "attribute count", &count))?;
            }
        }
        def.ascii = bytes.attribute("ascii") == Some("1");
    }

    if let Some(bits) = descendant(d, "Bits") {
        if let Some(n) = attr_parse::<u32>(bits, "count")? {
            def.bits_count = n;
            def.bytes_count = n.div_ceil(8);
        }
        def.signed = attr(bits, "signed").is_some();
        def.binary = descendant(bits, "Binary").is_some();
        if let Some(sc) = descendant(bits, "Scaled") {
            // Defaults as in ddt4all. Absent in our six files on 829 (Step), 906 (Offset) and
            // 919 (DivideBy) of 945 <Scaled> elements, so they are the normal case.
            def.scaled = Some(Scaled {
                step: attr_f64(sc, "Step")?.unwrap_or(1.0),
                offset: attr_f64(sc, "Offset")?.unwrap_or(0.0),
                divide_by: attr_f64(sc, "DivideBy")?.unwrap_or(1.0),
                format: attr(sc, "Format").map(str::to_string),
                unit: attr(sc, "Unit").map(str::to_string),
            });
        }
    }

    Ok(def)
}
