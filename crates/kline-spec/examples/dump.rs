//! Print a parsed definition as JSON, or decode responses of one request.
//!
//! ```text
//! cargo run -p kline-spec --example dump -- ddt-xml/Safir_2.xml
//! cargo run -p kline-spec --example dump -- ddt-xml/Safir_2.xml --request "P2 - Ctrl entrees" --response 7351...
//! cargo run -p kline-spec --example dump -- ddt-xml/x65_addressing.xml
//! ```
//! `--response` may be repeated; each decode is printed as one JSON line.
//! `--semantics ddt4all` decodes like ddt4all (used by the author's ddt4all crosscheck script); the default
//! is DDT2000.

use std::error::Error;

use kline_spec::{Addressing, EcuDef, HexBytes, Semantics, SpecError};

fn usage() -> ! {
    eprintln!(
        "usage: dump <file.xml> [--semantics ddt2000|ddt4all] [--request NAME [--response HEX]...]"
    );
    std::process::exit(2);
}

fn main() -> Result<(), Box<dyn Error>> {
    let mut args = std::env::args().skip(1);
    let mut path = None;
    let mut request = None;
    let mut responses = Vec::new();
    let mut semantics = Semantics::Ddt2000;
    while let Some(a) = args.next() {
        match a.as_str() {
            "--request" => request = Some(args.next().unwrap_or_else(|| usage())),
            "--response" => responses.push(args.next().unwrap_or_else(|| usage())),
            "--semantics" => {
                semantics = match args.next().as_deref() {
                    Some("ddt2000") => Semantics::Ddt2000,
                    Some("ddt4all") => Semantics::Ddt4all,
                    _ => usage(),
                }
            }
            _ if path.is_none() && !a.starts_with("--") => path = Some(a),
            _ => usage(),
        }
    }
    let path = path.unwrap_or_else(|| usage());
    let bytes = std::fs::read(&path)?;

    let ecu = match EcuDef::from_xml_bytes_as(&bytes, semantics) {
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
        // Unchecked on purpose: the ddt4all crosscheck feeds random frames.
        let items = req.decode_unchecked(&ecu, &frame.0)?;
        let out = serde_json::json!({
            "request": req.name,
            "response": frame,
            "items": items,
        });
        println!("{}", serde_json::to_string(&out)?);
    }
    Ok(())
}
