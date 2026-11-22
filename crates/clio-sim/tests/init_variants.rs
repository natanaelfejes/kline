//! Which 5-baud address byte the ECU accepts, and the CLI's InitVariant fallback order.

mod common;

use clio_core::{InitVariant, LinkError};
use clio_sim::driver::Driver;
use clio_sim::ecu::{AddressAccept, EcuProfile, EcuProtocol, SpecEcu};
use clio_sim::line::VirtualLine;
use common::*;

fn ecu(accept: AddressAccept, fast: bool) -> SpecEcu {
    let mut p = EcuProfile::sirius32();
    p.accept = accept;
    p.accept_fast_init = fast;
    SpecEcu::new(p, sirius(), sirius_scenario()).unwrap()
}

/// Outcome of one variant on a fresh ECU: the ~address byte, or the init error.
fn try_variant(accept: AddressAccept, v: InitVariant) -> Result<Option<u8>, LinkError> {
    let (mut d, _) = drive(ecu(accept, false));
    let mut c = cfg(&sirius());
    c.variant = v;
    d.init(&c)
        .map(|o| o.address_complement)
        .map_err(|e| e.init().cloned().unwrap())
}

#[test]
fn plain_and_xor80_acceptance_matrix() {
    use AddressAccept::*;
    use InitVariant::*;
    let none = Err(LinkError::NoKeywords { received: vec![] });
    assert_eq!(try_variant(Plain, SingleEcu), Ok(Some(0x85)));
    assert_eq!(try_variant(Plain, MultiEcu), none);
    assert_eq!(try_variant(Xor80, SingleEcu), none);
    assert_eq!(try_variant(Xor80, MultiEcu), Ok(Some(0x05)));
    assert_eq!(try_variant(Both, SingleEcu), Ok(Some(0x85)));
    assert_eq!(try_variant(Both, MultiEcu), Ok(Some(0x05)));
}

/// The fallback the CLI will implement: SingleEcu, then MultiEcu, then FastInit, on one line.
fn fallback(d: &mut Driver<VirtualLine>) -> Option<(InitVariant, Vec<u8>)> {
    for v in [
        InitVariant::SingleEcu,
        InitVariant::MultiEcu,
        InitVariant::FastInit,
    ] {
        let mut c = cfg(&sirius());
        c.variant = v;
        if let Ok(init) = d.init(&c) {
            let mut s = d.session(&c, &init);
            let r = positive(&d.request(&mut s, &[0x21, 0xA9]).unwrap());
            return Some((v, r));
        }
    }
    None
}

#[test]
fn fallback_finds_the_variant_the_ecu_accepts() {
    let want = |accept, fast| {
        let (mut d, j) = drive(ecu(accept, fast));
        let r = fallback(&mut d).map(|(v, r)| {
            assert_eq!(
                decode(&sirius(), "Live A9", &r),
                expected(&sirius(), "Live A9", &sirius_scenario())
            );
            v
        });
        j.assert_no_writes();
        // Strict tolerance; failed attempts leave no timing violations either (MultiEcu's
        // 7 ms inter-byte gap is within P4).
        j.assert_no_violations();
        r
    };
    assert_eq!(
        want(AddressAccept::Plain, false),
        Some(InitVariant::SingleEcu)
    );
    assert_eq!(
        want(AddressAccept::Xor80, false),
        Some(InitVariant::MultiEcu)
    );
    // An ECU that answers neither 5-baud byte, only fast init.
    let mut p = EcuProfile::sirius32();
    p.protocol = EcuProtocol::KwpFast;
    p.accept_fast_init = true;
    let (mut d, _) = drive(SpecEcu::new(p, sirius(), sirius_scenario()).unwrap());
    assert_eq!(
        fallback(&mut d).map(|(v, _)| v),
        Some(InitVariant::FastInit)
    );
}

#[test]
fn fast_init_variant() {
    let (mut d, j) = drive(ecu(AddressAccept::Both, true));
    let mut c = cfg(&sirius());
    c.variant = InitVariant::FastInit;
    let init = d.init(&c).unwrap();
    assert_eq!(init.keywords(), (0xD0, 0x8F));
    let mut s = d.session(&c, &init);
    positive(&d.request(&mut s, &[0x21, 0xAA]).unwrap());
    j.assert_no_violations();

    let (mut d, _) = drive(ecu(AddressAccept::Both, false));
    assert_eq!(d.init(&c).unwrap_err().init(), Some(&LinkError::NoReply));
}
