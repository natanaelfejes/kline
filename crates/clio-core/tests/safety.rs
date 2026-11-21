//! Safety policy on raw bytes (no Renault files needed).

use clio_core::safety::{
    check, check_approved, Approval, ApprovedRoutine, ConfirmationMismatch, DenyReason, Protocol,
    Target,
};

const SIRIUS: Target = Target {
    protocol: Protocol::Kwp,
    ecu_address: 0x7A,
};
const BMT: Target = Target {
    protocol: Protocol::Kwp,
    ecu_address: 0x26,
};
const SAFIR: Target = Target {
    protocol: Protocol::Iso8,
    ecu_address: 0x7A,
};

fn reason(t: &Target, p: &[u8]) -> Option<DenyReason> {
    check(t, p).err().map(|d| {
        assert_eq!(d.payload, p);
        d.reason
    })
}

#[test]
fn kwp_reads_allowed() {
    for p in [
        &[0x81][..],
        &[0x82],
        &[0x3E],
        &[0x3E, 0x01],
        &[0x10, 0x81],
        &[0x10, 0xA0],
        &[0x1A],
        &[0x1A, 0x80],
        &[0x17, 0xFF, 0x00],
        &[0x18, 0x02, 0xFF, 0x00],
        &[0x21, 0x80],
        &[0x21, 0x81],
        &[0x21, 0xA0],
        &[0x21, 0xA4],
    ] {
        let permit = check(&SIRIUS, p).unwrap_or_else(|d| panic!("{p:02X?}: {d}"));
        assert_eq!(permit.payload(), p);
    }
}

#[test]
fn kwp_everything_else_denied() {
    for (p, service) in [
        (&[0x27, 0x01][..], 0x27),
        (&[0x2E, 0x01, 0x00], 0x2E),
        (&[0x2F, 0x01], 0x2F),
        (&[0x30, 0xD0, 0x00, 0x0A], 0x30),
        (&[0x31, 0xA0, 0x00], 0x31),
        (&[0x31, 0xA2, 0x01, 0x00, 0x00], 0x31),
        (&[0x32, 0xA0, 0x00], 0x32),
        (&[0x34], 0x34),
        (&[0x35], 0x35),
        (&[0x36], 0x36),
        (&[0x37], 0x37),
        (&[0x3B, 0xD0, 0x10], 0x3B),
        (&[0x11, 0xFF], 0x11),
        (&[0x14, 0x00, 0x00], 0x14),
        (&[0x14, 0xFF, 0x00], 0x14),
        (&[0x20, 0x00], 0x20),
        (&[0x40, 0x02, 0x10], 0x40),
        (&[0x41, 0x01, 0, 0, 0], 0x41),
        (&[0x45], 0x45),
        (&[0x46, 0xFF], 0x46),
        (&[0x61, 0x02, 0x01], 0x61),
        (&[0x33, 0x50], 0x33),
        (&[0xFF, 0x00], 0xFF),
    ] {
        assert_eq!(
            reason(&SIRIUS, p),
            Some(DenyReason::ServiceNotAllowed { service }),
            "{p:02X?}"
        );
    }
    for p in [
        &[0x10, 0x85][..],
        &[0x10, 0x86],
        &[0x10],
        &[0x10, 0x81, 0x00],
        &[0x81, 0x00],
        &[0x21, 0xA0, 0x00],
        &[0x3E, 0x01, 0x00],
    ] {
        assert!(
            matches!(
                reason(&SIRIUS, p),
                Some(DenyReason::ParametersNotAllowed { .. })
            ),
            "{p:02X?}"
        );
    }
    assert_eq!(reason(&SIRIUS, &[]), Some(DenyReason::Empty));
}

#[test]
fn bmt_transponder_read_denied() {
    assert_eq!(reason(&BMT, &[0x21, 0xA4]), Some(DenyReason::Immobilizer));
    assert!(check(&BMT, &[0x21, 0xA3]).is_ok());
}

#[test]
fn iso8_allowlist() {
    for p in [
        &[0x21][..],
        &[0x21, 0x05],
        &[0x33, 0x50],
        &[0x33, 0x51],
        &[0x33, 0x52],
        &[0x33, 0x54],
        &[0x36],
        &[0x3F],
    ] {
        assert!(check(&SAFIR, p).is_ok(), "{p:02X?}");
    }
    for p in [
        &[0x33, 0x55][..],
        &[0x33, 0x4F],
        &[0x33],
        &[0x33, 0x50, 0x00],
        &[0x36, 0x00],
        &[0x31, 0x52],
        &[0x32, 0x51],
        &[0x40, 0x02, 0x10],
        &[0x41, 0x01, 0x00, 0x00, 0x00],
        &[0x45],
        &[0x46, 0xFF],
        &[0x20, 0x00],
        &[0x61, 0x02, 0x01],
        &[0xFF, 0x00],
        &[0x81],
        &[0x3E],
        &[0x10, 0x81],
    ] {
        assert!(check(&SAFIR, p).is_err(), "{p:02X?}");
    }
}

#[test]
fn approved_routines_need_approval_on_bmt() {
    for r in ApprovedRoutine::ALL {
        assert_eq!(
            reason(&BMT, r.payload()),
            Some(DenyReason::ApprovalRequired)
        );
        let a = Approval::from_typed_confirmation(r, r.confirmation_phrase()).unwrap();
        let permit = check_approved(&BMT, a).unwrap();
        assert_eq!(permit.payload(), r.payload());
    }
    assert_eq!(
        ApprovedRoutine::EraseRemotesAndLearnFirst.payload(),
        [0x31, 0xC7, 0x00, 0x00, 0x00]
    );
    assert_eq!(
        ApprovedRoutine::LearnSecondRemote.payload(),
        [0x31, 0xC8, 0x00, 0x00, 0x00]
    );
}

#[test]
fn approval_only_for_bmt() {
    let r = ApprovedRoutine::LearnSecondRemote;
    for t in [SIRIUS, SAFIR] {
        let a = Approval::from_typed_confirmation(r, r.confirmation_phrase()).unwrap();
        let d = check_approved(&t, a).unwrap_err();
        assert_eq!(d.reason, DenyReason::ApprovalWrongTarget);
    }
}

#[test]
fn approval_requires_exact_phrase() {
    let r = ApprovedRoutine::EraseRemotesAndLearnFirst;
    for typed in [
        "",
        "yes",
        "y",
        "erase all remotes and learn remote 1",
        "LEARN REMOTE 2",
    ] {
        assert_eq!(
            Approval::from_typed_confirmation(r, typed).unwrap_err(),
            ConfirmationMismatch
        );
    }
    assert!(
        Approval::from_typed_confirmation(r, "  ERASE ALL REMOTES AND LEARN REMOTE 1\n").is_ok()
    );
}

#[test]
fn routine_variants_are_not_approvable_by_bytes() {
    // Close relatives of the approved routines stay plain denied service 0x31.
    for p in [
        &[0x31, 0xC7][..],
        &[0x31, 0xC7, 0x00, 0x00, 0x01],
        &[0x31, 0xC8, 0x00, 0x00, 0x00, 0x00],
        &[0x31, 0x00, 0x00, 0x00],
    ] {
        assert_eq!(
            reason(&BMT, p),
            Some(DenyReason::ServiceNotAllowed { service: 0x31 })
        );
    }
}
