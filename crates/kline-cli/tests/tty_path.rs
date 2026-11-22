//! Security review L8 (mutant C3): the approval code is read from the controlling terminal
//! (`/dev/tty`, Windows `CONIN$`), never from stdin, so a pipe cannot answer it.

#[test]
fn approval_terminal_is_the_controlling_terminal() {
    if cfg!(windows) {
        assert_eq!(kline_cli::TTY_IN, "CONIN$");
        assert_eq!(kline_cli::TTY_OUT, "CONOUT$");
    } else {
        assert_eq!(kline_cli::TTY_IN, "/dev/tty");
        assert_eq!(kline_cli::TTY_OUT, "/dev/tty");
    }
}

/// Child part of [`devtty_does_not_open_stdin`]: runs without a controlling terminal and with
/// stdin redirected from a file. Opening the approval terminal must fail; if it read stdin it
/// would succeed.
#[test]
fn devtty_child() {
    if std::env::var_os("KLINE_TTY_CHILD").is_none() {
        return;
    }
    match kline_cli::DevTty::open() {
        Ok(_) => panic!("DevTty opened without a controlling terminal: it is not /dev/tty"),
        Err(e) => eprintln!("DevTty::open failed as expected: {e}"),
    }
}

#[cfg(unix)]
#[test]
fn devtty_does_not_open_stdin() {
    use std::process::{Command, Stdio};
    if Command::new("setsid").arg("--version").output().is_err() {
        eprintln!("SKIPPED: setsid not available");
        return;
    }
    let stdin = std::fs::File::open(env!("CARGO_MANIFEST_DIR").to_string() + "/Cargo.toml")
        .expect("a readable file for stdin");
    let out = Command::new("setsid")
        .arg("-w")
        .arg(std::env::current_exe().unwrap())
        .args(["--exact", "devtty_child", "--nocapture"])
        .env("KLINE_TTY_CHILD", "1")
        .stdin(Stdio::from(stdin))
        .output()
        .unwrap();
    let text =
        String::from_utf8_lossy(&out.stdout).to_string() + &String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "{text}");
    assert!(text.contains("failed as expected"), "{text}");
}
