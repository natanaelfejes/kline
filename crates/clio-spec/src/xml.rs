//! Byte-level XML helpers: encoding detection and small roxmltree conveniences.

use encoding_rs::{Encoding, UTF_16BE, UTF_16LE, UTF_8};
use roxmltree::Node;

use crate::error::SpecError;

/// Decode raw file bytes to a `String`.
///
/// Order: a BOM wins; otherwise the `encoding="..."` of the XML declaration; otherwise UTF-8
/// (XML 1.0 section 4.3.3 default). Decoding is strict: malformed input is an error, never
/// silently replaced.
pub(crate) fn decode_bytes(bytes: &[u8]) -> Result<String, SpecError> {
    let (encoding, body) = match Encoding::for_bom(bytes) {
        Some((enc, bom_len)) => (enc, &bytes[bom_len..]),
        None => {
            let enc = match declared_encoding(bytes) {
                Some(label) => Encoding::for_label(label.trim().as_bytes())
                    .ok_or_else(|| SpecError::UnknownEncoding(label.to_string()))?,
                None => UTF_8,
            };
            if enc == UTF_16LE || enc == UTF_16BE {
                // A declaration we could read as ASCII cannot be UTF-16 without a BOM.
                return Err(SpecError::Encoding("UTF-16 (declared without BOM)"));
            }
            (enc, bytes)
        }
    };
    encoding
        .decode_without_bom_handling_and_without_replacement(body)
        .map(|cow| cow.into_owned())
        .ok_or(SpecError::Encoding(encoding.name()))
}

/// The `encoding` pseudo-attribute of a leading `<?xml ... ?>` declaration, if any.
fn declared_encoding(bytes: &[u8]) -> Option<&str> {
    if !bytes.starts_with(b"<?xml") {
        return None;
    }
    let end = bytes.windows(2).take(1024).position(|w| w == b"?>")?;
    let decl = std::str::from_utf8(&bytes[..end]).ok()?;
    let rest = &decl[decl.find("encoding")? + "encoding".len()..];
    let rest = rest.trim_start().strip_prefix('=')?.trim_start();
    let quote = rest.chars().next()?;
    if quote != '"' && quote != '\'' {
        return None;
    }
    let rest = &rest[1..];
    Some(&rest[..rest.find(quote)?])
}

/// Direct element children with the given local name.
pub(crate) fn children<'a, 'i>(
    node: Node<'a, 'i>,
    local: &'a str,
) -> impl Iterator<Item = Node<'a, 'i>> + 'a {
    node.children()
        .filter(move |n| n.is_element() && n.tag_name().name() == local)
}

/// First direct element child with the given local name.
pub(crate) fn child<'a, 'i>(node: Node<'a, 'i>, local: &'a str) -> Option<Node<'a, 'i>> {
    children(node, local).next()
}

/// Element descendants (excluding `node` itself) with the given local name, in document order.
/// This mirrors DOM `getElementsByTagName`, which ddt4all uses for most lookups.
pub(crate) fn descendants<'a, 'i>(
    node: Node<'a, 'i>,
    local: &'a str,
) -> impl Iterator<Item = Node<'a, 'i>> + 'a {
    node.descendants()
        .skip(1)
        .filter(move |n| n.is_element() && n.tag_name().name() == local)
}

/// First element descendant with the given local name.
pub(crate) fn descendant<'a, 'i>(node: Node<'a, 'i>, local: &'a str) -> Option<Node<'a, 'i>> {
    descendants(node, local).next()
}

/// Concatenated text (including CDATA) of all descendants.
pub(crate) fn text(node: Node) -> String {
    node.descendants()
        .filter(|n| n.is_text())
        .filter_map(|n| n.text())
        .collect()
}

/// A non-empty attribute value. Empty attributes are treated as absent, like ddt4all's
/// `if attr:` checks.
pub(crate) fn attr<'a>(node: Node<'a, '_>, name: &str) -> Option<&'a str> {
    node.attribute(name).filter(|v| !v.is_empty())
}

pub(crate) fn invalid_value(node: Node, what: &str, value: &str) -> SpecError {
    SpecError::InvalidValue {
        element: node.tag_name().name().to_string(),
        what: what.to_string(),
        value: value.to_string(),
    }
}

/// Parse an integer attribute (decimal, surrounding whitespace allowed like Python `int()`).
pub(crate) fn attr_parse<T: std::str::FromStr>(
    node: Node,
    name: &str,
) -> Result<Option<T>, SpecError> {
    match attr(node, name) {
        None => Ok(None),
        Some(v) => v
            .trim()
            .parse::<T>()
            .map(Some)
            .map_err(|_| invalid_value(node, &format!("attribute {name}"), v)),
    }
}

/// Parse a float attribute (surrounding whitespace allowed like Python `float()`, e.g. `" .375"`).
pub(crate) fn attr_f64(node: Node, name: &str) -> Result<Option<f64>, SpecError> {
    attr_parse::<f64>(node, name)
}

/// Parse a `0`/`1` flag attribute.
pub(crate) fn attr_flag(node: Node, name: &str) -> Result<Option<bool>, SpecError> {
    match attr(node, name).map(str::trim) {
        None => Ok(None),
        Some("1") => Ok(Some(true)),
        Some("0") => Ok(Some(false)),
        Some(v) => Err(invalid_value(node, &format!("attribute {name}"), v)),
    }
}
