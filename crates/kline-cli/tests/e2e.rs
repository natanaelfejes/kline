//! `kline` end to end against kline-sim, in-process (virtual clock; the simulator's strict timing
//! checks apply). Every test also checks the ECU write log.

mod common;

use common::*;
use kline_sim::ecu::{AddressAccept, EcuProfile, EcuProtocol, FaultRule, ReplyFault, SpecEcu};
use kline_sim::line::VirtualLine;
use serde_json::json;

fn sirius_with(accept: AddressAccept, fast: bool) -> SpecEcu {
    let mut p = EcuProfile::sirius32();
    p.accept = accept;
    p.accept_fast_init = fast;
    SpecEcu::new(p, fixture("sirius_cli.xml"), sirius_scenario()).unwrap()
}

fn connected(r: &Run) -> serde_json::Value {
    let c = r.event("connected");
    assert_eq!(c.len(), 1, "stdout: {}\nstderr: {}", r.stdout, r.stderr);
    c[0].clone()
}

#[test]
fn connect_with_each_explicit_variant() {
    let sx = xml("sirius_cli.xml");
    for (variant, accept, fast, byte, compl) in [
        ("single", AddressAccept::Both, false, Some("7A"), Some("85")),
        ("multi", AddressAccept::Both, false, Some("FA"), Some("05")),
        ("fast", AddressAccept::Both, true, None, None),
    ] {
        let (r, j) = run(
            sirius_with(accept, fast),
            &["--json", "connect", "--xml", &sx, "--variant", variant],
        );
        assert_eq!(r.code, 0, "{variant}: {}\n{}", r.stdout, r.stderr);
        let c = connected(&r);
        assert_eq!(c["variant"], variant);
        assert_eq!(c["address_byte"], json!(byte));
        assert_eq!(c["address_complement"], json!(compl));
        assert_eq!(
            (c["kw1"].as_str(), c["kw2"].as_str()),
            (Some("D0"), Some("8F"))
        );
        assert_eq!(c["keywords_match"], json!(true));
        // The Sirius definition has 10 A0: the session is opened (Tier 0).
        assert_eq!(r.event("session_start")[0]["ok"], json!(true));
        assert_eq!(r.event("stopped")[0]["ok"], json!(true));
        j.assert_no_writes();
        j.assert_no_violations();
    }
}

fn attempts(r: &Run) -> Vec<(String, bool)> {
    r.event("init_attempt")
        .iter()
        .map(|a| {
            (
                a["variant"].as_str().unwrap().to_string(),
                a["ok"].as_bool().unwrap(),
            )
        })
        .collect()
}

