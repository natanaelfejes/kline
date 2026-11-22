//! Shared test helpers: action/event scripts and Renault file access.
#![allow(dead_code)]

use std::fmt::Debug;

use clio_core::frame::checksum;
use clio_core::init::{FastInit, FiveBaudInit, InitOutcome};
use clio_core::{Action, Event, InitKind, LinkConfig, Step};

/// An expected [`Action`]. `Action::Write` carries a crate-private `Frame` that tests cannot
/// build, so traces are written with this mirror and compared through `PartialEq<A>`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum A {
    SetDtr(bool),
    SetBreak(bool),
    Sleep(u32),
    CheckBreakLoopback,
    FlushRx,
    Write(Vec<u8>, u32),
    Read(usize, u32),
}

impl From<&Action> for A {
    fn from(a: &Action) -> A {
        match a {
            Action::SetDtr(b) => A::SetDtr(*b),
            Action::SetBreak(b) => A::SetBreak(*b),
            Action::Sleep { ms } => A::Sleep(*ms),
            Action::CheckBreakLoopback => A::CheckBreakLoopback,
            Action::FlushRx => A::FlushRx,
            Action::Write {
                frame,
                inter_byte_ms,
            } => A::Write(frame.bytes().to_vec(), *inter_byte_ms),
            Action::Read { count, timeout_ms } => A::Read(*count, *timeout_ms),
        }
    }
}

impl PartialEq<A> for Action {
    // The mirror has to be built to compare: Action::Write's frame is crate-private.
    #[allow(clippy::cmp_owned)]
    fn eq(&self, other: &A) -> bool {
        A::from(self) == *other
    }
}

/// The action of a `Step::Act`, panicking otherwise.
pub fn act<T: Debug, E: Debug>(s: Step<T, E>) -> Action {
    match s {
        Step::Act(a) => a,
        other => panic!("expected an action, got {other:?}"),
    }
}

pub fn dtr(on: bool) -> A {
    A::SetDtr(on)
}
pub fn brk(on: bool) -> A {
    A::SetBreak(on)
}
pub fn sleep(ms: u32) -> A {
    A::Sleep(ms)
}
pub fn flush() -> A {
    A::FlushRx
}
pub fn loopback() -> A {
    A::CheckBreakLoopback
}
pub fn write(bytes: &[u8], inter_byte_ms: u32) -> A {
    A::Write(bytes.to_vec(), inter_byte_ms)
}
pub fn read(count: usize, timeout_ms: u32) -> A {
    A::Read(count, timeout_ms)
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
pub type Script = Vec<(A, Event)>;

/// The 5-baud address bits as literal `SetBreak` values (true = K line low = bit 0), LSB first,
/// written out by hand per address byte (not computed from the byte).
pub fn literal_bits(byte: u8) -> [bool; 8] {
    match byte {
        // 0x7A = 0111_1010 -> LSB first 0,1,0,1,1,1,1,0
        0x7A => [true, false, true, false, false, false, false, true],
        // 0xFA = 1111_1010 -> 0,1,0,1,1,1,1,1
        0xFA => [true, false, true, false, false, false, false, false],
        // 0x26 = 0010_0110 -> 0,1,1,0,0,1,0,0
        0x26 => [true, false, false, true, true, false, true, true],
        // 0xA6 = 1010_0110 -> 0,1,1,0,0,1,0,1
        0xA6 => [true, false, false, true, true, false, true, false],
        _ => panic!("no literal waveform for {byte:#04x}"),
    }
}

/// The 5-baud address bits of `byte` from [`literal_bits`], each followed by the bit time.
pub fn address_bits(byte: u8) -> Script {
    let mut s = Vec::new();
    for low in literal_bits(byte) {
        s.push((brk(low), done()));
        s.push((sleep(200), done()));
    }
    s
}

/// Run a real init for `cfg` against a cooperative fake ECU answering key bytes `kw1 kw2`
/// (and `~address` / `C1 KW1 KW2` as needed). Tests get an [`InitOutcome`] only this way.
pub fn open(cfg: &LinkConfig, kw1: u8, kw2: u8) -> InitOutcome {
    match cfg.init_kind() {
        InitKind::KwpFast { .. } => {
            let mut m = FastInit::new(cfg).expect("fast init config");
            let first = m.start();
            let mut reply = vec![0x83, cfg.tool_address, cfg.ecu_address, 0xC1, kw1, kw2];
            reply.push(checksum(&reply));
            let mut echo: Option<Vec<u8>> = None;
            let mut replies = vec![reply[1..].to_vec(), reply[..1].to_vec()];
            auto(
                first,
                |a| match a {
                    A::Write(b, _) => {
                        echo = Some(b.clone());
                        Event::Done
                    }
                    A::Read(n, _) => match echo.take() {
                        Some(e) if e.len() == *n => Event::ReadOk(e),
                        _ => Event::ReadOk(replies.pop().expect("more reads than scripted")),
                    },
                    _ => Event::Done,
                },
                |ev| m.on_event(ev),
            )
        }
        _ => {
            let mut m = FiveBaudInit::new(cfg).expect("5-baud config");
            let first = m.start();
            let addr = cfg.ecu_address;
            let mut kw_done = false;
            auto(
                first,
                |a| match a {
                    A::CheckBreakLoopback => Event::BreakLoopbackSeen(true),
                    A::Read(3, _) if !kw_done => {
                        kw_done = true;
                        Event::ReadOk(vec![0x55, kw1, kw2])
                    }
                    A::Read(2, _) => Event::ReadOk(vec![!kw2, !addr]),
                    _ => Event::Done,
                },
                |ev| m.on_event(ev),
            )
        }
    }
}

fn auto(
    first: Action,
    mut answer: impl FnMut(&A) -> Event,
    mut feed: impl FnMut(Event) -> Step<InitOutcome, clio_core::LinkError>,
) -> InitOutcome {
    let mut a = first;
    for _ in 0..200 {
        let ev = answer(&A::from(&a));
        match feed(ev) {
            Step::Act(n) => a = n,
            Step::Done(o) => return o,
            Step::Failed(e) => panic!("init failed: {e:?}"),
        }
    }
    panic!("init did not finish")
}

/// Play `script` against a machine: `first` is the action the machine started with, `feed`
/// delivers events. Asserts every action, and that the machine stops exactly when the script
/// ends. Returns the final step.
pub fn drive<T: Debug, E: Debug>(
    first: Action,
    script: &[(A, Event)],
    mut feed: impl FnMut(Event) -> Step<T, E>,
) -> Step<T, E> {
    assert!(!script.is_empty(), "empty script");
    let mut current = first;
    for (i, (expected, ev)) in script.iter().enumerate() {
        assert_eq!(&A::from(&current), expected, "action #{i} differs");
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
