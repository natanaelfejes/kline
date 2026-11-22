// The git commit a binary was built from (re-check R10, final critique F11), for
// `kline --version`, `kline-agent --version` and the capture header. Included by the build
// scripts of kline-host, kline-cli and kline-agent, and compiled into kline-host's unit tests.
//
// `KLINE_GIT_COMMIT` (rustc env) is the short hash of HEAD, with `-dirty` when the work tree
// had changes (tracked or untracked, not ignored), or `unknown` without git or outside a git
// checkout. Setting `KLINE_GIT_COMMIT` in the build environment overrides it (a source export
// without `.git`).

/// `short`, `short-dirty` or `unknown`. `head`: output of `git rev-parse --short HEAD`;
/// `status`: output of `git status --porcelain` (None: it failed, so the state is unknown and
/// the tree is reported dirty).
#[allow(dead_code)]
pub fn describe_commit(head: Option<&str>, status: Option<&str>) -> String {
    let Some(h) = head.map(str::trim).filter(|h| !h.is_empty()) else {
        return "unknown".to_string();
    };
    match status {
        Some(s) if s.trim().is_empty() => h.to_string(),
        _ => format!("{h}-dirty"),
    }
}

#[allow(dead_code)]
pub fn git_output(dir: &std::path::Path, args: &[&str]) -> Option<String> {
    let o = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .arg("--no-optional-locks")
        .args(args)
        .output()
        .ok()?;
    if !o.status.success() {
        return None;
    }
    String::from_utf8(o.stdout).ok()
}

/// Print the `cargo:` lines of a build script: the env var and what makes it run again.
#[allow(dead_code)]
pub fn emit_git_commit() {
    use std::path::{Path, PathBuf};
    println!("cargo:rerun-if-env-changed=KLINE_GIT_COMMIT");
    if let Ok(v) = std::env::var("KLINE_GIT_COMMIT") {
        println!("cargo:rustc-env=KLINE_GIT_COMMIT={v}");
        return;
    }
    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap_or_default());
    // crates/<name> -> the workspace root.
    let root = manifest
        .parent()
        .and_then(Path::parent)
        .unwrap_or(&manifest)
        .to_path_buf();
    let head = git_output(&root, &["rev-parse", "--short", "HEAD"]);
    let status = head
        .as_ref()
        .and_then(|_| git_output(&root, &["status", "--porcelain"]));
    println!(
        "cargo:rustc-env=KLINE_GIT_COMMIT={}",
        describe_commit(head.as_deref(), status.as_deref())
    );
    // Run again when HEAD, the branch it points to or the index moves, and when any file of
    // the work tree changes (the dirty flag). `target` and the ignored definitions are left out.
    let git_path = |p: &str| {
        git_output(&root, &["rev-parse", "--git-path", p])
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .map(|s| root.join(s))
    };
    for p in ["HEAD", "index", "packed-refs"] {
        if let Some(path) = git_path(p) {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    if let Some(r) = git_output(&root, &["symbolic-ref", "-q", "HEAD"]) {
        if let Some(path) = git_path(r.trim()) {
            println!("cargo:rerun-if-changed={}", path.display());
        }
    }
    if let Ok(entries) = std::fs::read_dir(&root) {
        for e in entries.flatten() {
            let name = e.file_name();
            let name = name.to_string_lossy();
            if matches!(name.as_ref(), "target" | ".git" | "ddt-xml" | "spec-out") {
                continue;
            }
            println!("cargo:rerun-if-changed={}", e.path().display());
        }
    }
}
