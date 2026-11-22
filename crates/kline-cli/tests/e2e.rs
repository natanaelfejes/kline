//! `kline` end to end against kline-sim, in-process (virtual clock; the simulator's strict timing
//! checks apply). Every test also checks the ECU write log.

mod common;

use common::*;
use kline_sim::ecu::{AddressAccept, EcuProfile, EcuProtocol, FaultRule, ReplyFault, SpecEcu};
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
    for req in ["2701", "31A0A3", "3BA000", "1085", "82"] {
        let (r, j) = run_opts(
            sirius_ecu(),
            &["exec", "--xml", &xml("sirius_cli.xml"), req],
            Opts {
                interactive: true,
                human: Human::TypesShownCode,
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
    // Off the BMT, 31 C7 is an undocumented (expert) request, and its definition name says
    // "key": the name guard refuses it (the remote-learning exemption holds only on 0x26).
    assert_eq!(r.code, 7, "{}", r.stdout);
    let e = &r.event("risk")[0];
    assert_eq!(e["ecu_address"], "51");
    assert!(e["refused"]
        .as_str()
        .unwrap()
        .contains("immobilizer/transponder/key"));
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
    let (r, j) = run(sirius_ecu(), &["--json", "selftest"]);
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let s = &r.event("selftest")[0];
    assert_eq!(s["baud_requested"], 10400);
    assert_eq!(s["break_jitter"]["edges"], 40);
    assert_eq!(s["break_jitter"]["five_baud_ok"], true, "{s}");
    assert!(s["note"].as_str().unwrap().contains("latency"));
    // No ECU was woken and nothing was written.
    j.assert_no_writes();
    assert!(r.event("connected").is_empty());
    // A macOS callin device is refused before opening.
    let (r, _) = run_with_port(sirius_ecu(), "/dev/tty.usbserial-X", &["selftest"]);
    assert_eq!(r.code, 3, "{}", r.stderr);
    assert!(r.stderr.contains("/dev/cu.usbserial-X"), "{}", r.stderr);
    assert!(!r.opened);
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
