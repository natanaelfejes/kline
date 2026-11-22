//! Read-only identifier scan plan: exact 2-byte reads, one at a time, >= 200 ms apart, stops
//! after 3 missing replies or a security NRC.

use kline_core::frame::Reply;
use kline_core::scan::{
    ScanPlan, ScanResult, ScanService, ScanStep, ScanStop, MIN_SCAN_INTERVAL_MS,
};

fn send(plan: &mut ScanPlan, now: u64) -> Vec<u8> {
    match plan.next(now) {
        ScanStep::Send(r) => r.payload().to_vec(),
        other => panic!("expected Send at {now}, got {other:?}"),
    }
}

#[test]
fn only_two_byte_reads_over_the_range() {
    for (service, sid) in [
        (ScanService::LocalIdentifier, 0x21),
        (ScanService::EcuIdentification, 0x1A),
    ] {
        let mut plan = ScanPlan::new(service, 0xFE..=0xFF, 0);
        let ok = Reply::Positive(vec![sid + 0x40]);
        assert_eq!(send(&mut plan, 0), [sid, 0xFE]);
        plan.report(ScanResult::Reply(&ok));
        assert_eq!(send(&mut plan, 200), [sid, 0xFF]);
        plan.report(ScanResult::Reply(&ok));
        assert_eq!(plan.next(400), ScanStep::Finished);
    }
}

#[test]
fn rate_limit_and_one_at_a_time() {
    assert_eq!(MIN_SCAN_INTERVAL_MS, 200);
    let mut plan = ScanPlan::new(ScanService::LocalIdentifier, 0x00..=0x10, 50);
    assert_eq!(plan.interval_ms(), 200);
    send(&mut plan, 1000);
    assert_eq!(plan.next(5000), ScanStep::AwaitingReport);
    plan.report(ScanResult::Reply(&Reply::Positive(vec![0x61, 0x00])));
    assert_eq!(plan.next(1100), ScanStep::Wait { ms: 100 });
    assert_eq!(send(&mut plan, 1200), [0x21, 0x01]);
}

#[test]
fn stops_after_three_missing_replies() {
    let mut plan = ScanPlan::new(ScanService::LocalIdentifier, 0x00..=0xFF, 200);
    let mut now = 0;
    for _ in 0..2 {
        send(&mut plan, now);
        plan.report(ScanResult::Missing);
        now += 200;
    }
    // A reply in between does not reset the count (3 missing in total).
    send(&mut plan, now);
    plan.report(ScanResult::Reply(&Reply::Negative {
        sid: 0x21,
        nrc: 0x12,
    }));
    now += 200;
    send(&mut plan, now);
    plan.report(ScanResult::Missing);
    assert_eq!(
        plan.next(now + 200),
        ScanStep::Stopped(ScanStop::MissingReplies)
    );
    assert_eq!(
        plan.next(now + 10_000),
        ScanStep::Stopped(ScanStop::MissingReplies)
    );
}

#[test]
fn stops_at_first_security_nrc() {
    for nrc in [0x33u8, 0x35, 0x36] {
        let mut plan = ScanPlan::new(ScanService::EcuIdentification, 0x80..=0x90, 200);
        send(&mut plan, 0);
        plan.report(ScanResult::Reply(&Reply::Negative { sid: 0x1A, nrc }));
        assert_eq!(
            plan.next(1000),
            ScanStep::Stopped(ScanStop::SecurityNrc(nrc))
        );
    }
    // Other NRCs (e.g. 0x12 subFunctionNotSupported, 0x31 requestOutOfRange) keep going.
    let mut plan = ScanPlan::new(ScanService::EcuIdentification, 0x80..=0x90, 200);
    send(&mut plan, 0);
    plan.report(ScanResult::Reply(&Reply::Negative {
        sid: 0x1A,
        nrc: 0x31,
    }));
    assert!(matches!(plan.next(200), ScanStep::Send(_)));
}
