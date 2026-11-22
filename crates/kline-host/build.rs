//! Sets `KLINE_GIT_COMMIT` for the capture header (re-check R10): see build_git.rs.
include!("build_git.rs");

fn main() {
    emit_git_commit();
}
