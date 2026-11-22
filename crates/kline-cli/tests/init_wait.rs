//! Final critique F14 (P3max wait between init attempts) and F12 (one fast-init retry after a
//! timing-guard abort), against kline-sim (virtual clock).

mod common;

use common::*;
use kline_sim::ecu::InitFault;

fn t_of(line: &str) -> f64 {
    let s: String = line
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    s.parse().unwrap()
}

/// Times of the failed-attempt lines and of the init starts, in trace order.
fn attempt_gaps(trace: &str) -> Vec<f64> {
    let lines: Vec<&str> = trace.lines().collect();
    let mut gaps = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if l.contains("INIT attempt") && l.contains("failed") {
            if let Some(next) = lines[i..].iter().find(|n| {
                n.contains("] INIT 5-baud address byte") || n.contains("] INIT fast init")
            }) {
                gaps.push(t_of(next) - t_of(l));
            }
        }
    }
    gaps
}

#[test]
fn after_an_attempt_the_ecu_answered_the_next_init_waits_p3max() {
    // The ECU sends its key bytes but never ~address: it heard the wake-up and may still be
    // in its half-open session for up to P3max (5000 ms, ISO 14230-2).
    let ecu = sirius_ecu().with_init_fault(InitFault::NoAddressComplement);
    let (r, _) = run(
        ecu,
        &[
            "--trace",
            "connect",
            "--xml",
            &xml("sirius_cli.xml"),
            "--variant",
            "single",
            "--init-attempts",
            "2",
        ],
    );
    assert_eq!(r.code, 4, "{}\n{}", r.stdout, r.stderr);
    let gaps = attempt_gaps(&r.stderr);
    assert_eq!(gaps.len(), 1, "{gaps:?}\n{}", r.stderr);
    assert!(gaps[0] >= 5000.0, "{gaps:?}");
    assert!(
        r.stderr
            .contains("waiting 5000 ms (P3max) before the next init"),
        "{}",
        r.stderr
    );
    assert!(
        r.stdout.contains("waiting 5000 ms (P3max)"),
        "the human line says so: {}",
        r.stdout
    );
}

#[test]
fn after_a_silent_attempt_the_next_init_does_not_wait_p3max() {
    let ecu = sirius_ecu().with_init_fault(InitFault::Silent);
    let (r, _) = run(
        ecu,
        &[
            "--trace",
            "connect",
            "--xml",
            &xml("sirius_cli.xml"),
            "--variant",
            "single",
            "--init-attempts",
            "2",
        ],
    );
    assert_eq!(r.code, 4, "{}\n{}", r.stdout, r.stderr);
    let gaps = attempt_gaps(&r.stderr);
    assert_eq!(gaps.len(), 1, "{gaps:?}");
    assert!(gaps[0] < 1000.0, "{gaps:?}");
    assert!(!r.stderr.contains("P3max"), "{}", r.stderr);
}
