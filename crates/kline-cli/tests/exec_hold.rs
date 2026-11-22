//! `kline exec --hold/--watch` (final critique F01) and the report of an approved request whose
//! reply was lost (F06), end to end against kline-sim (virtual clock).

mod common;

use common::*;
use kline_sim::ecu::{FaultRule, ReplyFault, SpecEcu};
use kline_spec::Value;

const C7: [u8; 5] = [0x31, 0xC7, 0x00, 0x00, 0x00];
const C8: [u8; 5] = [0x31, 0xC8, 0x00, 0x00, 0x00];

fn interactive() -> Opts {
    Opts {
        interactive: true,
        human: Human::TypesShownCode,
        ..Opts::default()
    }
}

/// What went out after the approved request (exclusive), from the trace.
fn after(stderr: &str, approved: &[u8]) -> Vec<Vec<u8>> {
    let tx = tx_payloads(stderr);
    let i = tx
        .iter()
        .position(|p| p == approved)
        .unwrap_or_else(|| panic!("{approved:02X?} not sent: {tx:02X?}"));
    assert_eq!(
        tx.iter().filter(|p| p.as_slice() == approved).count(),
        1,
        "approved request sent more than once: {tx:02X?}"
    );
    tx[i + 1..].to_vec()
}

#[test]
fn hold_keeps_the_session_with_tester_present_only_then_stops() {
    let (r, j) = run_opts(
        bmt_ecu(),
        &[
            "--trace",
            "exec",
            "--xml",
            &xml("bmt_cli.xml"),
            "31C7000000",
            "--hold",
            "10",
        ],
        interactive(),
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let rest = after(&r.stderr, &C7);
    let (last, mid) = rest.split_last().expect("nothing after the request");
    assert_eq!(
        last,
        &vec![0x82],
        "ends with StopCommunication: {rest:02X?}"
    );
    assert!(
        mid.iter().all(|p| p.first() == Some(&0x3E)),
        "only TesterPresent while holding: {rest:02X?}"
    );
    // 10 s at TesterPresent 3000 ms (definition): 3 of them.
    assert!(mid.len() >= 3 && mid.len() <= 4, "{rest:02X?}");
    assert!(
        r.stdout.contains("holding the session for 10 s"),
        "{}",
        r.stdout
    );
    let w = j.writes();
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!(w[0].payload, C7.to_vec());
    j.assert_no_violations();
}

#[test]
fn watch_sends_only_the_read_and_prints_what_changed() {
    // The remote is pressed 2 s after 31 C7: the BMT then reports the frame in 21 A3.
    let ecu = SpecEcu::new(
        kline_sim::ecu::EcuProfile::bmt(),
        fixture("bmt_cli.xml"),
        bmt_scenario().number("TIR RF RECU", 0.0),
    )
    .unwrap()
    .with_change_after(&C7, 2000, "TIR RF RECU", Value::Number(1.0));
    let (r, j) = run_opts(
        ecu,
        &[
            "--trace",
            "exec",
            "--xml",
            &xml("bmt_cli.xml"),
            "31C7000000",
            "--hold",
            "5",
            "--watch",
            "21A3",
        ],
        interactive(),
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let rest = after(&r.stderr, &C7);
    let (last, mid) = rest.split_last().unwrap();
    assert_eq!(last, &vec![0x82], "{rest:02X?}");
    assert!(
        mid.iter().all(|p| p == &vec![0x21, 0xA3]),
        "only the watch read: {rest:02X?}"
    );
    // 5 s every 500 ms.
    assert!(mid.len() >= 9 && mid.len() <= 11, "{} reads", mid.len());
    // First read: everything, with the flag meaning; later only the change.
    assert!(r.stdout.contains("TIR RF RECU"), "{}", r.stdout);
    assert!(
        r.stdout
            .contains("TIR RF RECU 0 -> 1  (1 = a remote frame was received)"),
        "{}",
        r.stdout
    );
    assert_eq!(
        r.stdout.matches("TIR RF RECU 0 -> 1").count(),
        1,
        "printed once, when it changed:\n{}",
        r.stdout
    );
    let w = j.writes();
    assert_eq!(w.len(), 1, "exactly one approved request: {w:?}");
    assert_eq!(w[0].payload, C7.to_vec());
}

#[test]
fn watch_interval_sets_the_read_period() {
    let (r, j) = run_opts(
        bmt_ecu(),
        &[
            "--trace",
            "exec",
            "--xml",
            &xml("bmt_cli.xml"),
            "Learn remote key 2",
            "--hold",
            "4",
            "--watch",
            "Remote status",
            "--watch-interval",
            "1000",
        ],
        interactive(),
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let rest = after(&r.stderr, &C8);
    let reads = rest.iter().filter(|p| *p == &vec![0x21, 0xA3]).count();
    assert!((4..=5).contains(&reads), "{reads}: {rest:02X?}");
    assert_eq!(j.writes().len(), 1);
}

#[test]
fn ctrl_c_during_hold_sends_stop_and_nothing_else() {
    let (r, j) = run_opts(
        bmt_ecu(),
        &[
            "--trace",
            "exec",
            "--xml",
            &xml("bmt_cli.xml"),
            "31C7000000",
            "--hold",
            "60",
            "--watch",
            "21A3",
        ],
        Opts {
            ctrl_c_after_tx: Some((C7.to_vec(), 1500)),
            ..interactive()
        },
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let rest = after(&r.stderr, &C7);
    assert_eq!(rest.last(), Some(&vec![0x82]), "{rest:02X?}");
    // About 1.5 s of reads, not 60 s.
    assert!(rest.len() <= 6, "{rest:02X?}");
    assert!(
        r.stdout.contains("stopped by Ctrl-C, Stop sent"),
        "{}",
        r.stdout
    );
    assert_eq!(j.writes().len(), 1);
}

#[test]
fn a_negative_reply_is_not_held() {
    let ecu = bmt_ecu().with_fault(FaultRule::on(&C7, ReplyFault::Negative { nrc: 0x22 }));
    let (r, j) = run_opts(
        ecu,
        &[
            "--trace",
            "exec",
            "--xml",
            &xml("bmt_cli.xml"),
            "31C7000000",
            "--hold",
            "30",
            "--watch",
            "21A3",
        ],
        interactive(),
    );
    assert_eq!(r.code, 6, "{}\n{}", r.stdout, r.stderr);
    assert_eq!(after(&r.stderr, &C7), vec![vec![0x82]]);
    assert_eq!(j.writes().len(), 1);
}

#[test]
fn hold_and_watch_are_checked_before_anything_is_opened() {
    let x = xml("bmt_cli.xml");
    for (args, code) in [
        // A non-read watch: refused (exit 7), nothing sent.
        (vec!["31C7000000", "--hold", "30", "--watch", "14FF00"], 7),
        (
            vec!["31C7000000", "--hold", "30", "--watch", "31C8000000"],
            7,
        ),
        (vec!["31C7000000", "--hold", "30", "--watch", "21A401"], 7),
        // Limits and combinations: usage (exit 2).
        (vec!["31C7000000", "--hold", "61"], 2),
        (vec!["31C7000000", "--hold", "0"], 2),
        (vec!["31C7000000", "--watch", "21A3"], 2),
        (
            vec![
                "31C7000000",
                "--hold",
                "5",
                "--watch",
                "21A3",
                "--watch-interval",
                "2",
            ],
            2,
        ),
        (vec!["31C7000000", "--hold", "5", "--stop", "31C8000000"], 2),
    ] {
        let mut full = vec!["exec", "--xml", &x];
        full.extend(args.iter().copied());
        let (r, j) = run_opts(bmt_ecu(), &full, interactive());
        assert_eq!(r.code, code, "{args:?}: {}\n{}", r.stdout, r.stderr);
        assert!(!r.opened, "{args:?}: port opened");
        assert!(r.screen.is_empty(), "{args:?}");
        j.assert_no_writes();
    }
}

#[test]
fn a_lost_reply_says_the_request_was_sent_and_is_never_repeated() {
    for fault in [
        ReplyFault::NoReply,
        ReplyFault::BadChecksum,
        ReplyFault::DropBytes { count: 2 },
    ] {
        let ecu = bmt_ecu().with_fault(FaultRule::on(&C7, fault));
        let (r, j) = run_opts(
            ecu,
            &[
                "--trace",
                "exec",
                "--xml",
                &xml("bmt_cli.xml"),
                "31C7000000",
            ],
            interactive(),
        );
        assert_eq!(r.code, 13, "{fault:?}: {}\n{}", r.stdout, r.stderr);
        for needle in [
            "WAS SENT to the ECU",
            "its reply was not received correctly",
            "do NOT repeat it blindly",
            "first read the state (e.g. kline read --xml",
            "21A3)",
        ] {
            assert!(
                r.stderr.contains(needle),
                "{fault:?}: {needle:?}\n{}",
                r.stderr
            );
        }
        // Sent once, then only Stop: no re-send (no --hold given; with it see
        // a_lost_reply_still_holds_and_watches_without_repeating_the_request).
        assert_eq!(after(&r.stderr, &C7), vec![vec![0x82]], "{fault:?}");
        assert_eq!(j.writes().len(), 1, "{fault:?}");
    }
    // A request that never reached the wire is not reported as sent: wrong code, exit 9.
    let (r, j) = run_opts(
        bmt_ecu(),
        &["exec", "--xml", &xml("bmt_cli.xml"), "31C7000000"],
        Opts {
            human: Human::TypesWrongCode,
            ..interactive()
        },
    );
    assert_eq!(r.code, 9);
    assert!(!r.stderr.contains("WAS SENT"));
    j.assert_no_writes();
}

#[test]
fn a_lost_reply_is_in_the_saved_output_with_its_exit_code() {
    let ecu = bmt_ecu().with_fault(FaultRule::on(&C7, ReplyFault::NoReply));
    let (r, _) = run_opts(
        ecu,
        &["exec", "--xml", &xml("bmt_cli.xml"), "31C7000000"],
        interactive(),
    );
    assert_eq!(r.code, 13, "{}", r.stderr);
    // The human record next to the capture keeps the same text.
    let out: String = std::fs::read_dir(&r.capture_dir)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.to_string_lossy().ends_with(".out.txt"))
        .map(|p| std::fs::read_to_string(p).unwrap())
        .collect();
    assert!(out.contains("WAS SENT to the ECU"), "{out}");
    assert!(out.contains("done (exit 13)"), "{out}");
}

#[test]
fn risk_prints_the_exact_exec_command_with_hold_and_watch() {
    let x = xml("bmt_cli.xml");
    let (r, j) = run(
        bmt_ecu(),
        &[
            "risk",
            "--xml",
            &x,
            "31C7000000",
            "--hold",
            "30",
            "--watch",
            "21A3",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let want = format!("exec --xml {x} 31C7000000 --hold 30 --watch 21A3");
    assert!(r.stdout.contains(&want), "{want}\n{}", r.stdout);
    assert!(!r.opened);
    j.assert_no_writes();
    // JSON carries it too.
    let (r, _) = run(
        bmt_ecu(),
        &[
            "--json",
            "risk",
            "--xml",
            &x,
            "31C8000000",
            "--hold",
            "30",
            "--watch",
            "21A3",
        ],
    );
    let e = &r.event("risk")[0];
    let c = e["command"].as_str().unwrap_or_default();
    assert!(
        c.ends_with(&format!("exec --xml {x} 31C8000000 --hold 30 --watch 21A3")),
        "{e}"
    );
    // Remote learning without --hold: the suggested command holds and watches.
    let (r, _) = run(bmt_ecu(), &["risk", "--xml", &x, "31C7000000"]);
    assert!(
        r.stdout
            .contains(&format!("exec --xml {x} 31C7000000 --hold 30 --watch 21A3")),
        "{}",
        r.stdout
    );
    // A non-read watch is refused by risk as well.
    let (r, _) = run(
        bmt_ecu(),
        &[
            "risk",
            "--xml",
            &x,
            "31C7000000",
            "--hold",
            "30",
            "--watch",
            "14FF00",
        ],
    );
    assert_eq!(r.code, 7, "{}", r.stdout);
}

/// Re-check R11: after exit 13 (approved frame written, reply lost, damaged or late) with
/// --hold/--watch the watch still runs for the hold time (only the Tier 0 read and
/// TesterPresent), then Stop; the exit code stays 13 and the request goes out exactly once.
#[test]
fn a_lost_reply_still_holds_and_watches_without_repeating_the_request() {
    for fault in [
        ReplyFault::NoReply,
        ReplyFault::BadChecksum,
        ReplyFault::Slow { delay_ms: 1500 },
    ] {
        let ecu = bmt_ecu().with_fault(FaultRule::on(&C7, fault));
        let (r, j) = run_opts(
            ecu,
            &[
                "--trace",
                "exec",
                "--xml",
                &xml("bmt_cli.xml"),
                "31C7000000",
                "--hold",
                "5",
                "--watch",
                "21A3",
            ],
            interactive(),
        );
        assert_eq!(r.code, 13, "{fault:?}: {}\n{}", r.stdout, r.stderr);
        assert!(r.stderr.contains("WAS SENT to the ECU"), "{fault:?}");
        let rest = after(&r.stderr, &C7);
        let (last, mid) = rest.split_last().unwrap();
        assert_eq!(last, &vec![0x82], "{fault:?}: {rest:02X?}");
        assert!(
            mid.iter()
                .all(|p| p == &vec![0x21, 0xA3] || p.first() == Some(&0x3E)),
            "{fault:?}: only the watch read and TesterPresent: {rest:02X?}"
        );
        let reads = mid.iter().filter(|p| *p == &vec![0x21, 0xA3]).count();
        assert!(reads >= 6, "{fault:?}: watched for the hold: {rest:02X?}");
        assert!(
            r.stdout.contains("holding the session for 5 s"),
            "{fault:?}"
        );
        let w = j.writes();
        assert_eq!(w.len(), 1, "{fault:?}: approved request once: {w:?}");
        assert_eq!(w[0].payload, C7.to_vec());
    }
    // --hold without --watch: TesterPresent only, still exit 13.
    let ecu = bmt_ecu().with_fault(FaultRule::on(&C7, ReplyFault::NoReply));
    let (r, j) = run_opts(
        ecu,
        &[
            "--trace",
            "exec",
            "--xml",
            &xml("bmt_cli.xml"),
            "31C7000000",
            "--hold",
            "10",
        ],
        interactive(),
    );
    assert_eq!(r.code, 13, "{}\n{}", r.stdout, r.stderr);
    let rest = after(&r.stderr, &C7);
    let (last, mid) = rest.split_last().unwrap();
    assert_eq!(last, &vec![0x82]);
    assert!(
        !mid.is_empty() && mid.iter().all(|p| p.first() == Some(&0x3E)),
        "{rest:02X?}"
    );
    assert_eq!(j.writes().len(), 1);
}
