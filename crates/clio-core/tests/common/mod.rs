//! Shared test helpers: action/event scripts and Renault file access.
#![allow(dead_code)]

use std::fmt::Debug;

use clio_core::{Action, Event, Step};

pub fn dtr(on: bool) -> Action {
    Action::SetDtr(on)
}
pub fn brk(on: bool) -> Action {
    Action::SetBreak(on)
}
pub fn sleep(ms: u32) -> Action {
    Action::Sleep { ms }
}
pub fn flush() -> Action {
    Action::FlushRx
}
pub fn write(bytes: &[u8], inter_byte_ms: u32) -> Action {
    Action::Write {
        bytes: bytes.to_vec(),
        inter_byte_ms,
    }
}
pub fn read(count: usize, timeout_ms: u32) -> Action {
    Action::Read { count, timeout_ms }
}
pub fn done() -> Event {
    Event::Done
}
pub fn ok(b: &[u8]) -> Event {
    Event::ReadOk(b.to_vec())
}
pub fn timeout(b: &[u8]) -> Event {
    Event::ReadTimeout(b.to_vec())
}

/// One expected action and the event the fake driver answers with.
pub type Script = Vec<(Action, Event)>;

/// The 5-baud address bits of `byte`, LSB first: `SetBreak(bit == 0)`, then the bit time.
pub fn address_bits(byte: u8) -> Script {
    let mut s = Vec::new();
    for i in 0..8 {
        s.push((brk((byte >> i) & 1 == 0), done()));
        s.push((sleep(200), done()));
    }
    s
}

/// Play `script` against a machine: `first` is the action the machine started with, `feed`
/// delivers events. Asserts every action, and that the machine stops exactly when the script
/// ends. Returns the final step.
pub fn drive<T: Debug, E: Debug>(
    first: Action,
    script: &[(Action, Event)],
    mut feed: impl FnMut(Event) -> Step<T, E>,
) -> Step<T, E> {
    assert!(!script.is_empty(), "empty script");
    let mut current = first;
    for (i, (expected, ev)) in script.iter().enumerate() {
        assert_eq!(&current, expected, "action #{i} differs");
        match feed(ev.clone()) {
            Step::Act(a) => current = a,
            terminal => {
                assert_eq!(
                    i,
                    script.len() - 1,
                    "machine finished after action #{i} with {terminal:?}, script has more"
                );
                return terminal;
            }
        }
    }
    panic!("machine still wants {current:?} after the script ended");
}

/// Directory with the (gitignored) Renault definition files. `CLIO_DDT_XML` overrides the
/// default `<repo>/ddt-xml`.
pub fn ddt_xml_dir() -> std::path::PathBuf {
    match std::env::var_os("CLIO_DDT_XML") {
        Some(p) => std::path::PathBuf::from(p),
        None => std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../ddt-xml"),
    }
}

/// Bytes of a Renault file, or `None` (with a SKIPPED message) when it is not available
/// (SAFETY.md rule 1).
pub fn ddt_file(name: &str) -> Option<Vec<u8>> {
    let path = ddt_xml_dir().join(name);
    match std::fs::read(&path) {
        Ok(b) => Some(b),
        Err(e) => {
            eprintln!(
                "SKIPPED: {} not available ({e}); put the DDT2000 XML files in ddt-xml/",
                path.display()
            );
            None
        }
    }
}
