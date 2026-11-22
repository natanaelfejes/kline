//! `cargo build -p kline-core --no-default-features` must compile: the crate is no_std + alloc
//! without the `std`/`spec` features, and does not pull kline-spec.

use std::process::Command;

#[test]
fn builds_without_default_features() {
    let target_dir = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("no_std_build");
    let manifest = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml");
    let out = Command::new(env!("CARGO"))
        .args([
            "build",
            "--offline",
            "--quiet",
            "--no-default-features",
            "--manifest-path",
        ])
        .arg(&manifest)
        .arg("--target-dir")
        .arg(&target_dir)
        .output()
        .expect("run cargo");
    assert!(
        out.status.success(),
        "no_std build failed:\n{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let tree = Command::new(env!("CARGO"))
        .args([
            "tree",
            "--offline",
            "--no-default-features",
            "-e",
            "normal",
            "--manifest-path",
        ])
        .arg(&manifest)
        .output()
        .expect("run cargo tree");
    let tree = String::from_utf8_lossy(&tree.stdout);
    assert!(tree.contains("kline-core"), "cargo tree output: {tree}");
    assert!(
        !tree.contains("kline-spec"),
        "no_std build pulls kline-spec:\n{tree}"
    );
}
