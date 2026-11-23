//! kline-bridge builds as `no_std` + `alloc`, and its default build has no approvals
//! (SAFETY.md rule 2: kline-core's `approved-routines` stays off unless `approvals` is asked
//! for explicitly).

use std::process::Command;

fn manifest() -> std::path::PathBuf {
    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml")
}

#[test]
fn builds_without_default_features() {
    let target_dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("bridge_no_std");
    let out = Command::new(env!("CARGO"))
        .args([
            "build",
            "--offline",
            "--quiet",
            "--no-default-features",
            "--manifest-path",
        ])
        .arg(manifest())
        .arg("--target-dir")
        .arg(&target_dir)
        .output()
        .expect("run cargo");
    assert!(
        out.status.success(),
        "no_std build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn features_tree(args: &[&str]) -> String {
    let out = Command::new(env!("CARGO"))
        .args([
            "tree",
            "--offline",
            "-e",
            "features,normal",
            "-i",
            "kline-core",
            "--manifest-path",
        ])
        .arg(manifest())
        .args(["-p", "kline-bridge"])
        .args(args)
        .output()
        .expect("run cargo tree");
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

#[test]
fn default_and_no_std_builds_have_no_approvals() {
    for args in [&[][..], &["--no-default-features"][..]] {
        let tree = features_tree(args);
        assert!(tree.contains("kline-core"), "{tree}");
        assert!(
            !tree.contains("approved-routines"),
            "approvals in the {args:?} build:\n{tree}"
        );
        assert!(
            !tree.contains("feature \"spec\""),
            "kline-spec pulled in:\n{tree}"
        );
    }
    // Control: the explicit feature does show up.
    let tree = features_tree(&["--features", "approvals"]);
    assert!(tree.contains("approved-routines"), "{tree}");
}
