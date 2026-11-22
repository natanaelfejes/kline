//! `kline --version` names the git commit of the build (re-check R10, final critique F11), and
//! every capture starts with a header that records it.

mod common;

#[path = "../../kline-host/build_git.rs"]
mod build_git;

use common::*;

/// What the build script should have found for this work tree (the build reruns on any change,
/// so the tree is the one the test binary was built from).
fn expected_commit() -> String {
    let root = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let head = build_git::git_output(&root, &["rev-parse", "--short", "HEAD"]);
    let status = head
        .as_ref()
        .and_then(|_| build_git::git_output(&root, &["status", "--porcelain"]));
    build_git::describe_commit(head.as_deref(), status.as_deref())
}

#[test]
fn version_names_the_commit_and_keeps_the_licence_lines() {
    let commit = expected_commit();
    let (r, _) = run(sirius_ecu(), &["--version"]);
    assert_eq!(r.code, 0, "{}", r.stderr);
    assert!(
        r.stdout.contains(&format!("kline 0.1.0 ({commit})")),
        "{commit}: {}",
        r.stdout
    );
    let (r, _) = run(sirius_ecu(), &["-V"]);
    assert!(r.stdout.contains(&format!("({commit})")), "{}", r.stdout);
    // The long form (the binary) keeps the copyright and licence lines.
    let o = std::process::Command::new(env!("CARGO_BIN_EXE_kline"))
        .arg("--version")
        .output()
        .unwrap();
    let out = String::from_utf8(o.stdout).unwrap();
    assert!(out.contains(&format!("({commit})")), "{out}");
    assert!(out.contains("Copyright (C) 2026 Natanael Feješ"), "{out}");
    assert!(out.contains("GPL-3.0-or-later"), "{out}");
}

#[test]
fn the_capture_header_records_the_commit_and_replay_skips_it() {
    let commit = expected_commit();
    let (r, _) = run(
        sirius_ecu(),
        &["read", "--xml", &xml("sirius_cli.xml"), "21A9"],
    );
    assert_eq!(r.code, 0, "{}", r.stderr);
    let cap = r.captures().pop().expect("a capture");
    let text = std::fs::read_to_string(&cap).unwrap();
    let first: serde_json::Value = serde_json::from_str(text.lines().next().unwrap()).unwrap();
    let h = &first["header"];
    assert_eq!(h["tool"], "kline", "{first}");
    assert_eq!(h["version"], "0.1.0", "{first}");
    assert_eq!(h["commit"], commit.as_str(), "{first}");
    assert_eq!(h["command"], "read", "{first}");
    // The capture still replays (the header is not an entry).
    let (r, _) = run(sirius_ecu(), &["replay", &cap.display().to_string()]);
    assert_eq!(r.code, 0, "{}\n{}", r.stdout, r.stderr);
    let f = std::fs::File::open(&cap).unwrap();
    let entries = kline_sim::replay::read_jsonl(std::io::BufReader::new(f)).unwrap();
    assert!(!entries.is_empty());
}

#[test]
fn describe_commit_marks_dirty_and_unknown() {
    use build_git::describe_commit as d;
    assert_eq!(d(Some("5e0d5ff\n"), Some("")), "5e0d5ff");
    assert_eq!(
        d(Some("5e0d5ff\n"), Some(" M docs/cli.md\n")),
        "5e0d5ff-dirty"
    );
    assert_eq!(d(Some("5e0d5ff"), None), "5e0d5ff-dirty");
    assert_eq!(d(None, None), "unknown");
    assert_eq!(d(Some("  "), Some("")), "unknown");
}