#[test]
fn auto_falls_back_to_multi_when_the_ecu_accepts_only_the_xor80_byte() {
    // DDT2000 order (x65 addressing: FastInit, then 5-baud Type I), then our MultiEcu.
    let (r, j) = run(
        sirius_with(AddressAccept::Xor80, false),
        &["--json", "connect", "--xml", &xml("sirius_cli.xml")],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert_eq!(
        attempts(&r),
        [
            ("fast".into(), false),
            ("single".into(), false),
            ("multi".into(), true)
        ]
    );
    assert_eq!(r.event("init_attempt")[1]["address_byte"], "7A");
    assert_eq!(connected(&r)["variant"], "multi");
    j.assert_no_writes();
}

#[test]
fn auto_tries_plain_5_baud_right_after_fast_init() {
    let (r, _) = run(
        sirius_with(AddressAccept::Plain, false),
        &["--json", "connect", "--xml", &xml("sirius_cli.xml")],
    );
    assert_eq!(r.code, 0);
    assert_eq!(
        attempts(&r),
        [("fast".into(), false), ("single".into(), true)]
    );
    assert_eq!(connected(&r)["variant"], "single");
}

#[test]
fn auto_tries_fast_init_first() {
    let mut p = EcuProfile::sirius32();
    p.protocol = EcuProtocol::KwpFast;
    p.accept_fast_init = true;
    let ecu = SpecEcu::new(p, fixture("sirius_cli.xml"), sirius_scenario()).unwrap();
    let (r, j) = run(ecu, &["--json", "connect", "--xml", &xml("sirius_cli.xml")]);
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert_eq!(attempts(&r), [("fast".into(), true)]);
    j.assert_no_writes();
}

#[test]
fn every_variant_failing_is_exit_4_init_after_three_rounds() {
    let ecu = sirius_ecu().with_init_fault(kline_sim::ecu::InitFault::Silent);
    let (r, _) = run(ecu, &["--json", "connect", "--xml", &xml("sirius_cli.xml")]);
    assert_eq!(r.code, 4);
    assert_eq!(r.event("error")[0]["kind"], "init");
    let a = r.event("init_attempt");
    // DDT2000 "Init retry n/3": 3 rounds of the 3 variants.
    assert_eq!(a.len(), 9);
    let rounds: Vec<u64> = a.iter().map(|x| x["round"].as_u64().unwrap()).collect();
    assert_eq!(rounds, [1, 1, 1, 2, 2, 2, 3, 3, 3]);
    // --init-attempts 1: one round only.
    let ecu = sirius_ecu().with_init_fault(kline_sim::ecu::InitFault::Silent);
    let (r, _) = run(
        ecu,
        &[
            "--json",
            "connect",
            "--xml",
            &xml("sirius_cli.xml"),
            "--init-attempts",
            "1",
        ],
    );
    assert_eq!(r.event("init_attempt").len(), 3);
}

#[test]
fn ident_reads_2180_and_chooses_the_fitting_definition() {
    // Two candidates: the BMT fixture does not fit the Sirius 2180 reply (MinBytes / layout
    // differ in the other direction would also do); the Sirius one does.
    let (r, j) = run(
        sirius_ecu(),
        &[
            "--json",
            "ident",
            "--xml",
            &xml("sirius_cli.xml"),
            "--xml",
            &xml("abs_fast.xml"),
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let id = &r.event("ident")[0];
    assert_eq!(id["request"], "2180");
    assert!(id["chosen"].as_str().unwrap().ends_with("sirius_cli.xml"));
    // abs_fast declares other key bytes (EA 8F), so it cannot fit.
    assert_eq!(id["candidates"][1]["keywords_match"], json!(false));
    assert_eq!(id["candidates"][1]["fits"], json!(false));
    assert_eq!(id["ambiguous"], json!(false));
    let cal = &id["candidates"][0]["decoded"][0]["items"][0];
    assert_eq!(
        (cal["name"].as_str(), cal["value"].as_str()),
        (Some("Calibration"), Some("SIM-0001"))
    );
    j.assert_no_writes();
}

#[test]
fn live_decodes_to_the_scenario_values() {
    let (r, j) = run(
        sirius_ecu(),
        &[
            "--json",
            "live",
            "--xml",
            &xml("sirius_cli.xml"),
            "--frames",
            "21A9,Live AA",
            "--rate",
            "5",
            "--count",
            "3",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let live = r.event("live");
    assert_eq!(live.len(), 6, "3 cycles x 2 frames");
    let sc = sirius_scenario();
    for e in &live {
        let it = items(&e["decoded"][0]);
        assert!(!it.is_empty());
        for (name, v) in it {
            assert_eq!(v, scenario_json(sc.get(&name).unwrap()), "{name}");
        }
    }
    // 5 Hz: cycle starts 200 ms apart (virtual clock).
    let t: Vec<u64> = live
        .iter()
        .step_by(2)
        .map(|e| e["t_ms"].as_u64().unwrap())
        .collect();
    assert!(t[1] - t[0] >= 200 && t[1] - t[0] < 230, "{t:?}");
    j.assert_no_writes();
    j.assert_no_violations();
}

#[test]
fn live_sends_tester_present_when_idle_longer_than_the_tp_delay() {
    let (r, j) = run(
        sirius_ecu(),
        &[
            "--json",
            "--trace",
            "live",
            "--xml",
            &xml("sirius_cli.xml"),
            "--frames",
            "21A9",
            "--rate",
            "0.2",
            "--count",
            "2",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    // 5 s between cycles > TesterPresent 3000 ms: `3E` goes out (81 7A F1 3E 72).
    assert!(r.stderr.contains("TX 81 7A F1 3E 2A"), "{}", r.stderr);
    assert_eq!(r.event("live").len(), 2);
    j.assert_no_writes();
}

#[test]
fn dtc_shows_the_fault_flags_that_are_set() {
    let (r, j) = run(
        sirius_ecu(),
        &["--json", "dtc", "--xml", &xml("sirius_cli.xml")],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let d = &r.event("dtc")[0];
    assert_eq!(d["request"], "21A0");
    let active: Vec<&str> = d["active"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(active, ["Injector 1 circuit", "Knock sensor circuit"]);
    assert_eq!(r.event("dtc_summary")[0]["active"], 2);
    j.assert_no_writes();
}

#[test]
fn scan_stops_after_three_missing_replies() {
    let ecu = sirius_ecu()
        .with_fault(FaultRule::on(&[0x21, 0xB2], ReplyFault::NoReply))
        .with_fault(FaultRule::on(&[0x21, 0xB4], ReplyFault::NoReply))
        .with_fault(FaultRule::on(&[0x21, 0xB5], ReplyFault::NoReply));
    let (r, j) = run(
        ecu,
        &[
            "--json",
            "scan",
            "--xml",
            &xml("sirius_cli.xml"),
            "--range",
            "21:B0-C5",
        ],
    );
    assert_eq!(r.code, 12, "{}\n{}", r.stdout, r.stderr);
    let s = r.event("scan");
    let ids: Vec<(&str, &str)> = s
        .iter()
        .map(|e| (e["id"].as_str().unwrap(), e["result"].as_str().unwrap()))
        .collect();
    assert_eq!(
        ids,
        [
            ("B0", "negative"),
            ("B1", "negative"),
            ("B2", "missing"),
            ("B3", "negative"),
            ("B4", "missing"),
            ("B5", "missing"),
        ]
    );
    let done = &r.event("scan_done")[0];
    assert_eq!(
        (done["sent"].as_u64(), done["stopped"].as_str()),
        (Some(6), Some("missing_replies"))
    );
    j.assert_no_writes();
}

#[test]
fn scan_refuses_non_read_services_before_connecting() {
    let (r, j) = run(
        sirius_ecu(),
        &[
            "--json",
            "scan",
            "--xml",
            &xml("sirius_cli.xml"),
            "--range",
            "31:00-FF",
        ],
    );
    assert_eq!(r.code, 7);
    assert!(!r.opened, "port must not be opened");
    j.assert_no_writes();
}

#[test]
fn read_of_a_non_read_is_refused_before_any_init() {
    for req in ["11FF", "Reset learning", "2701", "3BD01005", "1085"] {
        let (r, j) = run(
            sirius_ecu(),
            &["--json", "read", "--xml", &xml("sirius_cli.xml"), req],
        );
        assert_eq!(r.code, 7, "{req}: {}", r.stdout);
        assert_eq!(r.event("error")[0]["kind"], "refused");
        assert!(!r.opened, "{req}: port opened");
        j.assert_no_writes();
    }
}

#[test]
fn read_decodes_and_negative_reply_is_exit_6() {
    let (r, _) = run(
        sirius_ecu(),
        &["--json", "read", "--xml", &xml("sirius_cli.xml"), "21AA"],
    );
    assert_eq!(r.code, 0);
    let e = &r.event("read")[0];
    assert_eq!(e["names"], json!(["Live AA"]));
    assert_eq!(items(&e["decoded"][0])["Coolant temperature"], json!(88.0));
    let (r, _) = run(
        sirius_ecu(),
        &["--json", "read", "--xml", &xml("sirius_cli.xml"), "21B0"],
    );
    assert_eq!(r.code, 6, "{}", r.stdout);
    assert_eq!(r.event("read")[0]["negative"]["nrc"], "12");
}

#[test]
fn exec_is_refused_with_json_and_without_a_tty() {
    for (args, interactive) in [
        (
            vec!["--json", "exec", "--xml", &*xml("sirius_cli.xml"), "11FF"],
            true,
        ),
        (
            vec!["exec", "--xml", &*xml("sirius_cli.xml"), "11FF"],
            false,
        ),
    ] {
        let (r, j) = run_opts(
            sirius_ecu(),
            &args,
            Opts {
                interactive,
                ..Opts::default()
            },
        );
        assert_eq!(r.code, 7, "{}\n{}", r.stdout, r.stderr);
        assert!(!r.opened, "port must not be opened");
        assert!(r.screen.is_empty(), "no approval prompt");
        j.assert_no_writes();
    }
}

#[test]
fn exec_sends_only_after_the_shown_code_is_typed() {
    let (r, j) = run_opts(
        sirius_ecu(),
        &["exec", "--xml", &xml("sirius_cli.xml"), "Reset learning"],
        Opts {
            interactive: true,
            human: Human::TypesShownCode,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    // The prompt quotes the RiskInfo verbatim.
    assert!(
        r.screen.contains("APPROVAL REQUIRED: service (tier 1)"),
        "{}",
        r.screen
    );
    assert!(
        r.screen.contains("erases learned adaptations"),
        "{}",
        r.screen
    );
    assert!(r.screen.contains("ERASES_LEARNED_VALUES"));
    let w = j.writes();
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!(w[0].payload, vec![0x11, 0xFF]);
}

#[test]
fn exec_with_a_wrong_code_sends_nothing() {
    let (r, j) = run_opts(
        sirius_ecu(),
        &["exec", "--xml", &xml("sirius_cli.xml"), "11FF"],
        Opts {
            interactive: true,
            human: Human::TypesWrongCode,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, 9, "{}\n{}", r.stdout, r.stderr);
    j.assert_no_writes();
}

#[test]
fn exec_expert_tier_shows_unknown_effect_and_a_10_char_code() {
    let (r, j) = run_opts(
        sirius_ecu(),
        &["exec", "--xml", &xml("sirius_cli.xml"), "31A400"],
        Opts {
            interactive: true,
            human: Human::TypesShownCode,
            ..Opts::default()
        },
    );
    assert_eq!(
        r.code, 6,
        "sim answers an unknown routine 7F: {}\n{}",
        r.stdout, r.stderr
    );
    assert!(r.screen.contains("expert (tier 3)") && r.screen.contains("UNKNOWN_EFFECT"));
    let code = r.screen.split("Type the code ").nth(1).unwrap();
    assert_eq!(code.split_whitespace().next().unwrap().len(), 10);
    assert_eq!(
        j.writes().len(),
        1,
        "the approved expert request reached the ECU"
    );
}

#[test]
fn exec_refuses_forbidden_requests_even_interactively() {
    for req in ["31A0A3", "3BA000", "2EAE00", "38A0A3", "82"] {
        let (r, j) = run_opts(
            sirius_ecu(),
            &["exec", "--xml", &xml("sirius_cli.xml"), req],
            Opts {
                interactive: true,
                human: Human::TypesShownCode,
                ..Opts::default()
            },
        );
        assert_eq!(r.code, 7, "{req}: {}", r.stderr);
        assert!(r.screen.is_empty(), "{req}: no approval offered");
        j.assert_no_writes();
    }
}

#[test]
fn trace_prints_every_byte_with_timing_and_saves_it() {
    let (r, _) = run(
        sirius_ecu(),
        &["--trace", "read", "--xml", &xml("sirius_cli.xml"), "21A9"],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    for needle in [
        "INIT 5-baud address byte 7A",
        "DTR low",
        "BREAK on  (K low)",
        "BREAK LOOPBACK seen",
        "SLEEP 200 ms",
        "RX 3/3",
        ": 55 D0 8F",
        "TX 70 (inter-byte 0 ms)",
        "TX 82 7A F1 21 A9 B7",
        "TX 81 7A F1 82 6E",
    ] {
        assert!(
            r.stderr.contains(needle),
            "missing {needle:?} in\n{}",
            r.stderr
        );
    }
    // Every trace line carries a time stamp.
    assert!(r
        .stderr
        .lines()
        .filter(|l| l.contains("TX "))
        .all(|l| l.starts_with('[') && l.contains(" ms] ")));
    let cap = &r.captures()[0];
    let trace = std::fs::read_to_string(cap.with_extension("trace.txt")).unwrap();
    assert!(trace.contains("TX 82 7A F1 21 A9 B7"));
}

#[test]
fn can_definitions_are_refused_clearly() {
    let (r, _) = run(
        sirius_ecu(),
        &["--json", "connect", "--xml", &xml("can_only.xml")],
    );
    assert_eq!(r.code, 8);
    assert!(r.event("error")[0]["message"]
        .as_str()
        .unwrap()
        .contains("CAN not supported by a KKL cable yet"));
    assert!(!r.opened);
}

#[test]
fn non_clio_fast_init_ecu_end_to_end() {
    let abs = xml("abs_fast.xml");
    let veh = xml("vehicle_cli.xml");
    let (r, j) = run(
        abs_ecu(),
        &["--json", "connect", "--xml", &abs, "--vehicle", &veh],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let c = connected(&r);
    assert_eq!(
        c["variant"], "fast",
        "auto: only fast init applies to a FastInit definition"
    );
    assert_eq!(r.event("init_attempt").len(), 1);
    assert_eq!(
        (c["kw1"].as_str(), c["ecu_address"].as_str()),
        (Some("EA"), Some("28"))
    );
    assert_eq!(c["vehicle"]["function"], "ABS");
    assert!(
        r.event("session_start").is_empty(),
        "no 10 xx in this definition"
    );
    let (r, _) = run(
        abs_ecu(),
        &["--json", "read", "--xml", &abs, "Wheel speeds"],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let it = items(&r.event("read")[0]["decoded"][0]);
    assert_eq!(it["Front left speed"], json!(42.5));
    assert_eq!(it["Front right speed"], json!(42.25));
    let (r, _) = run(abs_ecu(), &["--json", "dtc", "--xml", &abs]);
    assert_eq!(r.code, 0);
    assert_eq!(r.event("dtc")[0]["active"][0]["name"], "Pump motor fault");
    j.assert_no_writes();
}

#[test]
fn non_clio_name_guard_refuses_immobilizer_requests() {
    let abs = xml("abs_fast.xml");
    // `Ecriture code` writes the data item "Code antidemarrage": refused by name.
    let (r, _) = run(abs_ecu(), &["--json", "risk", "--xml", &abs, "3B801234"]);
    assert_eq!(r.code, 7, "{}", r.stdout);
    let e = &r.event("risk")[0];
    assert_eq!(e["allowed"], json!(false));
    assert!(e["refused"].as_str().unwrap().contains("immobilizer"));
    let (r, j) = run_opts(
        abs_ecu(),
        &["exec", "--xml", &abs, "Ecriture code"],
        Opts {
            interactive: true,
            human: Human::TypesShownCode,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, 7);
    assert!(r.screen.is_empty() && !r.opened);
    j.assert_no_writes();
    // An ordinary output test of the same ECU is Tier 1 and preparable.
    let (r, _) = run(abs_ecu(), &["--json", "risk", "--xml", &abs, "Pump test"]);
    assert_eq!(r.code, 0);
    assert_eq!(r.event("risk")[0]["risk"]["tier_number"], 1);
}

/// Security review L4 / code review L6: the name guard and the class come from every loaded
/// candidate definition (union), not only the first.
#[test]
fn definition_guard_is_the_union_of_all_candidates() {
    let d = temp_dir("guard-union");
    let abs = std::fs::read_to_string(fixture_path("abs_fast.xml")).unwrap();
    let neutral = d.join("neutral.xml");
    std::fs::write(&neutral, abs.replace("Code antidemarrage", "Code reglage")).unwrap();
    let body = d.join("body.xml");
    std::fs::write(
        &body,
        abs.replace(
            "Target Name=\"CLI fixture ABS\"",
            "Target Name=\"CLI fixture BMT\"",
        ),
    )
    .unwrap();
    let (neutral, body, abs) = (path_str(&neutral), path_str(&body), xml("abs_fast.xml"));
    let risk = |xmls: &[&str], req: &str| {
        let mut args = vec!["--json", "risk"];
        for x in xmls {
            args.extend(["--xml", x]);
        }
        args.push(req);
        run(abs_ecu(), &args).0
    };
    assert_eq!(risk(&[&neutral], "3B801234").code, 0);
    let r = risk(&[&neutral, &abs], "3B801234");
    assert_eq!(r.code, 7, "{}", r.stdout);
    assert!(r.event("risk")[0]["refused"]
        .as_str()
        .unwrap()
        .contains("immobilizer"));
    // Class: the second candidate is a BMT definition, so the BMT rules apply.
    assert_eq!(risk(&[&neutral], "2E1000").code, 0);
    assert_eq!(risk(&[&neutral, &body], "2E1000").code, 7);
}

#[test]
fn address_override_and_risk_report() {
    let (r, _) = run(
        bmt_ecu(),
        &[
            "--json",
            "risk",
            "--xml",
            &xml("bmt_cli.xml"),
            "Clear faults",
        ],
    );
    assert_eq!(r.code, 0);
    let e = &r.event("risk")[0];
    assert_eq!(e["names"], json!(["Clear faults"]));
    assert_eq!(e["risk"]["reversible"], json!(false));
    assert_eq!(e["risk"]["warnings"], json!(["ERASES_FAULT_MEMORY"]));
    // --address moves the policy target: 31 C7 is BMT remote learning only at 0x26.
    let (r, _) = run(
        bmt_ecu(),
        &["--json", "risk", "--xml", &xml("bmt_cli.xml"), "31C7000000"],
    );
    assert_eq!(
        r.event("risk")[0]["risk"]["warnings"],
        json!(["ERASES_REMOTES"])
    );
    let (r, _) = run(
        bmt_ecu(),
        &[
            "--json",
            "risk",
            "--xml",
            &xml("bmt_cli.xml"),
            "--address",
            "0x51",
            "31C7000000",
        ],
    );
    // The definition is a BMT ("CLI fixture BMT"): the BMT rules follow the definition to any
    // address (security re-check finding 2), so 31 C7 stays the Tier 1 remote learning.
    assert_eq!(r.code, 0, "{}", r.stdout);
    let e = &r.event("risk")[0];
    assert_eq!(e["ecu_address"], "51");
    assert_eq!(e["risk"]["warnings"], json!(["ERASES_REMOTES"]));
    // ... and the immobilizer routines stay forbidden there.
    let (r, _) = run(
        bmt_ecu(),
        &[
            "--json",
            "risk",
            "--xml",
            &xml("bmt_cli.xml"),
            "--address",
            "0x51",
            "31C2000000",
        ],
    );
    assert_eq!(r.code, 7, "{}", r.stdout);
    // Same bytes, no definition name: plain expert tier.
    let (r, _) = run(
        bmt_ecu(),
        &[
            "--json",
            "risk",
            "--xml",
            &xml("abs_fast.xml"),
            "--address",
            "0x51",
            "31C7000000",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.event("risk")[0]["risk"]["tier_number"], 3);
}

#[test]
fn bmt_ident_with_two_2180_requests_and_remote_learning_exec() {
    let (r, j) = run(
        bmt_ecu(),
        &["--json", "ident", "--xml", &xml("bmt_cli.xml")],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    let d = &r.event("ident")[0]["candidates"][0]["decoded"];
    assert_eq!(
        d.as_array().unwrap().len(),
        2,
        "IDENTIFICATION and TYPE DE CARTE share 2180"
    );
    j.assert_no_writes();
    let (r, j) = run_opts(
        bmt_ecu(),
        // Named with "key" like the real BMT routine ("CLE"): exempt from the name guard
        // only as the exact owner-approved payload on the BMT.
        &[
            "exec",
            "--xml",
            &xml("bmt_cli.xml"),
            "Erase remotes and learn remote key 1",
        ],
        Opts {
            interactive: true,
            human: Human::TypesShownCode,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert!(r.screen.contains("erases all learned remote controls"));
    assert_eq!(j.writes().len(), 1);
    assert_eq!(j.writes()[0].payload, vec![0x31, 0xC7, 0, 0, 0]);
}

#[test]
fn bmt_transponder_routines_are_never_offered() {
    for req in ["31C0000000", "3BD20201", "2E0102", "21A401"] {
        let (r, j) = run_opts(
            bmt_ecu(),
            &["exec", "--xml", &xml("bmt_cli.xml"), req],
            Opts {
                interactive: true,
                human: Human::TypesShownCode,
                ..Opts::default()
            },
        );
        assert_eq!(r.code, 7, "{req}");
        assert!(r.screen.is_empty());
        j.assert_no_writes();
    }
}

#[test]
fn config_write_reads_identification_and_backup_first() {
    let (r, j) = run_opts(
        bmt_ecu(),
        &["--trace", "exec", "--xml", &xml("bmt_cli.xml"), "3BD20501"],
        Opts {
            interactive: true,
            human: Human::TypesShownCode,
            ..Opts::default()
        },
    );
    // The fixture BMT has no 3B D2 definition: the sim answers 7F 3B 12 (exit 6), but only
    // after 21 80 and the 21 A1 backup, and the write reached the ECU once.
    assert_eq!(r.code, 6, "{}\n{}", r.stdout, r.stderr);
    let tx: Vec<&str> = r.stderr.lines().filter(|l| l.contains("] TX 8")).collect();
    let pos = |needle: &str| tx.iter().position(|l| l.contains(needle)).unwrap();
    assert!(
        pos("21 80") < pos("21 A1") && pos("21 A1") < pos("3B D2 05 01"),
        "{tx:#?}"
    );
    assert!(r.screen.contains("config write (tier 2)"));
    assert_eq!(j.writes().len(), 1);
}

#[test]
fn human_output_matches_the_quickstart() {
    let (r, _) = run(
        sirius_ecu(),
        &["read", "--xml", &xml("sirius_cli.xml"), "Live A9"],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    for needle in [
        "init single byte 7A: ok",
        "connected: sirius_cli at 7A (kwp, kwp 5-baud) with variant single; sync 55 KW D0 8F, ~address 85, key bytes match the definition",
        "session 10 A0: ok 50A0",
        "21 A9 -> 61 A9 ",
        "  [Live A9]",
        "Engine speed     850 rpm",
    ] {
        assert!(r.stdout.contains(needle), "missing {needle:?} in\n{}", r.stdout);
    }
    assert!(r.stderr.contains("capture: "), "{}", r.stderr);
}

#[test]
fn dtc_uses_failure_flags_and_reports_the_clear_plan() {
    let (r, j) = run(
        sirius_ecu(),
        &["--json", "dtc", "--xml", &xml("sirius_cli.xml")],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let d = &r.event("dtc")[0];
    assert_eq!(d["active"][0]["devices"][0], "Injector 1");
    let s = &r.event("dtc_summary")[0];
    assert!(
        s["method"].as_str().unwrap().starts_with("Failures Id=1"),
        "{s}"
    );
    assert_eq!(s["clear_plan"]["ours"]["payload"], "140000");
    assert_eq!(s["clear_plan"]["ddt2000"]["payload"], "14FF00");
    assert_eq!(s["clear_plan"]["differs"], true);
    j.assert_no_writes();
}

#[test]
fn clear_dtc_sends_the_definition_request_only_after_approval() {
    let (r, j) = run_opts(
        sirius_ecu(),
        &["clear-dtc", "--xml", &xml("sirius_cli.xml")],
        Opts {
            interactive: true,
            human: Human::TypesShownCode,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert!(r.stdout.contains("will send: 14 00 00"), "{}", r.stdout);
    assert!(
        r.stdout
            .contains("DDT2000's global clear would send 14 FF 00"),
        "{}",
        r.stdout
    );
    assert!(
        r.screen.contains("APPROVAL REQUIRED: service (tier 1)"),
        "{}",
        r.screen
    );
    let w = j.writes();
    assert_eq!(w.len(), 1, "{w:?}");
    assert_eq!(w[0].payload, vec![0x14, 0x00, 0x00]);
}

#[test]
fn clear_dtc_ddt2000_mode_and_refusals() {
    let (r, j) = run_opts(
        sirius_ecu(),
        &[
            "clear-dtc",
            "--ddt2000-clear",
            "--xml",
            &xml("sirius_cli.xml"),
        ],
        Opts {
            interactive: true,
            human: Human::TypesShownCode,
            ..Opts::default()
        },
    );
    // The simulated ECU only knows its definition's 14 00 00 and answers 7F 14 12 (exit 6).
    assert_eq!(r.code, 6, "{}\n{}", r.stdout, r.stderr);
    assert_eq!(j.writes()[0].payload, vec![0x14, 0xFF, 0x00]);
    // Never with --json, never without a TTY, nothing sent.
    let (r, j) = run(
        sirius_ecu(),
        &["--json", "clear-dtc", "--xml", &xml("sirius_cli.xml")],
    );
    assert_ne!(r.code, 0);
    j.assert_no_writes();
    let (r, j) = run(
        sirius_ecu(),
        &["clear-dtc", "--xml", &xml("sirius_cli.xml")],
    );
    assert_ne!(r.code, 0);
    j.assert_no_writes();
    let (r, j) = run_opts(
        sirius_ecu(),
        &["clear-dtc", "--xml", &xml("sirius_cli.xml")],
        Opts {
            interactive: true,
            human: Human::TypesWrongCode,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, 9, "{}", r.stdout);
    j.assert_no_writes();
}

#[test]
fn ident_retries_busy_nrc_like_ddt2000() {
    // 7F 21 21 (busy) twice, then the real reply: DDT2000 RequestByteWithRetry waits 200 ms
    // and sends again.
    let ecu = sirius_ecu()
        .with_fault(FaultRule::on(&[0x21, 0x80], ReplyFault::Negative { nrc: 0x21 }).once())
        .with_fault(FaultRule::on(&[0x21, 0x80], ReplyFault::Negative { nrc: 0x21 }).once());
    let (r, j) = run(ecu, &["--json", "ident", "--xml", &xml("sirius_cli.xml")]);
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let id = &r.event("ident")[0];
    assert_eq!(id["tries"], 3);
    j.assert_no_writes();
    // --nrc-tries 1: no retry, the negative reply is the result (exit 6).
    let ecu = sirius_ecu()
        .with_fault(FaultRule::on(&[0x21, 0x80], ReplyFault::Negative { nrc: 0x21 }).once());
    let (r, _) = run(
        ecu,
        &[
            "--json",
            "ident",
            "--xml",
            &xml("sirius_cli.xml"),
            "--nrc-tries",
            "1",
        ],
    );
    assert_eq!(r.code, 6, "{}", r.stdout);
}

#[test]
fn live_retries_a_cycle_only_when_every_request_failed() {
    // One of two frames always silent: the cycle still counts as good (DDT2000 retries only
    // when every request of the cycle failed).
    let ecu = sirius_ecu().with_fault(FaultRule::on(&[0x21, 0xAA], ReplyFault::NoReply));
    let (r, _) = run(
        ecu,
        &[
            "--json",
            "live",
            "--xml",
            &xml("sirius_cli.xml"),
            "--frames",
            "21A9,21AA",
            "--count",
            "5",
            "--rate",
            "10",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert!(r.event("live_retry").is_empty());
    // Both silent: 3 retries 400 ms apart, then exit 5.
    let ecu = sirius_ecu()
        .with_fault(FaultRule::on(&[0x21, 0xA9], ReplyFault::NoReply))
        .with_fault(FaultRule::on(&[0x21, 0xAA], ReplyFault::NoReply));
    let (r, _) = run(
        ecu,
        &[
            "--json",
            "live",
            "--xml",
            &xml("sirius_cli.xml"),
            "--frames",
            "21A9,21AA",
            "--rate",
            "10",
        ],
    );
    assert_eq!(r.code, 5, "{}\n{}", r.stdout, r.stderr);
    let retries = r.event("live_retry");
    assert_eq!(retries.len(), 3);
    assert_eq!(retries[0]["wait_ms"], 400);
}

#[test]
fn periodic_tester_present_option_sends_tp_every_2000_ms() {
    let (r, j) = run(
        sirius_ecu(),
        &[
            "--trace",
            "live",
            "--xml",
            &xml("sirius_cli.xml"),
            "--frames",
            "21A9",
            "--count",
            "6",
            "--rate",
            "1",
            "--periodic-tester-present",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    // 6 cycles at 1 Hz with a request every second: idle TP (3000 ms) would never fire.
    let tps = r.stderr.matches("TX 81 7A F1 3E 2A").count();
    assert!(tps >= 2, "{tps} TP\n{}", r.stderr);
    j.assert_no_writes();
}

fn autoident_ecu(version: &str) -> SpecEcu {
    let sc = sirius_scenario()
        .set("Diag version", kline_spec::Value::Hex("0C".into()))
        .text("Supplier", "001")
        .set("Program", kline_spec::Value::Hex("00E1".into()))
        .set("Soft version", kline_spec::Value::Hex(version.into()));
    let sc = sc
        .iter()
        .filter(|(n, _)| !["Calibration", "Supplier code"].contains(&n.as_str()))
        .fold(kline_sim::scenario::Scenario::new(), |s, (n, v)| {
            s.set(n, v.clone())
        });
    SpecEcu::new(EcuProfile::sirius32(), fixture("sirius_autoident.xml"), sc).unwrap()
}

#[test]
fn ident_uses_the_ddt2000_autoident_ladder() {
    let (r, j) = run(
        autoident_ecu("9200"),
        &["--json", "ident", "--xml", &xml("sirius_autoident.xml")],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let id = &r.event("ident")[0];
    assert_eq!(id["autoident"]["match"], "exact");
    assert_eq!(id["autoident"]["ident"]["diag_version"], 12);
    assert_eq!(id["autoident"]["ident"]["version"], "9200");
    assert_eq!(id["method"], "DDT2000 AutoIdent: exact");
    // 24-byte reply (MinBytes 24): fine for us, DDT2000's DiagFailures wants exactly 26.
    assert_eq!(id["reply_len"], 24);
    assert_eq!(id["ddt2000_diagfailures_accepts"], false);
    j.assert_no_writes();
    // Another software version: the only definition is still taken (MatchUnique).
    let (r, _) = run(
        autoident_ecu("9300"),
        &["--json", "ident", "--xml", &xml("sirius_autoident.xml")],
    );
    assert_eq!(r.code, 0, "{}", r.stdout);
    assert_eq!(r.event("ident")[0]["autoident"]["match"], "unique");
}

#[test]
fn selftest_toggles_break_only_and_reports_timing() {
    let ecu = sirius_ecu();
    let j = ecu.journal();
    let r = run_line_port(
        VirtualLine::with_ecu(ecu),
        "virtual",
        &["--json", "selftest"],
        Opts {
            ports: vec![ftdi_port("virtual")],
            ..Opts::default()
        },
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let s = &r.event("selftest")[0];
    assert_eq!(s["baud_requested"], 10400);
    assert_eq!(s["break_jitter"]["edges"], 40);
    assert_eq!(s["break_jitter"]["five_baud_ok"], true, "{s}");
    assert!(s["note"].as_str().unwrap().contains("latency"));
    assert!(s["baud_reported_source"].is_string(), "{s}");
    assert!(s["echo_rtt"].is_null(), "no echo test unless asked: {s}");
    // A cause is given exactly when TiniL fails.
    assert_eq!(
        s["fast_init_cause"].is_string(),
        s["break_jitter"]["fast_init_ok"] == false,
        "{s}"
    );
    // No ECU was woken and nothing was written.
    j.assert_no_writes();
    assert!(r.event("connected").is_empty());
    // A macOS callin device is refused before opening.
    let (r, _) = run_with_port(sirius_ecu(), "/dev/tty.usbserial-X", &["selftest"]);
    assert_eq!(r.code, 3, "{}", r.stderr);
    assert!(r.stderr.contains("/dev/cu.usbserial-X"), "{}", r.stderr);
    assert!(!r.opened);
}

/// Re-review R3: `selftest --rts low` drops RTS once (the bench comparison `--rts low` vs
/// `leave` depends on it) and traces the same `PORT dtr=1 rts=0` line as an init; the default
/// leaves RTS alone.
#[test]
fn selftest_rts_low_drops_rts_once_and_traces_the_port_line() {
    for (extra, rts_calls, line) in [
        (&["--rts", "low"][..], 1, "PORT dtr=1 rts=0"),
        (&[][..], 0, "PORT dtr=1 rts=left as opened"),
    ] {
        let ecu = sirius_ecu();
        let j = ecu.journal();
        let mut args = extra.to_vec();
        args.extend(["--trace", "selftest"]);
        let r = run_line_port(
            VirtualLine::with_ecu(ecu),
            "virtual",
            &args,
            Opts {
                ports: vec![ftdi_port("virtual")],
                ..Opts::default()
            },
        );
        assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
        assert!(r.stderr.contains(line), "{}", r.stderr);
        let notes: Vec<String> = j.notes().into_iter().map(|(_, n)| n).collect();
        assert_eq!(
            notes.iter().filter(|n| n.starts_with("RTS")).count(),
            rts_calls,
            "{notes:?}"
        );
        if rts_calls == 1 {
            assert!(notes.iter().any(|n| n == "RTS low"), "{notes:?}");
        }
    }
}

#[test]
fn selftest_reports_host_sleep_jitter_before_opening_the_port() {
    let (r, _) = run(sirius_ecu(), &["--json", "selftest"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let t = &r.event("selftest_timing")[0];
    let js = t["sleep_jitter"].as_array().unwrap();
    let targets: Vec<u64> = js
        .iter()
        .map(|j| j["target_ms"].as_u64().unwrap())
        .collect();
    assert_eq!(targets, [1, 25, 200], "{t}");
    assert_eq!(t["ok"], true, "{t}");
    assert!(js
        .iter()
        .all(|j| j["samples"] == 10 && j["max_late_us"] == 0));
    // Timing comes first (it needs no cable), then the port.
    let names: Vec<String> = r
        .events()
        .iter()
        .map(|e| e["event"].as_str().unwrap_or("").to_string())
        .collect();
    let (a, b) = (
        names.iter().position(|n| n == "selftest_timing").unwrap(),
        names.iter().position(|n| n == "selftest").unwrap(),
    );
    assert!(a < b, "{names:?}");

    // A host whose sleeps wake 1.5 ms late cannot hold TiniL (25 +- 1 ms).
    let r = run_line_port(
        VirtualLine::with_ecu(sirius_ecu()),
        "virtual",
        &["--json", "selftest"],
        Opts {
            clock_late_us: 1_500,
            ..Opts::default()
        },
    );
    let t = &r.event("selftest_timing")[0];
    assert_eq!(t["ok"], false, "{t}");
    assert_eq!(t["sleep_jitter"][1]["ok"], false, "{t}");
    assert_eq!(
        t["sleep_jitter"][2]["ok"], true,
        "200 ms +-10 ms still fine: {t}"
    );
    // Human output names the verdict.
    let r = run_line_port(
        VirtualLine::with_ecu(sirius_ecu()),
        "virtual",
        &["selftest"],
        Opts {
            clock_late_us: 1_500,
            ..Opts::default()
        },
    );
    assert!(
        r.stdout.contains("sleep  25 ms x10") && r.stdout.contains("TOO IMPRECISE"),
        "{}",
        r.stdout
    );
}

#[cfg(target_os = "linux")]
#[test]
fn selftest_reads_and_sets_the_linux_ftdi_latency_timer() {
    let root = temp_dir("sysfs");
    let dev = root.join("bus/usb-serial/devices/ttyUSB7");
    std::fs::create_dir_all(&dev).unwrap();
    std::fs::write(dev.join("latency_timer"), "16\n").unwrap();
    let o = || Opts {
        sys_root: Some(root.clone()),
        ..Opts::default()
    };
    let r = run_line_port(
        VirtualLine::with_ecu(sirius_ecu()),
        "/dev/ttyUSB7",
        &["--json", "selftest"],
        o(),
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.event("selftest")[0]["latency_timer"]["ms"], 16);
    let r = run_line_port(
        VirtualLine::with_ecu(sirius_ecu()),
        "/dev/ttyUSB7",
        &["--json", "selftest", "--set-latency", "1"],
        o(),
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert_eq!(r.event("selftest")[0]["latency_timer"]["ms"], 1);
    assert_eq!(
        std::fs::read_to_string(dev.join("latency_timer")).unwrap(),
        "1\n"
    );
    // Out of range is a usage error; a non-ttyUSB port cannot be set.
    let (r, _) = run(sirius_ecu(), &["selftest", "--set-latency", "0"]);
    assert_eq!(r.code, 2, "{}", r.stderr);
    let (r, _) = run(sirius_ecu(), &["selftest", "--set-latency", "1"]);
    assert_eq!(r.code, 3, "{}", r.stderr);
    assert!(r.stderr.contains("not a ttyUSB"), "{}", r.stderr);
}

#[cfg(not(target_os = "linux"))]
#[test]
fn selftest_set_latency_is_linux_only() {
    let (r, _) = run(sirius_ecu(), &["selftest", "--set-latency", "1"]);
    assert_eq!(r.code, 2, "{}", r.stderr);
    assert!(r.stderr.contains("Linux-only"), "{}", r.stderr);
}

#[test]
fn refused_session_is_reported_and_no_other_session_is_tried() {
    let ecu = sirius_ecu().with_fault(FaultRule::on(
        &[0x10, 0xA0],
        ReplyFault::Negative { nrc: 0x12 },
    ));
    let (r, j) = run(
        ecu,
        &[
            "--json",
            "--trace",
            "connect",
            "--xml",
            &xml("sirius_cli.xml"),
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let s = &r.event("session_start")[0];
    assert_eq!(s["ok"], false);
    assert!(s["reply"].as_str().unwrap().contains("7F 10 12"), "{s}");
    assert_eq!(s["note"], "not retried with another session");
    // The only StartDiagnosticSession on the wire is 10 A0.
    assert_eq!(r.event("session_start").len(), 1);
    assert!(r.stderr.contains("TX 82 7A F1 10 A0"), "{}", r.stderr);
    assert!(!r.stderr.contains("TX 82 7A F1 10 81"), "{}", r.stderr);
    assert!(!r.stderr.contains("TX 82 7A F1 10 C0"), "{}", r.stderr);
    j.assert_no_writes();
}

/// Security re-check finding 6: definition names are printed sanitized in `risk` and on the
/// approval screen (a crafted name with a newline and an 8-bit CSI cannot fake prompt lines).
#[test]
fn crafted_definition_names_are_sanitized_in_security_prompts() {
    let src = std::fs::read_to_string(fixture_path("sirius_cli.xml")).unwrap();
    let crafted = src.replace(
        r#"<Request Name="Reset learning">"#,
        r#"<Request Name="Reset learning&#10;tier:              read (tier 0)&#x9B;2J">"#,
    );
    assert_ne!(src, crafted);
    let dir = temp_dir("crafted");
    let path = dir.join("crafted.xml");
    std::fs::write(&path, crafted).unwrap();
    let x = path_str(&path);
    let (r, _) = run(sirius_ecu(), &["risk", "--xml", &x, "11FF"]);
    assert_eq!(r.code, 0, "{}{}", r.stdout, r.stderr);
    assert!(
        r.stdout.contains(r"Reset learning\u{a}tier:"),
        "{}",
        r.stdout
    );
    assert!(
        !r.stdout.contains("\ntier:              read"),
        "{}",
        r.stdout
    );
    assert!(!r.stdout.contains('\u{9b}'), "{}", r.stdout);
    let (r, j) = run_opts(
        sirius_ecu(),
        &["exec", "--xml", &x, "11FF"],
        Opts {
            interactive: true,
            human: Human::TypesWrongCode,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, 9, "{}", r.stderr);
    assert!(
        r.screen.contains(r"Reset learning\u{a}tier:"),
        "{}",
        r.screen
    );
    assert!(
        !r.screen.contains("\ntier:              read"),
        "{}",
        r.screen
    );
    assert!(!r.screen.chars().any(|c| c == '\u{9b}' || c == '\u{1b}'));
    j.assert_no_writes();
}

#[test]
fn missing_break_loopback_warns_and_reads_unless_strict() {
    // Hydrogen final review (macOS IGNBRK): no RX byte during the start bit must not abort
    // the init by default; --strict-loopback keeps DDT2000's abort.
    let (r, _) = run_blind_break(
        sirius_ecu(),
        &["--json", "read", "--xml", &xml("sirius_cli.xml"), "21A9"],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let w = r.event("warning");
    assert_eq!(w.len(), 1, "{}", r.stdout);
    assert_eq!(w[0]["kind"], "break_loopback_not_seen");
    assert_eq!(r.event("read").len(), 1);

    let (r, j) = run_blind_break(
        sirius_ecu(),
        &[
            "--json",
            "read",
            "--xml",
            &xml("sirius_cli.xml"),
            "--strict-loopback",
            "21A9",
        ],
    );
    assert_eq!(r.code, kline_host::error::exit::INIT, "{}", r.stdout);
    assert!(r.stdout.contains("inactive"), "{}", r.stdout);
    j.assert_no_writes();
}

#[test]
fn selftest_echo_rtt_times_the_loopback() {
    let (r, _) = run(sirius_ecu(), &["--json", "selftest", "--echo-rtt"]);
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let e = &r.event("selftest")[0]["echo_rtt"];
    assert_eq!(e["sent"], 10, "{e}");
    assert_eq!(e["echoed"], 10, "{e}");
    assert!(e["max_us"].as_u64().unwrap() < 25_000, "{e}");
    assert!(e["write_error"].is_null(), "{e}");
    assert!(r.event("connected").is_empty());
}

#[test]
fn set_builds_the_request_from_the_definition() {
    // Hydrogen final review: actuator/config requests built from <Sent> data items, not hex.
    let abs = xml("abs_fast.xml");
    let (r, _) = run(
        abs_ecu(),
        &[
            "--json",
            "risk",
            "--xml",
            &abs,
            "Pump test",
            "--set",
            "Pump=0",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let e = &r.event("risk")[0];
    assert_eq!(e["request"], "30A000");
    assert_eq!(e["names"], json!(["Pump test"]));
    assert_eq!(e["risk"]["tier_number"], 1);
    // Unknown item, off-grid / out-of-range value, --set on hex: refused before any I/O.
    for (args, code) in [
        (vec!["Pump test", "--set", "Speed=1"], 8),
        (vec!["Pump test", "--set", "Pump=1.5"], 8),
        (vec!["Pump test", "--set", "Pump=999"], 8),
        (vec!["30A001", "--set", "Pump=0"], 2),
        (vec!["Pump test", "--set", "Pump"], 2),
    ] {
        let mut a = vec!["--json", "risk", "--xml", &abs];
        a.extend(args.iter().copied());
        let (r, _) = run(abs_ecu(), &a);
        assert_eq!(r.code, code, "{args:?}: {}", r.stdout);
        assert!(!r.opened);
    }
    // The name guard still sees a built request: "Code antidemarrage" stays refused.
    let (r, _) = run(
        abs_ecu(),
        &[
            "--json",
            "risk",
            "--xml",
            &abs,
            "Ecriture code",
            "--set",
            "Code antidemarrage=hex:1234",
        ],
    );
    assert_eq!(r.code, 7, "{}", r.stdout);
    assert_eq!(r.event("risk")[0]["request"], "3B801234");
    assert!(r.event("risk")[0]["refused"]
        .as_str()
        .unwrap()
        .contains("immobilizer"));
}

#[test]
fn exec_stop_request_gets_its_own_approval_and_is_sent_after() {
    let abs = xml("abs_fast.xml");
    let (r, j) = run_opts(
        abs_ecu(),
        &[
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
        ],
        Opts {
            interactive: true,
            human: Human::TypesShownCode,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, 0, "{}\n{}\n{}", r.stdout, r.stderr, r.screen);
    assert_eq!(
        r.screen.matches("APPROVAL REQUIRED").count(),
        2,
        "{}",
        r.screen
    );
    assert!(r.screen.contains("to send the stop request now"));
    let sent: Vec<Vec<u8>> = j.writes().into_iter().map(|w| w.payload).collect();
    assert_eq!(sent, vec![vec![0x30, 0xA0, 0x01], vec![0x30, 0xA0, 0x00]]);
    assert!(r.stdout.contains("stop 30 A0 00"), "{}", r.stdout);

    // A refused stop request (immobilizer-named) refuses the whole test before any I/O.
    let (r, _) = run_opts(
        abs_ecu(),
        &[
            "exec",
            "--xml",
            &abs,
            "Pump test",
            "--stop",
            "Ecriture code",
        ],
        Opts {
            interactive: true,
            human: Human::TypesShownCode,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, 7, "a refused stop refuses the test: {}", r.stdout);
    assert!(r.screen.is_empty() && !r.opened);
}

#[test]
fn dtc_on_iso8_without_fault_memory_reports_fault_named_items() {
    // Hydrogen final review: Safir (ISO8, Failures Id=0) made `dtc` exit 8. It now says why
    // and decodes the fault-named items of its Tier 0 reads.
    let x = xml("iso8_nofaults.xml");
    let (r, j) = run(iso8_nofaults_ecu(), &["--json", "dtc", "--xml", &x]);
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let note = &r.event("dtc_note")[0];
    assert!(
        note["method"].as_str().unwrap().contains("Failures Id=0"),
        "{note}"
    );
    let d = &r.event("dtc")[0];
    assert_eq!(d["request"], "3351");
    // Engine speed is non-zero but not a fault; sensor B is 0.
    let names: Vec<&str> = d["active"]
        .as_array()
        .unwrap()
        .iter()
        .map(|a| a["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["Fault present sensor A"], "{d}");
    assert_eq!(r.event("dtc_summary")[0]["active"], 1);
    j.assert_no_writes();
}

#[test]
fn human_output_is_saved_next_to_the_capture_and_ends_with_done() {
    let (r, _) = run(
        sirius_ecu(),
        &["read", "--xml", &xml("sirius_cli.xml"), "Live A9"],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    let cap = &r.captures()[0];
    let out = cap.with_extension("out.txt");
    let text = std::fs::read_to_string(&out).unwrap_or_else(|e| panic!("{}: {e}", out.display()));
    for needle in [
        "connected: sirius_cli at 7A",
        "21 A9 -> 61 A9 ",
        "Engine speed     850 rpm",
        "capture: ",
        "done (exit 0)",
    ] {
        assert!(text.contains(needle), "missing {needle:?} in\n{text}");
    }
    assert_eq!(
        r.stderr.lines().last(),
        Some("done (exit 0)"),
        "{}",
        r.stderr
    );
    // An error is saved too, with its exit code.
    let ecu = sirius_ecu().with_fault(FaultRule::on(
        &[0x21, 0xA9],
        ReplyFault::Negative { nrc: 0x12 },
    ));
    let (r, _) = run(ecu, &["read", "--xml", &xml("sirius_cli.xml"), "21A9"]);
    assert_eq!(r.code, 6);
    let text = std::fs::read_to_string(r.captures()[0].with_extension("out.txt")).unwrap();
    assert!(text.contains("ECU answered negatively: 7F 21 12"), "{text}");
    assert!(text.trim_end().ends_with("done (exit 6)"), "{text}");
    // --json: no done line (stdout stays JSON, stderr has no extra line), the capture event
    // names the file.
    let (r, _) = run(
        sirius_ecu(),
        &["--json", "read", "--xml", &xml("sirius_cli.xml"), "21A9"],
    );
    assert_eq!(r.code, 0);
    assert!(!r.stderr.contains("done (exit"), "{}", r.stderr);
    let c = &r.event("capture")[0];
    assert!(c["out"].as_str().unwrap().ends_with(".out.txt"), "{c}");
    let text = std::fs::read_to_string(c["out"].as_str().unwrap()).unwrap();
    assert!(text.trim_end().ends_with("done (exit 0)"), "{text}");
    // Code review L4: with --json the file also has the error and the done line, while
    // stdout stays one JSON object per line and stderr gets nothing extra.
    let ecu = sirius_ecu().with_fault(FaultRule::on(
        &[0x21, 0xA9],
        ReplyFault::Negative { nrc: 0x12 },
    ));
    let (r, _) = run(
        ecu,
        &["--json", "read", "--xml", &xml("sirius_cli.xml"), "21A9"],
    );
    assert_eq!(r.code, 6);
    let text = std::fs::read_to_string(r.captures()[0].with_extension("out.txt")).unwrap();
    assert!(
        text.contains("error: ECU answered negatively: 7F 21 12"),
        "{text}"
    );
    assert!(text.trim_end().ends_with("done (exit 6)"), "{text}");
    assert!(!r.stderr.contains("done (exit"), "{}", r.stderr);
    assert_eq!(r.event("error").len(), 1, "{}", r.stdout);
    for l in r.stdout.lines() {
        serde_json::from_str::<serde_json::Value>(l).unwrap_or_else(|e| panic!("{l}: {e}"));
    }
}

/// Step 7b FIX-1: `--rts low` drops RTS after open (DTR high first), traced as
/// `PORT dtr=1 rts=0`; the default leaves RTS as the OS opened the port.
#[test]
fn rts_option_drops_rts_after_open_only_when_asked() {
    let sx = xml("sirius_cli.xml");
    let (r, j) = run(
        sirius_ecu(),
        &["--rts", "low", "--trace", "connect", "--xml", &sx],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert!(r.stderr.contains("PORT dtr=1 rts=0"), "{}", r.stderr);
    let notes: Vec<String> = j.notes().into_iter().map(|(_, n)| n).collect();
    assert_eq!(
        notes.iter().filter(|n| n.starts_with("RTS")).count(),
        1,
        "{notes:?}"
    );
    assert!(notes.iter().any(|n| n == "RTS low"), "{notes:?}");
    let (r, j) = run(sirius_ecu(), &["--trace", "connect", "--xml", &sx]);
    assert_eq!(r.code, 0);
    assert!(
        r.stderr.contains("PORT dtr=1 rts=left as opened"),
        "{}",
        r.stderr
    );
    assert!(!j.notes().iter().any(|(_, n)| n.starts_with("RTS")));
    let (r, _) = run(sirius_ecu(), &["--rts", "high", "connect", "--xml", &sx]);
    assert_eq!(r.code, 2, "only leave|low: {}", r.stderr);
}

#[test]
fn an_unanswered_fast_init_before_a_working_5_baud_reads_as_information() {
    let (r, _) = run(
        sirius_with(AddressAccept::Plain, false),
        &["--trace", "connect", "--xml", &xml("sirius_cli.xml")],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert!(
        r.stdout.contains(
            "init fast   fast init not answered (normal for many Renault K-line ECUs), trying 5-baud byte 7A"
        ),
        "{}",
        r.stdout
    );
    assert!(r.stdout.contains("init single byte 7A: ok"), "{}", r.stdout);
    assert!(!r.stdout.contains("no reply from ECU"), "{}", r.stdout);
    // Full detail in the trace.
    assert!(
        r.stderr
            .contains("INIT attempt fast (round 1) failed: no reply from ECU"),
        "{}",
        r.stderr
    );
    // A failing 5-baud before multi names the next attempt.
    let (r, _) = run(
        sirius_with(AddressAccept::Xor80, false),
        &["connect", "--xml", &xml("sirius_cli.xml")],
    );
    assert_eq!(r.code, 0);
    assert!(
        r.stdout.contains(
            "init single byte 7A: no keywords received (got nothing), trying 5-baud byte FA"
        ),
        "{}",
        r.stdout
    );
    // JSON keeps the raw error.
    let (r, _) = run(
        sirius_with(AddressAccept::Plain, false),
        &["--json", "connect", "--xml", &xml("sirius_cli.xml")],
    );
    assert_eq!(r.event("init_attempt")[0]["error"], "no reply from ECU");
}

#[test]
fn missing_definitions_say_how_to_fix_it() {
    let (r, _) = run(
        sirius_ecu(),
        &[
            "--defs",
            "/nonexistent-kline-defs",
            "connect",
            "--ecu",
            "bmt",
        ],
    );
    assert_eq!(r.code, 8, "{}", r.stderr);
    assert!(!r.opened);
    for needle in [
        "BMT_X65_ph1_p.xml",
        "/nonexistent-kline-defs does not exist",
        "kline setup --defs-from DIR",
        "--defs DIR",
        "KLINE_DDT_XML",
        ".local/share/kline/ecus",
    ] {
        assert!(
            r.stderr.contains(needle),
            "missing {needle:?} in\n{}",
            r.stderr
        );
    }
}

#[test]
fn setup_copies_the_definitions_and_then_ecu_works() {
    let src = temp_dir("setup-src");
    let dest = temp_dir("setup-dest").join("ecus");
    std::fs::copy(
        fixture_path("sirius_cli.xml"),
        src.join("SIRIUS32___EMS3132___Soft_RE1_maj.xml"),
    )
    .unwrap();
    std::fs::write(src.join("notes.txt"), "not a definition").unwrap();
    let d = path_str(&dest);
    let (r, j) = run(
        sirius_ecu(),
        &[
            "--json",
            "--defs",
            &d,
            "setup",
            "--defs-from",
            &path_str(&src),
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert!(!r.opened, "setup opens no port");
    let e = &r.event("setup")[0];
    assert_eq!(
        e["copied"],
        json!(["SIRIUS32___EMS3132___Soft_RE1_maj.xml"])
    );
    assert_eq!(e["ecus"]["sirius"], true);
    assert_eq!(e["ecus"]["bmt"], false);
    assert!(!dest.join("notes.txt").exists());
    j.assert_no_writes();
    let (r, _) = run(
        sirius_ecu(),
        &["--json", "--defs", &d, "connect", "--ecu", "sirius"],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    // Never into a git work tree (SAFETY.md rule 1).
    let repo = temp_dir("setup-git");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    let inside = repo.join("ddt-xml");
    let (r, _) = run(
        sirius_ecu(),
        &[
            "--defs",
            &path_str(&inside),
            "setup",
            "--defs-from",
            &path_str(&src),
        ],
    );
    assert_eq!(r.code, 8, "{}", r.stderr);
    assert!(r.stderr.contains("git work tree"), "{}", r.stderr);
    assert!(!inside
        .join("SIRIUS32___EMS3132___Soft_RE1_maj.xml")
        .exists());
    // A source without XML files.
    let empty = temp_dir("setup-empty");
    let (r, _) = run(
        sirius_ecu(),
        &["--defs", &d, "setup", "--defs-from", &path_str(&empty)],
    );
    assert_eq!(r.code, 8, "{}", r.stderr);
    assert!(r.stderr.contains("no .xml files"), "{}", r.stderr);
}

/// A definitions directory with the BMT fixture under both real BMT file names.
fn two_bmt_defs() -> String {
    let d = temp_dir("bmt-defs");
    for name in ["BMT_X65_ph1_p.xml", "BMT_SAGEM.xml"] {
        std::fs::copy(fixture_path("bmt_cli.xml"), d.join(name)).unwrap();
    }
    path_str(&d)
}

#[test]
fn bmt_definition_shortcuts_resolve_an_ambiguous_config_write() {
    let defs = two_bmt_defs();
    let exec = |ecu: &str| {
        run_opts(
            bmt_ecu(),
            &["--defs", &defs, "exec", "--ecu", ecu, "3BD20501"],
            Opts {
                interactive: true,
                human: Human::TypesShownCode,
                ..Opts::default()
            },
        )
    };
    // Both definitions decode 21 80: refused before the approval, nothing written.
    let (r, j) = exec("bmt");
    assert_eq!(r.code, 7, "{}\n{}", r.stdout, r.stderr);
    for needle in ["--ecu bmt-ph1p", "--ecu bmt-sagem", "--xml"] {
        assert!(
            r.stderr.contains(needle),
            "missing {needle:?}: {}",
            r.stderr
        );
    }
    j.assert_no_writes();
    // One definition named: the write goes out (the fixture sim answers 7F 3B 12, exit 6).
    for ecu in ["bmt-ph1p", "bmt-sagem"] {
        let (r, j) = exec(ecu);
        assert_eq!(r.code, 6, "{ecu}: {}\n{}", r.stdout, r.stderr);
        assert_eq!(j.writes().len(), 1, "{ecu}");
    }
    let (r, _) = run(
        bmt_ecu(),
        &["--json", "--defs", &defs, "connect", "--ecu", "bmt-sagem"],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(
        r.event("connected")[0]["definition"]
            .as_str()
            .unwrap()
            .ends_with("BMT_SAGEM.xml"),
        "{}",
        r.stdout
    );
}

/// Re-review N2: with `--ecu bmt` unresolved, every non-read `3B D2 ..` is refused, also the
/// Expert ones (`3B D2 0E 01`, index 14), with the same message as the Tier 2 write.
#[test]
fn unresolved_bmt_refuses_expert_3b_d2_like_tier_2() {
    let defs = two_bmt_defs();
    let exec = |req: &str| {
        run_opts(
            bmt_ecu(),
            &["--defs", &defs, "exec", "--ecu", "bmt", req],
            Opts {
                interactive: true,
                human: Human::TypesShownCode,
                ..Opts::default()
            },
        )
    };
    let (t2, j) = exec("3BD20501");
    assert_eq!(t2.code, 7, "{}", t2.stderr);
    j.assert_no_writes();
    let (r, j) = exec("3BD20E01");
    assert_eq!(r.code, 7, "{}\n{}", r.stdout, r.stderr);
    assert!(r.stderr.contains("--ecu bmt-ph1p"), "{}", r.stderr);
    assert!(!r.stderr.contains("REFUSED:"), "refused by the choice, not the policy");
    j.assert_no_writes();
    let msg = |s: &str| s.lines().find(|l| l.contains("--ecu bmt-ph1p")).map(str::to_owned);
    assert_eq!(msg(&t2.stderr), msg(&r.stderr));
}

/// Code review M3: `--ecu bmt` leaves the definition choice unresolved even when only one BMT
/// file is present, so a config write is refused until the definition is named.
#[test]
fn ecu_bmt_with_one_definition_still_refuses_a_config_write() {
    let d = temp_dir("bmt-defs-one");
    std::fs::copy(fixture_path("bmt_cli.xml"), d.join("BMT_X65_ph1_p.xml")).unwrap();
    let defs = path_str(&d);
    let exec = |ecu: &str| {
        run_opts(
            bmt_ecu(),
            &["--defs", &defs, "exec", "--ecu", ecu, "3BD20501"],
            Opts {
                interactive: true,
                human: Human::TypesShownCode,
                ..Opts::default()
            },
        )
    };
    let (r, j) = exec("bmt");
    assert_eq!(r.code, 7, "{}\n{}", r.stdout, r.stderr);
    for needle in ["--ecu bmt-ph1p", "--ecu bmt-sagem", "--xml"] {
        assert!(
            r.stderr.contains(needle),
            "missing {needle:?}: {}",
            r.stderr
        );
    }
    assert!(r.screen.is_empty(), "no approval offered: {}", r.screen);
    j.assert_no_writes();
    // Named explicitly: offered and sent (the fixture sim answers 7F 3B 12, exit 6).
    let (r, j) = exec("bmt-ph1p");
    assert_eq!(r.code, 6, "{}\n{}", r.stdout, r.stderr);
    assert_eq!(j.writes().len(), 1);
    // A Tier 1 request is not affected by the unresolved choice.
    let (r, _) = run_opts(
        bmt_ecu(),
        &["--defs", &defs, "exec", "--ecu", "bmt", "14FF00"],
        Opts {
            interactive: true,
            human: Human::TypesShownCode,
            ..Opts::default()
        },
    );
    assert_ne!(r.code, 7, "{}\n{}", r.stdout, r.stderr);
}

/// A definitions directory with bmt_cli.xml as ph1_p and bmt_cli_alt.xml as SAGEM.
fn differing_bmt_defs() -> String {
    let d = temp_dir("bmt-defs-diff");
    std::fs::copy(fixture_path("bmt_cli.xml"), d.join("BMT_X65_ph1_p.xml")).unwrap();
    std::fs::copy(fixture_path("bmt_cli_alt.xml"), d.join("BMT_SAGEM.xml")).unwrap();
    path_str(&d)
}

#[test]
fn ambiguous_ident_explains_the_impact_and_names_the_differing_bits() {
    let defs = differing_bmt_defs();
    let (r, _) = run(bmt_ecu(), &["--defs", &defs, "ident", "--ecu", "bmt"]);
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    for needle in [
        "AMBIGUOUS",
        "fine for reads",
        "before any config write choose the definition with --ecu bmt-ph1p or --ecu bmt-sagem or --xml",
        "21 A1 byte 3 mask 0x01: Option A (BMT_X65_ph1_p) vs Option B (BMT_SAGEM)",
        "21 A1 byte 3 mask 0x02: - (BMT_X65_ph1_p) vs Option C (BMT_SAGEM)",
        // The fixture's 21 80 reply has 9 bytes.
        "note: 9 bytes; DDT2000's DiagFailures identification needs exactly 26 (only DDT2000's fault screen checks that; fine for reads and for kline)",
    ] {
        assert!(r.stdout.contains(needle), "missing {needle:?} in\n{}", r.stdout);
    }
    let (r, _) = run(
        bmt_ecu(),
        &["--json", "--defs", &defs, "ident", "--ecu", "bmt"],
    );
    let id = &r.event("ident")[0];
    assert_eq!(id["ambiguous"], true);
    let d = id["differences"]
        .as_array()
        .unwrap()
        .iter()
        .find(|d| d["place"] == "byte 3 mask 0x01")
        .unwrap();
    assert_eq!(d["request"], "21A1");
    assert_eq!(d["place"], "byte 3 mask 0x01");
    assert_eq!(d["names"]["BMT_SAGEM"], "Option B");
    // One definition named: no ambiguity note.
    let (r, _) = run(bmt_ecu(), &["--defs", &defs, "ident", "--ecu", "bmt-sagem"]);
    assert_eq!(r.code, 0);
    assert!(!r.stdout.contains("AMBIGUOUS"), "{}", r.stdout);
}

#[test]
fn read_and_live_with_an_unresolved_bmt_choice_show_both_decodings_where_they_differ() {
    let defs = differing_bmt_defs();
    let (r, j) = run(
        bmt_ecu(),
        &["--defs", &defs, "read", "--ecu", "bmt", "21A1"],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    for needle in [
        "definition choice unresolved: BMT_X65_ph1_p, BMT_SAGEM; values below decoded with BMT_X65_ph1_p",
        "Option A  1",
        "decoded differently by the candidates:",
        "    BMT_X65_ph1_p: Option A 1",
        "    BMT_SAGEM: Option B 1; Option C 0",
    ] {
        assert!(r.stdout.contains(needle), "missing {needle:?} in\n{}", r.stdout);
    }
    j.assert_no_writes();
    // Same layout in both: no second decoding.
    let (r, _) = run(
        bmt_ecu(),
        &["--defs", &defs, "read", "--ecu", "bmt", "21A3"],
    );
    assert_eq!(r.code, 0);
    assert!(
        r.stdout.contains("definition choice unresolved"),
        "{}",
        r.stdout
    );
    assert!(!r.stdout.contains("decoded differently"), "{}", r.stdout);
    // live, and JSON.
    let (r, _) = run(
        bmt_ecu(),
        &[
            "--json", "--defs", &defs, "live", "--ecu", "bmt", "--frames", "21A1", "--count", "1",
        ],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    let c = &r.event("definition_choice")[0];
    assert_eq!(c["unresolved"], true);
    assert_eq!(c["decoded_with"], "BMT_X65_ph1_p");
    let alt = &r.event("live")[0]["alternatives"][0];
    assert_eq!(alt["definition"], "BMT_SAGEM");
    assert_eq!(alt["differing"][0]["name"], "Option B");
    // Named definition: nothing about a choice.
    let (r, _) = run(
        bmt_ecu(),
        &["--defs", &defs, "read", "--ecu", "bmt-ph1p", "21A1"],
    );
    assert_eq!(r.code, 0);
    assert!(!r.stdout.contains("unresolved"), "{}", r.stdout);
}

#[test]
fn live_says_when_the_ecu_rejects_every_request() {
    let ecu = sirius_ecu()
        .with_fault(FaultRule::on(
            &[0x21, 0xA9],
            ReplyFault::Negative { nrc: 0x12 },
        ))
        .with_fault(FaultRule::on(
            &[0x21, 0xAA],
            ReplyFault::Negative { nrc: 0x12 },
        ));
    let (r, _) = run(
        ecu,
        &[
            "live",
            "--xml",
            &xml("sirius_cli.xml"),
            "--frames",
            "21A9,21AA",
            "--rate",
            "10",
        ],
    );
    assert_eq!(r.code, 5, "{}\n{}", r.stdout, r.stderr);
    assert!(
        r.stderr.contains(
            "ECU rejected every request (7F 21 12 subFunctionNotSupported-invalidFormat) in 4 cycles"
        ),
        "{}",
        r.stderr
    );
    assert!(!r.stderr.contains("no communication"), "{}", r.stderr);
}

#[test]
fn ctrl_c_in_live_is_reported_with_the_stop() {
    let r = run_line_port(
        VirtualLine::with_ecu(sirius_ecu()),
        "virtual",
        &["live", "--xml", &xml("sirius_cli.xml"), "--frames", "21A9"],
        Opts {
            ctrl_c: true,
            ..Opts::default()
        },
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(
        r.stdout.contains("stopped by Ctrl-C, Stop sent"),
        "{}",
        r.stdout
    );
    let r = run_line_port(
        VirtualLine::with_ecu(sirius_ecu()),
        "virtual",
        &[
            "--json",
            "live",
            "--xml",
            &xml("sirius_cli.xml"),
            "--frames",
            "21A9",
        ],
        Opts {
            ctrl_c: true,
            ..Opts::default()
        },
    );
    let s = &r.event("stopped")[0];
    assert_eq!(
        (s["ok"].as_bool(), s["ctrl_c"].as_bool()),
        (Some(true), Some(true)),
        "{s}"
    );
}

#[test]
fn live_items_filter_and_compact_lines() {
    let sx = xml("sirius_cli.xml");
    let base = [
        "live",
        "--xml",
        &sx,
        "--frames",
        "21A9,21AA",
        "--count",
        "2",
        "--rate",
        "10",
    ];
    let with = |extra: &[&str], json: bool| {
        let mut v: Vec<&str> = Vec::new();
        if json {
            v.push("--json");
        }
        v.extend(base.iter().copied());
        v.extend(extra.iter().copied());
        run(sirius_ecu(), &v).0
    };
    let r = with(&["--items", "engine speed,Coolant temperature"], false);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(r.stdout.contains("Engine speed") && r.stdout.contains("Coolant temperature"));
    assert!(!r.stdout.contains("Battery voltage"), "{}", r.stdout);
    let r = with(&["--items", "Engine speed"], true);
    let live = r.event("live");
    assert_eq!(items(&live[0]["decoded"][0]).len(), 1, "{}", r.stdout);
    // 21AA has no selected item: its decoded list is empty, the reply is still there.
    assert!(live[1]["decoded"][0]["items"]
        .as_array()
        .unwrap()
        .is_empty());
    // Compact: one line per cycle, changed values only (all of them in the first cycle).
    let r = with(&["--compact"], false);
    assert_eq!(r.code, 0, "{}", r.stderr);
    let lines: Vec<&str> = r.stdout.lines().filter(|l| l.starts_with('#')).collect();
    assert_eq!(lines.len(), 2, "{}", r.stdout);
    assert!(
        lines[0].contains("Engine speed=850 rpm") && lines[0].contains("Coolant temperature=88"),
        "{}",
        lines[0]
    );
    assert!(lines[1].ends_with("(no change)"), "{}", lines[1]);
    // Compact with --items: the selected values every cycle.
    let r = with(&["--compact", "--items", "Engine speed"], false);
    let lines: Vec<&str> = r.stdout.lines().filter(|l| l.starts_with('#')).collect();
    assert_eq!(lines.len(), 2, "{}", r.stdout);
    assert!(
        lines.iter().all(|l| l.ends_with("Engine speed=850 rpm")),
        "{lines:?}"
    );
    // Unknown item: exit 8 before the port is opened.
    let r = with(&["--items", "Engine sped"], false);
    assert_eq!(r.code, 8, "{}", r.stderr);
    assert!(r.stderr.contains("Engine sped"), "{}", r.stderr);
    assert!(!r.opened);
}

fn ftdi_port(path: &str) -> kline_host::ports::PortInfo {
    kline_host::ports::PortInfo {
        path: path.into(),
        usb: Some((kline_host::ports::FTDI_VID, 0x6001)),
        manufacturer: Some("FTDI".into()),
        product: Some("FT232R USB UART".into()),
        serial: None,
    }
}

#[test]
fn selftest_on_a_port_that_is_not_a_kkl_cable_says_the_verdicts_mean_nothing() {
    // "virtual" is not in the port list: not an FTDI cable.
    let (r, _) = run(sirius_ecu(), &["selftest"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(
        r.stdout.contains(
            "this port is not a known FTDI KKL cable (no USB id: simulator, pty or a built-in port): timing verdicts below are not meaningful"
        ),
        "{}",
        r.stdout
    );
    assert!(!r.stdout.contains("USB round trip"), "{}", r.stdout);
    let (r, _) = run(sirius_ecu(), &["--json", "selftest"]);
    let s = &r.event("selftest")[0];
    assert_eq!(s["kkl_cable"], false, "{s}");
    assert_eq!(s["timing_meaningful"], false, "{s}");
    assert!(s["fast_init_cause"].is_null(), "{s}");
    // The same port listed as an FTDI device: a cable, no such line.
    let r = run_line_port(
        VirtualLine::with_ecu(sirius_ecu()),
        "virtual",
        &["selftest"],
        Opts {
            ports: vec![ftdi_port("virtual")],
            ..Opts::default()
        },
    );
    assert_eq!(r.code, 0);
    assert!(
        !r.stdout.contains("not a known FTDI KKL cable"),
        "{}",
        r.stdout
    );
    // Code review L8: a USB adapter of another make (CH340) may be a KKL cable: the host
    // timing verdicts and the fast-init cause still mean something.
    let mut ch340 = ftdi_port("virtual");
    ch340.usb = Some((0x1a86, 0x7523));
    ch340.manufacturer = None;
    ch340.product = Some("USB Serial".into());
    let r = run_line_port(
        VirtualLine::with_ecu(sirius_ecu()),
        "virtual",
        &["--json", "selftest"],
        Opts {
            ports: vec![ch340.clone()],
            ..Opts::default()
        },
    );
    let s = &r.event("selftest")[0];
    assert_eq!(s["kkl_cable"], false, "{s}");
    assert_eq!(s["timing_meaningful"], true, "{s}");
    assert_eq!(
        s["fast_init_cause"].is_string(),
        s["break_jitter"]["fast_init_ok"] == false,
        "{s}"
    );
    let r = run_line_port(
        VirtualLine::with_ecu(sirius_ecu()),
        "virtual",
        &["selftest"],
        Opts {
            ports: vec![ch340],
            ..Opts::default()
        },
    );
    assert!(
        r.stdout
            .contains("this port is not a known FTDI KKL cable (usb 1a86:7523)"),
        "{}",
        r.stdout
    );
    assert!(!r.stdout.contains("not meaningful"), "{}", r.stdout);
}

#[test]
fn dtc_counts_present_and_memorized_and_shows_the_clear_plan_only_on_request() {
    let alt = xml("bmt_cli_alt.xml");
    let ecu = || {
        let sc = kline_sim::scenario::Scenario::new()
            .text("Door switch fault", "present")
            .number("Défaut présent porte", 1.0)
            .number("Défaut mémorisé porte", 1.0);
        SpecEcu::new(EcuProfile::bmt(), fixture("bmt_cli_alt.xml"), sc).unwrap()
    };
    let (r, j) = run(ecu(), &["dtc", "--xml", &alt]);
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    assert!(
        r.stdout
            .contains("Lecture des defauts (21 A8): 3 set (present: 1, memorized: 1, other: 1)"),
        "{}",
        r.stdout
    );
    assert!(
        r.stdout
            .contains("3 fault flag(s) set (present: 1, memorized: 1, other: 1)"),
        "{}",
        r.stdout
    );
    assert!(!r.stdout.contains(" active"), "{}", r.stdout);
    assert!(!r.stdout.contains("clear-dtc would send"), "{}", r.stdout);
    j.assert_no_writes();
    let (r, _) = run(ecu(), &["dtc", "--xml", &alt, "--show-clear"]);
    assert!(
        r.stdout.contains("kline clear-dtc would send 14 FF 00"),
        "{}",
        r.stdout
    );
    // JSON keeps the clear plan and "active", and adds the counts.
    let (r, _) = run(ecu(), &["--json", "dtc", "--xml", &alt]);
    let s = &r.event("dtc_summary")[0];
    assert_eq!(s["active"], 3);
    assert_eq!(
        (s["present"].as_u64(), s["memorized"].as_u64()),
        (Some(1), Some(1))
    );
    assert!(s["clear_plan"].is_object(), "{s}");
}

/// Code review L3: Ctrl-C during the retry wait stops the read; the retry is not sent.
#[test]
fn read_retry_is_not_sent_after_ctrl_c() {
    let sx = xml("sirius_cli.xml");
    let ecu = sirius_ecu().with_fault(FaultRule::on(&[0x21, 0xA9], ReplyFault::BadChecksum).once());
    let (r, _) = run_opts(
        ecu,
        &["--json", "read", "--xml", &sx, "21A9"],
        Opts {
            ctrl_c: true,
            ..Opts::default()
        },
    );
    assert_eq!(r.event("read_retry").len(), 1, "{}", r.stdout);
    assert!(r.event("read").is_empty(), "retry was sent: {}", r.stdout);
    assert_ne!(r.code, 0, "{}", r.stdout);
}

#[test]
fn read_retries_once_after_a_bad_checksum_or_a_short_frame() {
    let sx = xml("sirius_cli.xml");
    for fault in [ReplyFault::BadChecksum, ReplyFault::DropBytes { count: 2 }] {
        let ecu = sirius_ecu().with_fault(FaultRule::on(&[0x21, 0xA9], fault).once());
        let (r, j) = run(ecu, &["read", "--xml", &sx, "21A9"]);
        assert_eq!(r.code, 0, "{fault:?}: {}\n{}", r.stdout, r.stderr);
        assert!(r.stdout.contains("retrying once in 400 ms"), "{}", r.stdout);
        assert!(
            r.stdout.contains("Engine speed     850 rpm"),
            "{}",
            r.stdout
        );
        j.assert_no_writes();
    }
    let ecu = sirius_ecu().with_fault(FaultRule::on(&[0x21, 0xA9], ReplyFault::BadChecksum).once());
    let (r, _) = run(ecu, &["--json", "read", "--xml", &sx, "21A9"]);
    assert_eq!(r.code, 0);
    let rr = &r.event("read_retry")[0];
    assert_eq!(rr["request"], "21A9");
    assert!(rr["message"].as_str().unwrap().contains("checksum"), "{rr}");
    // Still bad on the retry: exit 5, only one retry.
    let ecu = sirius_ecu().with_fault(FaultRule::on(&[0x21, 0xA9], ReplyFault::BadChecksum));
    let (r, _) = run(ecu, &["--trace", "read", "--xml", &sx, "21A9"]);
    assert_eq!(r.code, 5, "{}", r.stderr);
    assert_eq!(
        r.stderr.matches("TX 82 7A F1 21 A9").count(),
        2,
        "{}",
        r.stderr
    );
    // A missing reply is not retried (not a frame error).
    let ecu = sirius_ecu().with_fault(FaultRule::on(&[0x21, 0xA9], ReplyFault::NoReply).once());
    let (r, _) = run(ecu, &["read", "--xml", &sx, "21A9"]);
    assert_eq!(r.code, 5);
}

#[test]
fn ports_marks_the_macos_callin_twin_as_not_used() {
    let ports = vec![
        ftdi_port("/dev/cu.usbserial-AM6KP9MU"),
        ftdi_port("/dev/tty.usbserial-AM6KP9MU"),
        ftdi_port("/dev/tty.usbserial-LONE"),
    ];
    let o = || Opts {
        ports: ports.clone(),
        ..Opts::default()
    };
    let r = run_line_port(
        VirtualLine::with_ecu(sirius_ecu()),
        "virtual",
        &["ports"],
        o(),
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(
        r.stdout
            .contains("/dev/tty.usbserial-AM6KP9MU  usb 0403:6001 FT232R USB UART  (not used, use /dev/cu.usbserial-AM6KP9MU)"),
        "{}",
        r.stdout
    );
    assert!(
        !r.stdout
            .contains("LONE  usb 0403:6001 FT232R USB UART  (not used"),
        "{}",
        r.stdout
    );
    assert!(
        r.stdout.contains("cable: /dev/cu.usbserial-AM6KP9MU"),
        "{}",
        r.stdout
    );
    let r = run_line_port(
        VirtualLine::with_ecu(sirius_ecu()),
        "virtual",
        &["--json", "ports"],
        o(),
    );
    let p = r.event("port");
    assert_eq!(p[1]["use_instead"], "/dev/cu.usbserial-AM6KP9MU");
    assert!(p[0]["use_instead"].is_null() && p[2]["use_instead"].is_null());
}
