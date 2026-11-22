//! Capture -> replay round trips: the CLI's JSONL capture decodes offline with `kline replay`,
//! parses with kline-sim's reader, and drives kline-sim's `ReplayEcu` through the same session.

mod common;

use common::*;
use kline_sim::replay::{read_jsonl, ReplayEcu};

const LIVE: &[&str] = &["--json", "live", "--frames", "21A9,21AA", "--count", "2"];

fn live_args(x: &str) -> Vec<&str> {
    let mut v = LIVE.to_vec();
    v.extend(["--xml", x]);
    v
}

fn decoded_live(r: &Run) -> Vec<serde_json::Value> {
    r.event("live")
        .iter()
        .map(|e| e["decoded"].clone())
        .collect()
}

#[test]
fn capture_replays_offline_and_as_the_ecu() {
    let x = xml("sirius_cli.xml");
    let (first, j) = run(sirius_ecu(), &live_args(&x));
    assert_eq!(first.code, 0, "{}", first.stderr);
    j.assert_no_writes();
    let caps = first.captures();
    assert_eq!(caps.len(), 1, "one capture per session");
    let cap = caps[0].clone();

    // 1. Offline decode.
    let (r, _) = run(
        sirius_ecu(),
        &["--json", "replay", &path_str(&cap), "--xml", &x],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert!(!r.opened, "replay needs no port");
    // DDT2000 detection order: the (unanswered) fast init comes first, then the 5-baud init.
    let wakes = r.event("wake");
    assert_eq!(wakes[0]["kind"], "fast_init");
    let wake = wakes.iter().find(|w| w["kind"] == "five_baud").unwrap();
    assert_eq!(
        (wake["kind"].as_str(), wake["hex"].as_str()),
        (Some("five_baud"), Some("7A"))
    );
    assert_eq!(wake["reply"], "55D08F");
    let ex = r.event("exchange");
    let a9: Vec<_> = ex.iter().filter(|e| e["request"] == "21A9").collect();
    assert_eq!(a9.len(), 2);
    let sc = sirius_scenario();
    for e in &a9 {
        assert_eq!(
            e["replies"].as_array().unwrap().len(),
            1,
            "echo stripped: {e}"
        );
        for (name, v) in items(&e["decoded"][0]) {
            assert_eq!(v, scenario_json(sc.get(&name).unwrap()), "{name}");
        }
    }
    let reqs: Vec<&str> = ex.iter().map(|e| e["request"].as_str().unwrap()).collect();
    // "81" is the StartCommunication of the unanswered fast init.
    assert_eq!(reqs, ["81", "10A0", "21A9", "21AA", "21A9", "21AA", "82"]);

    // 2. The capture is kline-sim replay format, and replaying it as the ECU reproduces the
    //    session exactly (any difference would be a ReplayMismatch and silence).
    let entries = read_jsonl(std::io::BufReader::new(std::fs::File::open(&cap).unwrap())).unwrap();
    let ecu = ReplayEcu::new(entries);
    let rj = ecu.journal();
    let second = run_with(ecu, None, &live_args(&x), Opts::default());
    assert_eq!(second.code, 0, "{}\n{}", second.stdout, second.stderr);
    rj.assert_no_violations();
    assert_eq!(decoded_live(&first), decoded_live(&second));
}

#[test]
fn replay_rejects_a_broken_capture_with_exit_11() {
    let d = temp_dir("bad");
    let p = d.join("bad.jsonl");
    std::fs::write(&p, "{\"t_ms\":1,\"dir\":\"tx\",\"hex\":\"7\"}\n").unwrap();
    let (r, _) = run(sirius_ecu(), &["--json", "replay", &path_str(&p)]);
    assert_eq!(r.code, 11, "{}", r.stdout);
}

#[test]
fn replay_without_a_definition_still_splits_frames() {
    let x = xml("abs_fast.xml");
    let (first, _) = run(abs_ecu(), &["--json", "read", "--xml", &x, "21B1"]);
    assert_eq!(first.code, 0, "{}", first.stderr);
    let cap = first.captures()[0].clone();
    let (r, _) = run(abs_ecu(), &["--json", "replay", &path_str(&cap)]);
    assert_eq!(r.code, 0);
    assert_eq!(r.event("wake")[0]["kind"], "fast_init");
    let ex = r.event("exchange");
    // StartCommunication, the read, StopCommunication; separate length byte framing (KW1 EA).
    assert_eq!(ex[0]["request"], "81");
    assert_eq!(ex[1]["request"], "21B1");
    assert_eq!(ex[1]["replies"][0]["positive"], "61B1109A1081");
}

/// Re-review N5: an ISO8 status reply in a capture shows up in the replay JSON with its
/// DDT2000 name next to the raw code, for each of the four codes.
#[test]
fn replay_json_names_iso8_status_replies() {
    let d = temp_dir("iso8-status");
    let p = d.join("status.jsonl");
    let cases = [
        ("37", "requestUnknown"),
        ("38", "receiveError"),
        ("39", "invalidRequest"),
        ("15", "invalidRequest"),
    ];
    let mut log = String::new();
    for (i, (code, _)) in cases.iter().enumerate() {
        let t = 100 * (i as u64 + 1);
        let c = u8::from_str_radix(code, 16).unwrap();
        log += &format!("{{\"t_ms\":{t},\"dir\":\"tx\",\"hex\":\"03335187\"}}\n");
        log += &format!(
            "{{\"t_ms\":{},\"dir\":\"rx\",\"hex\":\"02{code}{:02X}\"}}\n",
            t + 20,
            2u8.wrapping_add(c)
        );
    }
    std::fs::write(&p, log).unwrap();
    let (r, _) = run(
        iso8_nofaults_ecu(),
        &[
            "--json",
            "replay",
            &path_str(&p),
            "--xml",
            &xml("iso8_nofaults.xml"),
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let ex = r.event("exchange");
    assert_eq!(ex.len(), cases.len(), "{}", r.stdout);
    for (e, (code, name)) in ex.iter().zip(cases) {
        assert_eq!(e["replies"][0]["iso8_status"], code, "{e}");
        assert_eq!(e["replies"][0]["name"], name, "{e}");
    }
}
