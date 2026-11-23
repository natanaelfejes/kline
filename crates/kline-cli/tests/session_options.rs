//! Fixes from the first real car session: `read` with several requests, `live --cycles` /
//! `--duration`, `--compact` with `--items`, the capture directory of every command, and a
//! global `--init-attempts`.

mod common;

use common::*;
use kline_sim::ecu::InitFault;

fn live_args<'a>(sx: &'a str, extra: &[&'a str]) -> Vec<&'a str> {
    let mut v = vec!["--json", "live", "--xml", sx, "--rate", "10"];
    v.extend_from_slice(extra);
    v
}

fn requests(r: &Run, event: &str) -> Vec<String> {
    r.event(event)
        .iter()
        .map(|e| e["request"].as_str().unwrap().to_string())
        .collect()
}

// ---- read with several requests -------------------------------------------------------

#[test]
fn read_takes_several_requests_in_one_session_in_order() {
    let sx = xml("sirius_cli.xml");
    let (r, j) = run(
        sirius_ecu(),
        &["--json", "read", "--xml", &sx, "21AA", "21A9", "Live AA"],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert_eq!(requests(&r, "read"), ["21AA", "21A9", "21AA"]);
    for e in r.event("read") {
        assert!(!items(&e["decoded"][0]).is_empty(), "{e}");
    }
    // One session: one capture, one Stop.
    assert_eq!(r.event("stopped").len(), 1);
    assert_eq!(r.captures().len(), 1);
    j.assert_no_writes();
    j.assert_no_violations();
}

#[test]
fn read_human_output_has_one_block_per_request() {
    let sx = xml("sirius_cli.xml");
    let (r, _) = run(sirius_ecu(), &["read", "--xml", &sx, "21A9", "21AA"]);
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let a = r.stdout.find("21 A9 -> 61 A9").expect(&r.stdout);
    let b = r.stdout.find("21 AA -> 61 AA").expect(&r.stdout);
    assert!(a < b);
}

#[test]
fn read_with_one_non_read_among_several_refuses_everything_before_the_init() {
    let sx = xml("sirius_cli.xml");
    for bad in ["11FF", "Reset learning", "2701", "3BD01005", "1085"] {
        let (r, j) = run(
            sirius_ecu(),
            &["--json", "read", "--xml", &sx, "21A9", bad, "21AA"],
        );
        assert_eq!(r.code, 7, "{bad}: {}", r.stdout);
        assert!(!r.opened, "{bad}: port opened");
        assert!(r.event("read").is_empty(), "{bad}: nothing was read");
        j.assert_no_writes();
    }
}

#[test]
fn read_continues_after_a_negative_reply_and_exits_6() {
    let sx = xml("sirius_cli.xml");
    // 21B0: the fixture's ECU answers 7F 21 12.
    let (r, _) = run(
        sirius_ecu(),
        &["--json", "read", "--xml", &sx, "21A9", "21B0", "21AA"],
    );
    assert_eq!(r.code, 6, "{}\n{}", r.stdout, r.stderr);
    let reads = r.event("read");
    assert_eq!(reads.len(), 3, "{}", r.stdout);
    assert_eq!(reads[1]["negative"]["nrc"], "12");
    assert!(
        reads[2]["reply"].is_string(),
        "21AA still read: {}",
        reads[2]
    );
    assert_eq!(r.event("read_error").len(), 1);
}

#[test]
fn read_set_needs_a_single_request() {
    let sx = xml("sirius_cli.xml");
    let (r, _) = run(
        sirius_ecu(),
        &["read", "--xml", &sx, "21A9", "21AA", "--set", "X=1"],
    );
    assert_eq!(r.code, 2, "{}\n{}", r.stdout, r.stderr);
    assert!(!r.opened);
}

#[test]
fn read_needs_at_least_one_request() {
    let sx = xml("sirius_cli.xml");
    let (r, _) = run(sirius_ecu(), &["read", "--xml", &sx]);
    assert_eq!(r.code, 2);
    assert!(!r.opened);
}

// ---- live --cycles / --duration / --count ---------------------------------------------

#[test]
fn live_cycles_stops_cleanly_after_n_cycles() {
    let sx = xml("sirius_cli.xml");
    let (r, j) = run(
        sirius_ecu(),
        &live_args(&sx, &["--frames", "21A9", "--cycles", "4"]),
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert_eq!(r.event("live").len(), 4);
    let s = &r.event("stopped")[0];
    assert_eq!(
        (s["ok"].as_bool(), s["ctrl_c"].as_bool()),
        (Some(true), Some(false))
    );
    j.assert_no_writes();
}

#[test]
fn live_count_is_the_same_option_as_cycles() {
    let sx = xml("sirius_cli.xml");
    let (r, _) = run(
        sirius_ecu(),
        &live_args(&sx, &["--frames", "21A9,21AA", "--count", "3"]),
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert_eq!(r.event("live").len(), 6, "3 cycles x 2 frames");
}

#[test]
fn live_duration_stops_after_the_seconds() {
    let sx = xml("sirius_cli.xml");
    let (r, j) = run(
        sirius_ecu(),
        &[
            "--json",
            "live",
            "--xml",
            &sx,
            "--frames",
            "21A9",
            "--rate",
            "2",
            "--duration",
            "2",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let live = r.event("live");
    // 2 Hz for 2 s: cycles at about 0, 500, 1000, 1500 ms; none at or after 2000 ms.
    assert_eq!(live.len(), 4, "{}", r.stdout);
    let first = live[0]["t_ms"].as_u64().unwrap();
    let last = live[3]["t_ms"].as_u64().unwrap();
    assert!(last - first < 2000, "{first} {last}");
    assert_eq!(r.event("stopped")[0]["ok"], true);
    j.assert_no_writes();
}

#[test]
fn live_cycles_and_duration_stop_at_whichever_comes_first() {
    let sx = xml("sirius_cli.xml");
    let base = |extra: &[&'static str]| {
        let mut v = vec![
            "--json", "live", "--xml", &sx, "--frames", "21A9", "--rate", "2",
        ];
        v.extend_from_slice(extra);
        run(sirius_ecu(), &v).0
    };
    let r = base(&["--duration", "60", "--cycles", "2"]);
    assert_eq!(r.event("live").len(), 2);
    let r = base(&["--duration", "1", "--cycles", "50"]);
    assert_eq!(
        r.event("live").len(),
        2,
        "1 s at 2 Hz: cycles at 0 and 500 ms"
    );
}

#[test]
fn live_duration_zero_is_a_usage_error() {
    let sx = xml("sirius_cli.xml");
    let (r, _) = run(
        sirius_ecu(),
        &live_args(&sx, &["--frames", "21A9", "--duration", "0"]),
    );
    assert_eq!(r.code, 2);
    assert!(!r.opened);
}

#[test]
fn live_with_a_duplicated_frame_reads_it_every_time() {
    let sx = xml("sirius_cli.xml");
    let (r, j) = run(
        sirius_ecu(),
        &live_args(&sx, &["--frames", "21A9,21AA,21A9", "--cycles", "2"]),
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert_eq!(
        requests(&r, "live"),
        ["21A9", "21AA", "21A9", "21A9", "21AA", "21A9"]
    );
    j.assert_no_violations();
}

#[test]
fn live_compact_items_prints_only_changed_values() {
    let sx = xml("sirius_cli.xml");
    let (r, _) = run(
        sirius_ecu(),
        &[
            "live",
            "--xml",
            &sx,
            "--frames",
            "21A9,21AA",
            "--rate",
            "10",
            "--cycles",
            "3",
            "--compact",
            "--items",
            "Engine speed",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let lines: Vec<&str> = r.stdout.lines().filter(|l| l.starts_with('#')).collect();
    assert_eq!(lines.len(), 3, "{}", r.stdout);
    assert!(lines[0].ends_with("Engine speed=850 rpm"), "{lines:?}");
    assert!(lines[1].ends_with("(no change)"), "{lines:?}");
    assert!(lines[2].ends_with("(no change)"), "{lines:?}");
}

// ---- capture directory ----------------------------------------------------------------

fn interactive() -> Opts {
    Opts {
        interactive: true,
        ..Opts::default()
    }
}

fn assert_captured(r: &Run, what: &str) {
    assert!(
        !r.captures().is_empty(),
        "{what}: no capture in {}\n{}\n{}",
        r.capture_dir.display(),
        r.stdout,
        r.stderr
    );
}

#[test]
fn exec_and_clear_dtc_write_the_capture_where_capture_dir_says() {
    let sx = xml("sirius_cli.xml");
    for position in [CaptureArg::BeforeCommand, CaptureArg::AfterCommand] {
        let o = || Opts {
            capture: position,
            ..interactive()
        };
        let (r, _) = run_opts(sirius_ecu(), &["exec", "--xml", &sx, "11FF"], o());
        assert_eq!(r.code, 0, "{}\n{}\n{}", r.stdout, r.stderr, r.screen);
        assert_captured(&r, "exec");
        let (r, _) = run_opts(sirius_ecu(), &["clear-dtc", "--xml", &sx], o());
        assert_eq!(r.code, 0, "{}\n{}\n{}", r.stdout, r.stderr, r.screen);
        assert_captured(&r, "clear-dtc");
    }
}

#[test]
fn kline_capture_dir_is_honoured_by_every_command() {
    let sx = xml("sirius_cli.xml");
    let env = |o: Opts| Opts {
        capture: CaptureArg::Env,
        ..o
    };
    let (r, _) = run_opts(
        sirius_ecu(),
        &["exec", "--xml", &sx, "11FF"],
        env(interactive()),
    );
    assert_eq!(r.code, 0, "{}\n{}\n{}", r.stdout, r.stderr, r.screen);
    assert_captured(&r, "exec");
    let (r, _) = run_opts(
        sirius_ecu(),
        &["clear-dtc", "--xml", &sx],
        env(interactive()),
    );
    assert_eq!(r.code, 0, "{}\n{}\n{}", r.stdout, r.stderr, r.screen);
    assert_captured(&r, "clear-dtc");
    for args in [
        vec!["read", "--xml", &sx, "21A9"],
        vec!["live", "--xml", &sx, "--frames", "21A9", "--cycles", "1"],
        vec!["connect", "--xml", &sx],
        vec!["dtc", "--xml", &sx],
        vec!["ident", "--xml", &sx],
    ] {
        let (r, _) = run_opts(sirius_ecu(), &args, env(Opts::default()));
        assert_captured(&r, args[0]);
    }
}

// ---- global --init-attempts -----------------------------------------------------------

#[test]
fn init_attempts_works_before_and_after_the_command() {
    let sx = xml("sirius_cli.xml");
    let silent = || sirius_ecu().with_init_fault(InitFault::Silent);
    let rounds = |args: &[&str]| run(silent(), args).0.event("init_attempt").len();
    // 3 variants per round.
    assert_eq!(
        rounds(&["--json", "--init-attempts", "1", "connect", "--xml", &sx]),
        3
    );
    assert_eq!(
        rounds(&["--json", "connect", "--xml", &sx, "--init-attempts", "1"]),
        3
    );
    assert_eq!(
        rounds(&[
            "--json",
            "read",
            "--xml",
            &sx,
            "21A9",
            "--init-attempts",
            "2"
        ]),
        6
    );
}

// ---- review round: line error, Ctrl-C, silent ECU ---------------------------------------

#[test]
fn read_line_error_after_a_negative_reply_is_the_exit_and_names_the_skipped() {
    use kline_sim::ecu::{FaultRule, ReplyFault};
    let sx = xml("sirius_cli.xml");
    let ecu = sirius_ecu().with_fault(FaultRule::on(&[0x21, 0xAA], ReplyFault::NoReply));
    let (r, _) = run(
        ecu,
        &["--json", "read", "--xml", &sx, "21B0", "21AA", "21A9"],
    );
    // 21B0 negative (6), then 21AA: no reply, the more severe line error (5) wins.
    assert_eq!(r.code, 5, "{}\n{}", r.stdout, r.stderr);
    let errs = r.event("read_error");
    let which: Vec<&str> = errs
        .iter()
        .map(|e| e["request"].as_str().unwrap())
        .collect();
    assert_eq!(which, ["21B0", "21AA"], "{}", r.stdout);
    let sk = &r.event("read_skipped")[0];
    assert_eq!(sk["requests"], serde_json::json!(["21A9"]));
    assert_eq!(sk["reason"], "line error");
    assert!(r.event("read").iter().all(|e| e["request"] != "21A9"));
}

#[test]
fn read_ctrl_c_between_requests_names_the_requests_not_read() {
    let sx = xml("sirius_cli.xml");
    let (r, _) = run_opts(
        sirius_ecu(),
        &["--json", "read", "--xml", &sx, "21A9", "21AA", "21A9"],
        Opts {
            ctrl_c_after_tx: Some((vec![0x21, 0xA9], 0)),
            ..Opts::default()
        },
    );
    assert_eq!(r.event("read").len(), 1, "{}", r.stdout);
    let sk = &r.event("read_skipped")[0];
    assert_eq!(sk["requests"], serde_json::json!(["21AA", "21A9"]));
    assert_eq!(sk["reason"], "Ctrl-C");
    assert_eq!(r.event("stopped")[0]["ctrl_c"], true);
}

#[test]
fn live_duration_with_a_silent_ecu_is_exit_5() {
    use kline_sim::ecu::{FaultRule, ReplyFault};
    let sx = xml("sirius_cli.xml");
    let ecu = sirius_ecu().with_fault(FaultRule::on(&[0x21, 0xA9], ReplyFault::NoReply));
    let (r, _) = run(
        ecu,
        &live_args(&sx, &["--frames", "21A9", "--duration", "1"]),
    );
    assert_eq!(r.code, 5, "{}\n{}", r.stdout, r.stderr);
    assert!(r.event("live").is_empty());
    assert_eq!(r.event("stopped")[0]["ok"], true, "Stop is still sent");
}

#[test]
fn live_huge_duration_does_not_overflow() {
    let sx = xml("sirius_cli.xml");
    let (r, _) = run(
        sirius_ecu(),
        &live_args(
            &sx,
            &[
                "--frames",
                "21A9",
                "--cycles",
                "1",
                "--duration",
                "18446744073709551615",
            ],
        ),
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert_eq!(r.event("live").len(), 1);
}

#[test]
fn global_init_attempts_is_carried_into_the_printed_command() {
    let sx = xml("sirius_cli.xml");
    let (r, _) = run(
        sirius_ecu(),
        &[
            "--json",
            "--init-attempts",
            "1",
            "risk",
            "--xml",
            &sx,
            "11FF",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert!(r.stdout.contains("--init-attempts 1"), "{}", r.stdout);
}
