# Running kline on Linux, macOS and Windows

How to set up a KKL cable for kline on each operating system: build tools, FTDI driver, port
names, permissions, the FTDI latency timer, timing precision for the 5-baud and fast init, and
where kline keeps its files.

## Contents

- [Build](#build)
- [Cable, driver, port name](#cable-driver-port-name)
- [Break, DTR and serial timeouts](#break-dtr-and-serial-timeouts)
- [Permissions](#permissions)
- [FTDI latency timer](#ftdi-latency-timer)
- [Timing precision](#timing-precision)
- [Data directories](#data-directories)
- [Approval terminal](#approval-terminal)
- [Simulator transports](#simulator-transports)
- [Not verified](#not-verified)

## Summary

kline builds and its full test suite runs natively on all three (CI: `.github/workflows/ci.yml`,
matrix `ubuntu-latest`, `macos-latest`, `windows-latest`). What differs is the cable driver, the
port name, permissions, the FTDI latency timer, timing precision and where data lives. Nothing
in this file has been verified with the real FTDI cable on a car; on Windows only the
Parallels VM with the cable on the Mac's USB (no K-line power) was measured; see [Not verified](#not-verified).

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

## Break, DTR and serial timeouts

Break (5-baud init) is `ioctl(TIOCSBRK/TIOCCBRK)` on Linux and macOS and
`SetCommBreak`/`ClearCommBreak` on Windows, DTR is `TIOCMBIS/TIOCMBIC` resp.
`EscapeCommFunction(SETDTR/CLRDTR)`, all through `serialport` 4.10. Read timeouts: on Windows
`serialport` sets `COMMTIMEOUTS` with `ReadIntervalTimeout = ReadTotalTimeoutMultiplier =
MAXDWORD` and our per-call constant (whole milliseconds, at least 1 ms), i.e. "return as soon as
one byte is there, else wait at most the constant": the same contract as `VMIN=0/VTIME` polling
on Unix.

Write timeouts: `serialport` writes the same constant into `WriteTotalTimeoutConstant`, so the
last read's timeout (down to 1 ms) also bounded the next `WriteFile`, which then returns 0 bytes
written and std's `write_all` fails with "failed to write whole buffer" (seen once in `selftest
--echo-rtt` on the Windows VM). Measured with the FT232R through Parallels USB passthrough, 100
writes each: 20 bytes at a 10 ms timeout returned 0 written 15-16 times, 5 bytes at 10 ms once,
20 bytes at 200 ms never (a 20-byte write+flush takes up to ~21 ms: WriteFile waits for the
bytes to go out). kline now sets the write's own timeout before every write (twice the bytes'
time at 10400 baud plus 200 ms, `transport::write_timeout_ms`) and reports a stall as
`serial write stalled: N of M bytes accepted within T ms` with a replug hint (exit 3);
`selftest --echo-rtt` reports it as "echo RTT: not measured: write failed ..." and exits 0.

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

## FTDI latency timer

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

## Timing precision

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

Break edges on the Windows VM (FT232R on the Mac's USB, attached to Parallels "Windows 11"
ARM64 as COM1; 2026-11-22). `kline selftest` (40 edges, 10 ms half-period) gave max deviation
0.55-5.74 ms over 8 runs (one outlier 63.7 ms with a 47 ms `set_break`), so TiniL (+-1 ms) is
`TOO IMPRECISE` in most runs while 5-baud (+-10 ms) is OK; on the Mac the same cable passes. A
probe (100 pulses of 25 ms each, spin-waited, timeBeginPeriod(1), two runs) per thread priority:

| Thread priority | SetCommBreak/ClearCommBreak call p50 / p99 / max | TiniL error p50 / p99 / max | pulses >= 1 ms off |
|---|---|---|---|
| normal | 0.26-0.30 / 0.8-1.8 / 3.2-14.6 ms | 0.28-0.31 / 2.9-3.2 / 3.3-4.8 ms | 4-7 % |
| `THREAD_PRIORITY_TIME_CRITICAL` | 0.28-0.31 / 1.9-2.1 / 2.7-3.6 ms | 0.30-0.33 / 2.0-2.7 / 2.6-3.2 ms | 5-13 % |
| MMCSS "Pro Audio" (`AvSetMmThreadCharacteristicsW`) | 0.28-0.31 / 0.9-1.3 / 1.5-1.8 ms | 0.32-0.34 / 1.8-2.7 / 2.2-8.1 ms | 5-6 % |
| MMCSS + TIME_CRITICAL | 0.28-0.29 / 1.2-1.6 / 1.9-5.0 ms | 0.30-0.31 / 2.3-3.8 / 3.2-5.1 ms | 7-9 % |

Priority does not change it: the spread is in the break call itself, i.e. the USB round trip
through the VM's passthrough, not in our sleep, so kline does not raise thread priority.
`selftest` says so when TiniL fails with a slow break call (`fast_init_cause`). Fast init from
this VM may therefore miss TiniL in a few percent of attempts; bare-metal Windows is NOT
VERIFIED.

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
Unix this is an anti-accident gate, not authentication ([SAFETY.md](SAFETY.md) rule 2). A Windows process
without a console should fail to open `CONIN$`, so the request is refused (exit 7) and nothing is
sent (NOT VERIFIED on Windows).

## Simulator transports

- In-process (`kline_sim::line::VirtualLine`, virtual clock): all OSes. kline-cli `tests/e2e.rs`
  runs every command end to end this way, including `exec` approvals.
- pty plus a Unix control socket (`kline_sim::pty::PtySim`, `kline --sim-control`): Linux and
  macOS only. Its tests (kline-sim and kline-cli `tests/pty.rs`, kline-agent `tests/agent.rs`
  sessions) are compiled only on Unix; on Windows `kline-sim/tests/pty_unix_only.rs` prints
  `SKIPPED` and `--sim-control` fails with "the simulator transport needs a Unix pty".

## Not verified

- Anything with the real FTDI cable on a car (powered K line), on any OS: looped-back break as
  a `00` byte (Windows VCP), echo RTT, DTR behaviour, 10400 baud accuracy, latency timer effect.
- The real FTDI cable on Linux, and on bare-metal Windows (Windows numbers above are from the
  Parallels VM only); whether the write stall also happens without a VM.
- `--set-latency` against a real `ftdi_sio` sysfs file (tested against a fake sysfs tree).
- The Windows approval prompt on `CONIN$`/`CONOUT$` in an interactive console.
- Windows x86_64 natively (verified: Windows 11 ARM64 in a VM, `aarch64-pc-windows-msvc`).
