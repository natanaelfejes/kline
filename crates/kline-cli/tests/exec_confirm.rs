//! `kline exec` confirmation: a mistyped code gives one more attempt with a new code (Neon
//! critique F1), and a Tier 2 `--stop` carries the session's identification (Neon F1).

mod common;

use common::*;

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
