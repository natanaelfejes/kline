//! Final critique F09: `--p3`, `--inter-byte`, `--w4` override the definition's timing, with
//! sane ranges, and the effective values are traced. F15: `--dtr high|low|leave` after open.

mod common;

use common::*;

fn t_of(line: &str) -> f64 {
    // `[    12.345 ms] ...` or similar: the first number on the line.
    let s: String = line
        .chars()
        .skip_while(|c| !c.is_ascii_digit())
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    s.parse().unwrap_or_else(|_| panic!("no time in {line:?}"))
}

#[test]
fn defaults_come_from_the_definition_and_are_traced() {
    let (r, _) = run(
        sirius_ecu(),
        &["--trace", "read", "--xml", &xml("sirius_cli.xml"), "21A9"],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    let init = r
        .stderr
        .lines()
        .find(|l| l.contains("INIT 5-baud address byte 7A"))
        .unwrap();
    assert!(
        init.contains("W4 25 ms, P3 5 ms, inter-byte 0 ms"),
        "{init}"
    );
}

#[test]
fn p3_inter_byte_and_w4_override_the_definition() {
    let (r, j) = run(
        sirius_ecu(),
        &[
            "--trace",
            "--p3",
            "60",
            "--inter-byte",
            "3",
            "--w4",
            "40",
            "read",
            "--xml",
            &xml("sirius_cli.xml"),
            "21A9",
        ],
    );
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let init = r
        .stderr
        .lines()
        .find(|l| l.contains("INIT 5-baud address byte 7A"))
        .unwrap();
    assert!(
        init.contains("W4 40 ms (--w4), P3 60 ms (--p3), inter-byte 3 ms (--inter-byte)"),
        "{init}"
    );
    // Every frame goes out byte by byte with 3 ms.
    let tx: Vec<&str> = r.stderr.lines().filter(|l| l.contains("] TX 8")).collect();
    assert!(!tx.is_empty());
    assert!(
        tx.iter().all(|l| l.contains("(inter-byte 3 ms)")),
        "{tx:#?}"
    );
    // P3: from the end of a reply (`F1 7A ..`) to the next request at least 60 ms (the first
    // request after the init follows the definition's FirstRequest delay instead, not P3).
    let lines: Vec<&str> = r.stderr.lines().collect();
    let mut gaps = Vec::new();
    for (i, l) in lines.iter().enumerate() {
        if l.contains("] TX 8") {
            if let Some(prev) = lines[..i].iter().rev().find(|p| p.contains("] RX ")) {
                if prev.contains(": F1 7A ") {
                    gaps.push(t_of(l) - t_of(prev));
                }
            }
        }
    }
    assert!(gaps.len() >= 2, "{gaps:?}");
    assert!(gaps.iter().all(|g| *g >= 60.0 - 0.5), "{gaps:?}");
    // W4: ~KW2 (70) at least 40 ms after the key bytes arrived.
    let kw = lines
        .iter()
        .position(|l| l.contains("RX 3/3") && l.contains("55 D0 8F"))
        .expect("key bytes read");
    let not_kw2 = lines[kw..]
        .iter()
        .find(|l| l.contains("] TX 70"))
        .expect("~KW2 sent");
    let w4 = t_of(not_kw2) - t_of(lines[kw]);
    assert!((40.0 - 0.5..45.0).contains(&w4), "W4 {w4} ms");
    j.assert_no_writes();
}

#[test]
fn timing_options_outside_their_range_are_usage_errors() {
    for args in [
        ["--p3", "1001"],
        ["--inter-byte", "21"],
        ["--w4", "24"],
        ["--w4", "51"],
    ] {
        let mut full = args.to_vec();
        full.extend(["read", "--xml", "x.xml", "21A9"]);
        let (r, j) = run(sirius_ecu(), &full);
        assert_eq!(r.code, 2, "{args:?}: {}", r.stderr);
        assert!(!r.opened);
        j.assert_no_writes();
    }
}

#[test]
fn dtr_option_sets_the_line_after_open() {
    for (extra, want, line) in [
        (&[][..], Some("DTR high"), "PORT dtr=1 rts=left as opened"),
        (
            &["--dtr", "high"][..],
            Some("DTR high"),
            "PORT dtr=1 rts=left as opened",
        ),
        (
            &["--dtr", "low"][..],
            Some("DTR low"),
            "PORT dtr=0 rts=left as opened",
        ),
        (
            &["--dtr", "leave"][..],
            None,
            "PORT dtr=left as opened rts=left as opened",
        ),
    ] {
        let ecu = sirius_ecu();
        let j = ecu.journal();
        let mut args = extra.to_vec();
        args.extend(["--trace", "selftest"]);
        let r = run_line_port(
            kline_sim::line::VirtualLine::with_ecu(ecu),
            "virtual",
            &args,
            Opts::default(),
        );
        assert_eq!(r.code, 0, "{extra:?}: {}\n{}", r.stdout, r.stderr);
        assert!(r.stderr.contains(line), "{extra:?}: {}", r.stderr);
        // The first DTR change the line saw, before any break toggle.
        let notes: Vec<String> = j.notes().into_iter().map(|(_, n)| n).collect();
        let first_dtr = notes.iter().find(|n| n.starts_with("DTR")).cloned();
        assert_eq!(first_dtr.as_deref(), want, "{extra:?}: {notes:?}");
    }
    let (r, _) = run(sirius_ecu(), &["--dtr", "sideways", "selftest"]);
    assert_eq!(r.code, 2);
}
