//! The pty simulator transport (`kline_sim::pty`, `--sim-control`) needs a Unix pseudo-terminal
//! and a Unix socket; tests/pty.rs here and in kline-cli are compiled only on Unix. On Windows
//! the simulator end-to-end tests run in-process (`VirtualLine`, kline-cli tests/e2e.rs).

#[cfg(not(unix))]
#[test]
fn pty_simulator_tests_are_skipped_on_this_os() {
    eprintln!(
        "SKIPPED: pty simulator tests need a Unix pty ({}); the in-process simulator e2e covers this OS",
        std::env::consts::OS
    );
}

#[cfg(unix)]
#[test]
fn pty_simulator_tests_run_on_unix() {
    // Nothing to do: tests/pty.rs runs here.
}
