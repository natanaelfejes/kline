# kline command line reference

This page documents the `kline` and `kline-agent` commands: options, ECU selection, every
command, the approval flow for service functions, exit codes, capture files, and a step-by-step
first test at the car. Run `kline --help` or `kline <command> --help` for the short version.

## Contents

- [Building kline and kline-agent](#building-kline-and-kline-agent)
- [Common options](#common-options)
- [Selecting the ECU](#selecting-the-ecu)
- [Commands](#commands)
- [exec: sending a service request with approval](#exec-sending-a-service-request-with-approval)
- [Name guard](#name-guard)
- [Exit codes](#exit-codes)
- [Capture files](#capture-files)
- [Running against the simulator](#running-against-the-simulator)
- [First test on the car](#first-test-on-the-car)
- [Not verified (host and cable)](#not-verified-host-and-cable)
- [kline-agent (read-only)](#kline-agent-read-only)

## Building kline and kline-agent

The command line is split over three crates:

| Crate | What | Approvals |
|---|---|---|
| `crates/kline-host` | shared runtime: transports (FTDI KKL cable, simulator pty), link runner, JSONL capture, `--trace`, ECU definitions, the Tier 0 commands | none: depends on kline-core **without** `approved-routines` |
| `crates/kline-cli` | binary `kline`: everything above plus `exec` | the only crate that enables kline-core's `approved-routines` |
| `crates/kline-agent` | binary `kline-agent` for scripts and AI assistants: reads plus `prepare` | none (`tests/agent.rs` runs `cargo tree -p kline-agent -e features` and fails if `approved-routines` or `kline-cli` appears) |

Build each binary on its own when you deploy them:

```
cargo build --release -p kline-cli      # target/release/kline
cargo build --release -p kline-agent    # target/release/kline-agent
```

Cargo unifies features across the packages of one build. `cargo build -p kline-cli -p kline-agent`
(or `--workspace`, or `--all-features`) compiles kline-core once, with `approved-routines`, and
links that copy into kline-agent too. kline-agent has no code path to an approval (it has no
`exec`, and kline-host has none), but in such a build the approval types are present in the binary,
so kline-agent **refuses to start** (exit 7, message on stderr) when `safety::APPROVALS_COMPILED`
is set. Always build it alone: `cargo build --release -p kline-agent`. In a joint `cargo test`
the agent's session tests are skipped with a message; `cargo test -p kline-agent` runs them.

## Common options

| Option | Meaning |
|---|---|
| `--json` | one JSON object per line on stdout (errors too: `{"event":"error","code":N,"kind":..,"message":..}`). Notes go to stderr. `kline-agent` always uses JSON |
| `--trace` | every action with a time stamp on stderr: DTR, break edges, sleeps, break loopback, flushes (with the bytes discarded), every TX frame and every RX read (`RX got/wanted in N ms`). Also saved as `<capture>.trace.txt` |
| `--port PATH` | serial port. Default: the single `/dev/cu.usbserial-*` (macOS AppleUSBFTDI), FTDI `/dev/ttyUSB*` (Linux) or FTDI `COMn` (Windows; `COM10` and above work as typed); none or several is exit 3. A macOS callin device `/dev/tty.*` is refused (exit 3; it blocks on carrier detect): use its `/dev/cu.*` twin |
| `--sim-control SOCKET` | `--port` is then kline-sim's pty, break/DTR go over this control socket ([sim.md](sim.md#pty-transport-how-break-and-dtr-are-represented)) |
| `--capture-dir DIR` (alias `--captures`) | default `$KLINE_CAPTURE_DIR`, else `~/.local/share/kline/captures` (Windows `%APPDATA%\kline\captures`) |
| `--defs DIR` | definitions for `--ecu`. Default `$KLINE_DDT_XML` (deprecated fallback `$CLIO_DDT_XML`, read for one release), else `~/.local/share/kline/ecus` |

## Selecting the ECU

These options apply to every command that talks to an ECU, and to `risk`, `prepare` and `replay`.

| Option | Meaning |
|---|---|
| `--ecu sirius` | `SIRIUS32___EMS3132___Soft_RE1_maj.xml` (0x7A) |
| `--ecu bmt` | `BMT_X65_ph1_p.xml` (0x26); `BMT_SAGEM.xml` is an extra `ident` candidate if present |
| `--xml PATH` | any DDT2000 ECU definition; repeat for `ident` candidates (the first one sets up the link) |
| `--address 0xNN` | overrides the definition's `<Function Address>` (also the safety target) |
| `--vehicle PATH` | vehicle addressing file (e.g. `vehicles/x65_addressing.xml`): function name, protocols, tool address |
| `--variant auto\|single\|multi\|fast` | init variant ([protocol.md](protocol.md#init-variants-single-ecu-multipoint-fast-init)). `auto` tries FastInit, SingleEcu (DDT2000's detection order), then MultiEcu, skips variants that would run the identical init, and logs each attempt (`init_attempt` events with `round`) |
| `--init-attempts N` | rounds over the variants, 500 ms apart (default 3, DDT2000) |
| `--nrc-tries N` | `ident`: tries of a request answered `7F xx 21`/`23` (default 10; 200/500 ms) |
| `--periodic-tester-present` | `3E` every 2000 ms with a required reply (DDT2000 screen option) instead of after 3000 ms idle |
| `--strict-loopback` | 5-baud: abort with "K line inactive" when RX shows nothing during the start-bit break, like DDT2000 (which waits for the Windows `EV_BREAK` event). Default: a `warning` event (`break_loopback_not_seen`) and the init goes on, because a POSIX tty may deliver no byte for a break ([protocol.md](protocol.md#5-baud-init-iso-9141-2-slow-init)) |
| `--session auto\|none\|a0\|81` | after the init: `auto` sends `10 A0` if the definition has it, else `10 81` if it has that (KWP only). A negative answer is reported (`note: not retried with another session`) and the command continues; no other session is tried (e.g. `10 C0`, seen in other Renault 0x7A logs, is an Expert request only) |

Protocol and timing come from the definition's `<K>` section through kline-core (`spec::k_timing`):
ISO8 5-baud, KWP 5-baud (with ~KW2 handshake), KWP fast init. A definition with only `<CAN>`
is refused: "CAN not supported by a KKL cable yet" (exit 8).

## Commands

| Command | Does | Notes |
|---|---|---|
| `ports` | lists ports, marks candidates `*`, says which cable would be used | |
| `selftest [--echo-rtt] [--set-latency MS]` | without the car: first measures the host's sleep precision with no port open (`selftest_timing`: 10 sleeps each of 1, 25 and 200 ms, late min/mean/max and early count against +-1 ms, +-1 ms and +-10 ms; [platforms.md](platforms.md#timing-precision)), so it reports even without a cable (then exit 3). Then opens the port at 10400, shows the USB id and the baud the driver reports (`baud_reported_source`: on macOS the termios output speed read back with `tcgetattr` after `IOSSIOSPEED`, the tty layer's record, not the FTDI divisor), toggles break 20 times (10 ms half-period, nothing written) and reports the edge jitter against the 5-baud (+-10 ms) and fast-init (+-1 ms) tolerances; when TiniL fails, `fast_init_cause` says whether the break call itself was slow (USB round trip, e.g. VM passthrough) or the sleeps woke late. `--echo-rtt` also writes a single `FF` byte 10 times and times its echo (USB latency, `kw2_handshake_ok` when every byte is back in < 25 ms; a write the driver does not take stops it with `write_error` and exit 0); it needs a looped-back K line (cable on the car's OBD socket, no init sent, or TX wired to RX). Linux: also reports the ttyUSB port's FTDI `latency_timer`; `--set-latency 1` writes it first (root or udev rule, [platforms.md](platforms.md#ftdi-latency-timer); elsewhere exit 2) | `kline` only; without `--echo-rtt` unplug the cable from the car |
| `connect` | init (variants), session start, Stop | reports sync, KW1 KW2, ~address, key-byte match |
| `ident` | `21 80` (KWP; else Tier 0 requests named like "ident"; NRC 21/23 retried like DDT2000), chooses the definition | DDT2000 AutoIdent ladder (exact, unique, soft, diag, soft+diag; DiagVersion numeric, Supplier/Soft/Version whitespace-normalized, case-sensitive) when the candidates have `<AutoIdents>`; else the first candidate whose key bytes match and whose identification requests decode. A reply shorter than MinBytes is decoded anyway and flagged; `ddt2000_diagfailures_accepts` says whether DiagFailures (exactly 26 bytes) would take it |
| `read REQ [--set ITEM=VALUE ..]` | one request (definition name or hex), decoded | Tier 0 only; a non-read is refused before the port is opened (exit 7) and again by kline-core. `--set` (also on `risk`, `exec`, `prepare`): builds the named request from its `<Sent>` data items (`Request::build`); VALUE is a number in physical units (must lie on the item's scaling grid), a list label, or `hex:..`; unknown items and off-grid values are exit 8 before anything is opened |
| `live --frames A,B --rate HZ [--count N]` | all frames once per cycle, `HZ` cycles/s | TesterPresent when idle >= TesterPresent delay (or every 2000 ms with `--periodic-tester-present`); Ctrl-C stops cleanly; a cycle in which every request failed is retried after 400 ms, the 4th in a row is exit 5 (DDT2000 "Read Data : Retry n/3") |
| `dtc` | `<Failures Id="1">` (Sirius 32): reads the Tier 0 frames that carry `FailureFlag="1"` items of `<Devices>` and lists the set flags with their device (DDT2000 DiagFailures); otherwise the frames whose name says `panne`/`defaut`/`fault`/`dtc`, non-zero items; otherwise (ISO8 Safir: `Failures Id="0"`, which DDT2000 shows as no fault list) a `dtc_note` saying so and the non-zero fault-named items of its Tier 0 reads (Safir `33 50`/`33 51`/`33 52` "Panne presente ..."). Ends with the clear plan | never clears |
| `clear-dtc [--ddt2000-clear]` | Tier 1 clear: prints the exact bytes (default: `ClearDiagnosticInformation.*` if the definition has one, else the definition's own `14 ..` request; Sirius `14 00 00`) and what DDT2000's global clear would send (`14 FF 00` for the Sirius), then the `exec` approval | `kline` only; `--ddt2000-clear` sends DDT2000's choice |
| `scan --range 21:B0-C5 [--interval MS]` | kline-core `ScanPlan`: `21`/`1A` only, >= 200 ms apart, stops after 3 missing replies or a security NRC (exit 12) | `kline` only; not offered by kline-agent |
| `risk REQ` | tier and `RiskInfo` of a request, nothing sent | exit 7 when forbidden or refused by the name guard |
| `exec REQ [--set ..] [--stop REQ [--stop-set ..]]` | ONE non-read request; `--stop` offers a second request afterwards (end of an actuator test) with its own `RiskInfo` and code | `kline` only, see below |
| `replay FILE [--ecu/--xml]` | decodes a capture offline: wake-ups, each request with its replies (echo stripped, 0x78 sequences split), decoded with the definition if given | |
| `prepare REQ` | `kline-agent` only: `RiskInfo` plus the exact `kline ... exec ...` command for a human | never sends |

## exec: sending a service request with approval

`exec` refuses, before opening anything, when `--json` is given or stdin/stdout is not a
terminal (exit 7). Otherwise it shows the request, its definition names and its `RiskInfo`, opens
the session, and for Tier 2 (config write) first reads `21 80` (the definition must be the one
`ident` chooses, unambiguously) and the backup read (`21 A1` for BMT `3B D2`, else `21 lid`).
It then makes the kline-core challenge, shows it on `/dev/tty` with a random code (6
characters, 10 for expert tier) and reads the answer from `/dev/tty` (not stdin), sending
TesterPresent while it waits (default 120 s, `--confirm-timeout`). A wrong code, timeout or
Ctrl-C is exit 9 and nothing is sent. Definition names on these screens are printed sanitized
(control characters, escape sequences and bidi overrides shown as `\u{..}`).

`--stop REQ` (e.g. an actuator test's stop payload): checked before anything is opened (a refused
stop refuses the whole command); after the request was sent, whatever its reply, kline shows a
second challenge for the stop request with its own code, keeps the session alive, and sends the
stop when that code is typed. If it is not typed in time the stop is NOT sent (said on stdout);
the session then ends with StopCommunication, which on most ECUs also ends an output test
(NOT VERIFIED on this car). One challenge per session at a time (kline-core), so the stop cannot
be approved before the request is sent.

The code is an anti-accident gate: an automated agent that drives a pseudo-terminal can read and
type it, and the owner permits that when he chooses. It is not a defence against such an agent.

## Name guard

Name guard (kline-core, all ECUs, Tier 1-3; `Session` applies it with the definition the CLI
loaded, `kline_core::spec::definition_guard`): a request is refused (exit 7) when a definition request with
those bytes, or one of its data items (name, description, mnemonic), contains an
immobilizer/transponder/key keyword (`kline_core::safety::immobilizer_keyword`: anti-demarrage,
transpond, immo, verlog, verrou, apptrp, ap1cle, code cle, key code, wegfahr, and the words
cle/cles/key/keys). The exact BMT remote-learning payloads `31C7000000`/`31C8000000` on a BMT/UCH/decoder
(0x26/0xA6 or a BMT/UCH/decoder definition) are exempt (their names contain "CLE"). "verrou" also matches central locking; that is refused too.

## Exit codes

| Code | Meaning |
|---|---|
| 0 | success |
| 1 | internal error |
| 2 | usage error (bad arguments; also `exec` given a read) |
| 3 | port: no cable, several cables, cannot open, transport I/O error |
| 4 | init: every init attempt failed |
| 5 | communication error after the init (no reply, collision, bad frame, live gave up) |
| 6 | the ECU answered negatively (`7F sid nrc`) |
| 7 | refused: safety policy, name guard, `exec` without a TTY or with `--json` (nothing sent) |
| 8 | definition: missing/invalid file, CAN only, unknown request name, no fault/ident request |
| 9 | approval not given: wrong code, timeout, terminal closed (nothing sent) |
| 10 | reply does not decode with the definition |
| 11 | capture/replay file problem |
| 12 | scan stopped early (3 missing replies or a security NRC) |

## Capture files

Every session writes `<UTC>-<ecu>-<command>-<pid>.jsonl` in the capture directory, one line per
TX/RX, flushed as written, in the kline-sim replay format ([sim.md](sim.md#replay-log-jsonl)) plus `unix_ms`:

```
{"t_ms":300,"dir":"tx","kind":"five_baud","hex":"7A","unix_ms":1795342272000}
{"t_ms":2383,"dir":"rx","hex":"55D08F","unix_ms":...}
{"t_ms":2408,"dir":"tx","hex":"70","unix_ms":...}
```

`t_ms` is ms since the port was opened. `kline replay` decodes it; kline-sim's `ReplayEcu` plays it
back as the ECU (`crates/kline-cli/tests/replay.rs` replays a CLI capture and gets the same
decoded values).

## Running against the simulator

```
kline --port /dev/pts/N --sim-control /tmp/sim.ctl connect --xml crates/kline-cli/tests/fixtures/sirius_cli.xml
```

The tests start `kline_sim::pty::PtySim` themselves; there is no standalone simulator binary yet.
How the simulator works: [sim.md](sim.md).

## First test on the car

This is the planned first hardware-in-the-loop session. Nothing here has been run on the car yet. Do the steps in order and keep every capture
(`~/.local/share/kline/captures`, with `.trace.txt` when `--trace` is on). Do **not** run `exec` or
`scan` on the first visit.

Before: definitions in `~/.local/share/kline/ecus` (or pass `--defs`); cable in the OBD
socket and the Mac; ignition ON, engine off; nothing else (DDT2000, another tester) on the line.

0. At home, cable in the Mac only: `kline selftest`. Expect `5-baud bits (+-10 ms): OK`; if the
   fast-init line says `TOO IMPRECISE`, fast init may fail (the auto order tries it first).
1. `kline ports`
   - OK: a line `* /dev/cu.usbserial-XXXX  usb 0403:6001 ...` and `cable: /dev/cu.usbserial-XXXX`.
   - Fail: `no KKL cable found` (exit 3): replug, check `ls /dev/cu.*`; several cables: pass `--port`.
2. `kline --trace connect --ecu bmt --variant single`
   - OK (exit 0): `BREAK LOOPBACK seen`, about 2 s of `BREAK on/off` + `SLEEP 200 ms`,
     `RX 3/3 ...: 55 D0 8F`, `TX 70`, `RX 2/2 ...: 70 D9`, then
     `connected: bmt at 26 (kwp, kwp 5-baud) with variant single; sync 55 KW D0 8F, ~address D9`
     and `TX 81 26 F1 82 ..` (Stop).
   - `BREAK LOOPBACK NOT seen` / `K line inactive` (exit 4): the break does not reach RX. Check
     ignition, the cable's K line (OBD pin 7). How the FTDI reports a looped break under macOS is
     NOT VERIFIED: send the trace.
   - `RX 0/3` (`no keywords`): the ECU did not wake on 0x26. Go to step 3.
   - `RX 1/3` or `2/3`, or key bytes other than `D0 8F`: keep the capture, stop, report it.
3. Only if step 2 got no keywords: `kline --trace connect --ecu bmt --variant multi`
   (expects `TX A6` as the 5-baud byte and `~address 59`), then `--variant fast`
   (`TX 81 26 F1 81 19`, expects `C1 ..`). Then `kline --trace connect --ecu bmt` (auto) should
   log the failed attempts and connect with the variant that worked.
4. `kline ident --ecu bmt`
   - OK: `21 80 -> 61 80 ...`, `definition: .../BMT_X65_ph1_p.xml` (or BMT_SAGEM). `AMBIGUOUS` means
     both definitions decode the reply: fine for reads, decides nothing for config writes.
   - `7F 21 xx` (exit 6) or `NONE fits` (exit 10): keep the capture, report it.
5. `kline live --ecu bmt --frames 21A3 --rate 2` and press the remote a few times: the remote
   status items should change. Ctrl-C to stop.
6. `kline dtc --ecu bmt`: lists the fault flags that are set (exit 0 even when some are set).
7. `kline --trace connect --ecu sirius --variant single`
   - OK: `RX 3/3 ...: 55 D0 8F`, `TX 70`, `RX 2/2 ...: 70 85`, then `session 10 A0: ok 50A0`.
   - Failures as in step 2; then step 3 with `--ecu sirius` (`FA` / `~address 05`; fast init
     `81 7A F1 81 6D`).
8. `kline ident --ecu sirius`, then
   `kline live --ecu sirius --frames 21A9,21AA --rate 2` (engine off: rpm 0, coolant and battery
   voltage plausible), then start the engine and watch rpm.
9. `kline dtc --ecu sirius`.
10. Offline: `kline replay ~/.local/share/kline/captures/<file>.jsonl --ecu sirius` should show the same
    values. Send the captures and traces for the [protocol.md "Not verified"](protocol.md#not-verified-needs-the-car) list.

## Not verified (host and cable)

- macOS speed setting: `serialport` 4.10.1 sets 10400 with Apple's `IOSSIOSPEED` ioctl
  (read in its source: `posix/termios.rs` macOS `set_termios`, `posix/tty.rs` macOS
  `set_baud_rate`), so no extra code path was added. Whether AppleUSBFTDI then really runs at
  10400 (and not the nearest standard rate) is not measured: `selftest` only shows what the
  driver reports, and on macOS `serialport` returns the requested value, not a read-back.
- FTDI latency timer: DDT2000/Renolink users set it to 1 ms on Windows; macOS AppleUSBFTDI is
  said to default to about 16 ms (web hint, not checked). With 16 ms, RX bytes can arrive up to
  16 ms late: harmless against the 300 ms reply timeout, but it eats into the 5-baud W4 window
  (we send ~KW2 25 ms after reading KW2; ISO 9141 allows 25..50 ms). `selftest --echo-rtt`
  measures it on a looped-back K line; not yet run on the cable.
- Break on RX: the port is opened with IGNBRK, BRKINT and PARMRK cleared (macOS `cfmakeraw` sets
  IGNBRK), so a break should read as one `00`. Whether AppleUSBFTDI delivers it is not known;
  without it the 5-baud init only warns (`--strict-loopback` aborts).
- A public ELM327 log shows 5-baud init to 0x7A failing where fast init worked (web hint):
  consistent with the fast-init-first `auto` order, not verified on this car.
- Some Renault 0x7A logs use `10 C0`; our Sirius definition says `10 A0`. Definition-driven, a
  refusal is reported, `10 C0` is never tried automatically (it is an Expert request with a programming-session warning).

## kline-agent (read-only)

`kline-agent` is meant for scripts, automation and AI assistants (an MCP server could wrap it;
none is included yet). Same options and commands as above except `scan` and `exec`, always JSON. For a non-read it
offers only `prepare`:

```
$ kline-agent prepare --ecu sirius 11FF
{"event":"prepare","request":"11FF","risk":{"tier":"service (tier 1)",...},
 "command":"kline exec --ecu sirius 11FF","note":"only a human can run this, ..."}
```

`read` of a non-read is exit 7 and nothing reaches the ECU (`crates/kline-agent/tests/agent.rs`
checks the simulator's write log).
