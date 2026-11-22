# Platforms: Linux, macOS, Windows

kline builds and its full test suite runs natively on all three (CI: `.github/workflows/ci.yml`,
matrix `ubuntu-latest`, `macos-latest`, `windows-latest`). What differs is the cable driver, the
port name, permissions, the FTDI latency timer, timing precision and where data lives. Nothing
in this file has been verified with the real FTDI cable on Linux or Windows; see "NOT VERIFIED".

## Build

| OS | Toolchain | Notes |
|---|---|---|
| Linux | rustup stable | no system libraries needed (`serialport` is built without libudev) |
| macOS | rustup stable | Xcode command line tools (`xcode-select --install`) for the linker |
| Windows | rustup stable, MSVC host (`x86_64-pc-windows-msvc`, or `aarch64-pc-windows-msvc` on ARM) | Visual Studio Build Tools 2022, workload "Desktop development with C++" (on ARM add "MSVC ARM64 build tools"). The GNU host (`*-windows-gnu`) also needs `dlltool` (MinGW binutils) for `getrandom`; without it the build fails with "error calling dlltool". |

```
cargo build --release -p kline-cli      # target/release/kline, kline.exe on Windows
cargo build --release -p kline-agent
cargo test --all-features
```

Build the C++ workload once on Windows, e.g. unattended:
`vs_buildtools.exe --quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --add Microsoft.VisualStudio.Component.VC.Tools.ARM64 --add Microsoft.VisualStudio.Component.Windows11SDK.22621`.

## Cable, driver, port name

| OS | Driver | Port | Auto-detect (`kline ports`, no `--port`) |
|---|---|---|---|
| Linux | `ftdi_sio` (kernel) | `/dev/ttyUSB0`, stable alias `/dev/serial/by-id/usb-FTDI_...` | `/dev/ttyUSB*` with FTDI vendor id `0403` |
| macOS | AppleUSBFTDI (DriverKit, built in) | `/dev/cu.usbserial-XXXX` | `/dev/cu.usbserial-*`. `/dev/tty.*` (callin, waits for carrier detect) is refused |
| Windows | FTDI VCP (`ftdibus.sys`/`ftser2k.sys`, from Windows Update or ftdichip.com) | `COM3`, `COM12`, ... | `COMn` with FTDI vendor id `0403` |

Windows: `COM10` and above are typed as is; `serialport` opens every name as `\\.\COMn`, and the
explicit `\\.\COM12` form is accepted too. A cheap clone cable with a counterfeit FT232 may be
disabled by some FTDI driver versions; that is a driver matter, not kline's.

Break (5-baud init) is `ioctl(TIOCSBRK/TIOCCBRK)` on Linux and macOS and
`SetCommBreak`/`ClearCommBreak` on Windows, DTR is `TIOCMBIS/TIOCMBIC` resp.
`EscapeCommFunction(SETDTR/CLRDTR)`, all through `serialport` 4.10. Read timeouts: on Windows
`serialport` sets `COMMTIMEOUTS` with `ReadIntervalTimeout = ReadTotalTimeoutMultiplier =
MAXDWORD` and our per-call constant (whole milliseconds, at least 1 ms), i.e. "return as soon as
one byte is there, else wait at most the constant": the same contract as `VMIN=0/VTIME` polling
on Unix.

Unix only: after opening, kline clears `IGNBRK`, `BRKINT` and `PARMRK` with termios (`nix`, a
`cfg(unix)` dependency), so a looped-back break reads as one `00` byte; macOS's `cfmakeraw` sets
`IGNBRK`. On macOS the `tcsetattr` for this gets a placeholder 9600 and 10400 is set again with
`IOSSIOSPEED` afterwards, as `serialport` does: `tcgetattr` reads back 10400 there and
AppleUSBFTDI refuses it in `tcsetattr` with EINVAL (measured, FT232R, macOS 27; the bug in
ab8de98/bff9da7). Windows has no such flag; whether the FTDI VCP driver delivers a looped-back break as a
`00` byte is NOT VERIFIED (without it the 5-baud init warns, unless `--strict-loopback`).

## Permissions

- Linux: the port belongs to group `dialout` (Debian, Ubuntu, Fedora) or `uucp` (Arch):
  `sudo usermod -aG dialout $USER`, then log out and in. Otherwise opening fails with exit 3
  "Permission denied".
- macOS: none.
- Windows: none; only one program can hold a COM port (close DDT2000/Renolink/terminals first,
  otherwise "Access is denied").

## Latency timer

The FTDI chip sends received bytes to the host when its buffer fills or its latency timer
expires (default 16 ms). The ~KW2 answer of a 5-baud init must leave 25..50 ms after KW2 (W4,
ISO 14230-2), so 16 ms on RX is the tight spot. `kline selftest --echo-rtt` on a looped-back K
line measures the real round trip on every OS.

- Linux: `/sys/bus/usb-serial/devices/ttyUSB0/latency_timer`. `kline selftest` reports it;
  `kline selftest --set-latency 1` writes it (needs root: `sudo kline selftest --set-latency 1`, or
  `sudo sh -c 'echo 1 > /sys/bus/usb-serial/devices/ttyUSB0/latency_timer'`). It resets when the
  cable is replugged; to make it permanent add a udev rule, e.g.
  `/etc/udev/rules.d/99-kline-ftdi.rules`:

  ```
  ACTION=="add", SUBSYSTEM=="usb-serial", DRIVER=="ftdi_sio", ATTR{latency_timer}="1"
  ```

  then `sudo udevadm control --reload` and replug.
