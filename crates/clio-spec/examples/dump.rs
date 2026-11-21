//! Print a parsed definition as JSON, or decode responses of one request.
//!
//! ```text
//! cargo run -p clio-spec --example dump -- ddt-xml/Safir_2.xml
//! cargo run -p clio-spec --example dump -- ddt-xml/Safir_2.xml --request "P2 - Ctrl entrees" --response 7351...
//! cargo run -p clio-spec --example dump -- ddt-xml/x65_addressing.xml
//! ```
//! `--response` may be repeated; each decode is printed as one JSON line.

use std::error::Error;

use clio_spec::{Addressing, EcuDef, HexBytes, SpecError};

fn usage() -> ! {
    eprintln!("usage: dump <file.xml> [--request NAME [--response HEX]...]");
    std::process::exit(2);
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let mut path = None;
    let mut request = None;
    let mut responses = Vec::new();
    while let Some(a) = args.next() {
        match a.as_str() {
            "--request" => request = Some(args.next().unwrap_or_else(|| usage())),
            "--response" => responses.push(args.next().unwrap_or_else(|| usage())),
            _ if path.is_none() && !a.starts_with("--") => path = Some(a),
            _ => usage(),
        }
    }
    let path = path.unwrap_or_else(|| usage());
    let bytes = std::fs::read(&path)?;

    let ecu = match EcuDef::from_xml_bytes(&bytes) {
        Ok(ecu) => ecu,
        Err(SpecError::WrongDocument(_)) if request.is_none() => {
            let addressing = Addressing::from_xml_bytes(&bytes)?;
            println!("{}", serde_json::to_string_pretty(&addressing)?);
            return Ok(());
        }
        Err(e) => return Err(e.into()),
    };

    let Some(name) = request else {
        println!("{}", serde_json::to_string_pretty(&ecu)?);
        return Ok(());
    };
    let req = ecu
        .request(&name)
        .ok_or_else(|| format!("no request named {name:?}"))?;
    if responses.is_empty() {
        println!("{}", serde_json::to_string_pretty(req)?);
        return Ok(());
    }
    for hex in responses {
        let frame = HexBytes::from_hex(&hex)?;
        let items = req.decode(&ecu, &frame.0)?;
        let out = serde_json::json!({
            "request": req.name,
            "response": frame,
            "items": items,
        });
        println!("{}", serde_json::to_string(&out)?);
    }
    Ok(())
}
