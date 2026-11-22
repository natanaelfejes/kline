//! `kline exec` confirmation: a mistyped code gives one more attempt with a new code (Neon
//! critique F1), and a Tier 2 `--stop` carries the session's identification (Neon F1).

mod common;

use common::*;
use kline_sim::ecu::{FaultRule, ReplyFault};

const C7: [u8; 5] = [0x31, 0xC7, 0x00, 0x00, 0x00];

fn script(s: &'static [Answer]) -> Opts {
    Opts {
        interactive: true,
        human: Human::Script(s),
        ..Opts::default()
    }
}

/// The codes the screens asked for, in order.
fn codes(screen: &str) -> Vec<String> {
    screen
        .split("Type the code ")
        .skip(1)
        .map(|s| s.split_whitespace().next().unwrap().to_string())
        .collect()
}

#[test]
fn a_mistyped_code_gives_one_more_attempt_with_a_new_code() {
    let (r, j) = run_opts(
        sirius_ecu(),
        &["exec", "--xml", &xml("sirius_cli.xml"), "11FF"],
        script(&[Answer::Wrong, Answer::Shown]),
    );
    assert_eq!(r.code, 0, "{}\n{}\n{}", r.stdout, r.stderr, r.screen);
    let c = codes(&r.screen);
    assert_eq!(c.len(), 2, "{}", r.screen);
    assert_ne!(c[0], c[1], "the second attempt has a new code");
    assert!(
        r.screen
            .contains("confirmation code does not match; nothing was sent"),
        "{}",
        r.screen
    );
    assert!(r.screen.contains("one more attempt"), "{}", r.screen);
    let w = j.writes();
    assert_eq!(w.len(), 1, "sent once: {w:?}");
    assert_eq!(w[0].payload, vec![0x11, 0xFF]);
}

#[test]
fn two_mistyped_codes_abort_with_exit_9_and_send_nothing() {
    let (r, j) = run_opts(
        sirius_ecu(),
        &["exec", "--xml", &xml("sirius_cli.xml"), "11FF"],
        script(&[Answer::Wrong, Answer::Wrong, Answer::Shown]),
    );
    assert_eq!(r.code, 9, "{}\n{}", r.stdout, r.stderr);
    assert_eq!(codes(&r.screen).len(), 2, "no third attempt: {}", r.screen);
    assert!(
        r.stderr
            .contains("confirmation code does not match; nothing was sent"),
        "{}",
        r.stderr
    );
    j.assert_no_writes();
}

#[test]
fn a_mistyped_stop_code_gives_one_more_attempt() {
    let abs = xml("abs_fast.xml");
    let args = [
        "exec",
        "--xml",
        &abs,
        "Pump test",
        "--set",
        "Pump=1",
        "--stop",
        "Pump test",
        "--stop-set",
        "Pump=0",
    ];
    let (r, j) = run_opts(
        abs_ecu(),
        &args,
        script(&[Answer::Shown, Answer::Wrong, Answer::Shown]),
    );
    assert_eq!(r.code, 0, "{}\n{}\n{}", r.stdout, r.stderr, r.screen);
    let sent: Vec<Vec<u8>> = j.writes().into_iter().map(|w| w.payload).collect();
    assert_eq!(sent, vec![vec![0x30, 0xA0, 0x01], vec![0x30, 0xA0, 0x00]]);
    // Twice wrong for the stop: the stop is not sent, exit 9.
    let (r, j) = run_opts(
        abs_ecu(),
        &args,
        script(&[Answer::Shown, Answer::Wrong, Answer::Wrong]),
    );
    assert_eq!(r.code, 9, "{}\n{}", r.stdout, r.stderr);
    assert!(r.stdout.contains("STOP REQUEST NOT SENT"), "{}", r.stdout);
    let sent: Vec<Vec<u8>> = j.writes().into_iter().map(|w| w.payload).collect();
    assert_eq!(sent, vec![vec![0x30, 0xA0, 0x01]]);
}

#[test]
fn a_tier_2_stop_carries_the_identification_of_the_session() {
    let (r, j) = run_opts(
        bmt_ecu(),
        &[
            "--trace",
            "exec",
            "--xml",
            &xml("bmt_cli.xml"),
            "31C7000000",
            "--stop",
            "3BD20501",
        ],
        script(&[Answer::Shown]),
    );
    // The fixture BMT has no 3B D2 definition: the sim answers 7F 3B 12 (exit 6), but the
    // stop was approved and sent, after the 21 80 identification and the 21 A1 backup that
    // were read before the request.
    assert_eq!(r.code, 6, "{}\n{}\n{}", r.stdout, r.stderr, r.screen);
    let tx = tx_payloads(&r.stderr);
    let pos = |p: &[u8]| tx.iter().position(|x| x == p).unwrap_or(usize::MAX);
    assert!(
        pos(&[0x21, 0x80]) < pos(&[0x21, 0xA1]) && pos(&[0x21, 0xA1]) < pos(&C7),
        "{tx:02X?}"
    );
    let sent: Vec<Vec<u8>> = j.writes().into_iter().map(|w| w.payload).collect();
    assert_eq!(sent, vec![C7.to_vec(), vec![0x3B, 0xD2, 0x05, 0x01]]);
    assert!(r.screen.contains("config write (tier 2)"), "{}", r.screen);
}

#[test]
fn a_tier_2_stop_without_identification_is_refused_before_anything_is_sent() {
    let ecu = bmt_ecu().with_fault(FaultRule::on(&[0x21, 0x80], ReplyFault::NoReply));
    let (r, j) = run_opts(
        ecu,
        &[
            "exec",
            "--xml",
            &xml("bmt_cli.xml"),
            "31C7000000",
            "--stop",
            "3BD20501",
        ],
        script(&[Answer::Shown]),
    );
    assert_ne!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert!(
        r.stderr.contains("--stop") && r.stderr.contains("nothing was sent"),
        "{}",
        r.stderr
    );
    assert!(r.screen.is_empty(), "no approval shown: {}", r.screen);
    j.assert_no_writes();
}