- Windows: Device Manager > Ports (COM & LPT) > USB Serial Port > Properties > Port Settings >
  Advanced > Latency Timer (msec) = 1. The driver stores it as `LatencyTimer` (DWORD) under
  `HKLM\SYSTEM\CurrentControlSet\Enum\FTDIBUS\<VID_0403+PID_6001+serial>\0000\Device Parameters`.
  kline does not read or write the registry; `--set-latency` is Linux-only (exit 2 elsewhere).
- macOS: AppleUSBFTDI has no user setting and publishes no latency value: `ioreg -l -w0 -r -n
  AppleUSBFTDI` (FT232R, macOS 27) shows only USB ids, `IOTTYSuffix` and the callout/dialin
  paths, no `LatencyTimer`. Measure with `--echo-rtt`.

## Timing

The 5-baud init is bit-banged with break (200 ms bits, +-10 ms, ISO 9141-2) and the fast-init low
pulse TiniL is 25 +- 1 ms (ISO 14230-2). kline's clock (`kline_host::hrtimer`) sleeps with the OS
until a spin tail before each deadline and busy-waits the rest. The tail starts at 1 ms (2 ms on
Windows) and grows to twice the worst OS-sleep lateness it has seen (at most 25 ms); the 300 ms
idle wait (W5) before every init teaches it. On Windows the transport also requests a 1 ms
system timer tick (`timeBeginPeriod(1)`, released when the port closes; the default tick is
15.625 ms, which alone would break TiniL).

`kline selftest` measures this on the host before it opens any port (`selftest_timing`: 10 sleeps
each of 1, 25 and 200 ms after one 300 ms warm-up, late min/mean/max and early count, against
+-1 ms, +-1 ms and +-10 ms), so it reports even without a cable. Measured on this project's hosts
(no cable, `kline selftest`, release build, commit ab8de98; one run each, so a snapshot, not a
guarantee):

| Host | 1 ms max late | 25 ms max late | 200 ms max late | OS sleep lateness seen | learned tail |
|---|---|---|---|---|---|
| Linux container, x86_64 | 0 ms | 0.02 ms | 0 ms | 0.2 ms | 1.4 ms |
| macOS 27, M4 Pro | 0.6 ms | 0 ms | 0.4 ms | 5.0-5.8 ms | 11-12.5 ms |
| Windows 11 ARM64 (Parallels VM on that Mac), `timeBeginPeriod(1)` granted | 0.1 ms | 0.07 ms | 0.01 ms | 3.6-6.2 ms | 9-14.5 ms |

With the earlier fixed tail (commit 1caa4ad) 25 ms sleeps woke up to ~4.8 ms late on macOS and
~1.65 ms late on the Windows VM (cause not investigated; timer coalescing is a guess), which is
why the tail is adaptive. A spin cannot help when the thread itself is preempted (a loaded host or
a VM whose vCPU is descheduled): run `selftest` and close heavy programs if it says
`TOO IMPRECISE`.

## Data directories

| OS | Default | Override |
|---|---|---|
| Linux | `~/.local/share/kline/{ecus,captures}` | `--defs`/`KLINE_DDT_XML`, `--captures`/`KLINE_CAPTURE_DIR` |
| macOS | `~/.local/share/kline/{ecus,captures}` (kept, not `~/Library/Application Support`, so existing setups keep working) | same |
| Windows | `%APPDATA%\kline\{ecus,captures}` (fallback `%USERPROFILE%\AppData\Roaming\kline`) | same |

## Approval terminal

Tier 1-3 requests (`kline exec`) show the `RiskInfo` and read the confirmation code from the
controlling terminal, never stdin: `/dev/tty` on Linux and macOS, the console buffers `CONIN$` and
`CONOUT$` on Windows (they reach the console window even when stdin/stdout are redirected). As on
Unix this is an anti-accident gate, not authentication (SAFETY.md rule 2). A Windows process
without a console should fail to open `CONIN$`, so the request is refused (exit 7) and nothing is
sent (NOT VERIFIED on Windows).

## Simulator transports

- In-process (`kline_sim::line::VirtualLine`, virtual clock): all OSes. kline-cli `tests/e2e.rs`
  runs every command end to end this way, including `exec` approvals.
- pty plus a Unix control socket (`kline_sim::pty::PtySim`, `kline --sim-control`): Linux and
  macOS only. Its tests (kline-sim and kline-cli `tests/pty.rs`, kline-agent `tests/agent.rs`
  sessions) are compiled only on Unix; on Windows `kline-sim/tests/pty_unix_only.rs` prints
  `SKIPPED` and `--sim-control` fails with "the simulator transport needs a Unix pty".

## NOT VERIFIED

- Anything with the real FTDI cable on Linux or Windows: break latency, looped-back break as a
  `00` byte (Windows VCP), DTR behaviour, 10400 baud accuracy, latency timer effect.
- `--set-latency` against a real `ftdi_sio` sysfs file (tested against a fake sysfs tree).
- The Windows approval prompt on `CONIN$`/`CONOUT$` in an interactive console.
- Windows x86_64 natively (verified: Windows 11 ARM64 in a VM, `aarch64-pc-windows-msvc`).
