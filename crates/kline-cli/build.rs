//! Sets `KLINE_GIT_COMMIT` for `--version` (re-check R10): see ../kline-host/build_git.rs.
include!("../kline-host/build_git.rs");

fn main() {
    emit_git_commit();
}
