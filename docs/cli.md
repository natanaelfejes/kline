# kline command line reference

This page documents the `kline` and `kline-agent` commands: options, ECU selection, every
command, the approval flow for service functions, exit codes, capture files, and a step-by-step
first test at the car. Run `kline --help` or `kline <command> --help` for the short version.

## Contents

- [Building kline and kline-agent](#building-kline-and-kline-agent)
- [Installing on the Mac (binary and definitions)](#installing-on-the-mac-binary-and-definitions)
- [Common options](#common-options)
- [Selecting the ECU](#selecting-the-ecu)
- [Commands](#commands)
- [exec: sending a service request with approval](#exec-sending-a-service-request-with-approval)
- [Name guard](#name-guard)
- [Exit codes](#exit-codes)
- [Capture files](#capture-files)
- [Running against the simulator](#running-against-the-simulator)
- [First test on the car](#first-test-on-the-car)
- [ABS (Bosch 5.3) on the car](#abs-bosch-53-on-the-car)
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

Which build is it: `kline --version` and `kline-agent --version` print the git commit the binary
was built from, e.g. `kline 0.1.0 (5e0d5ff)`, with `-dirty` when the work tree had uncommitted
or untracked changes, or `(unknown)` when it was built without git (a build script,
`crates/kline-host/build_git.rs`; `KLINE_GIT_COMMIT=...` in the build environment overrides it).
Every capture header and the first trace line (`BUILD kline 0.1.0 commit 5e0d5ff`) record the
same value. Build the car binary from a clean checkout so it does not say `-dirty`.

## Installing on the Mac (binary and definitions)

kline needs two things on the machine: the binary in `PATH`, and the DDT2000 definition files in
the definitions directory that `--ecu` reads (default `~/.local/share/kline/ecus`;
`%APPDATA%\kline\ecus` on Windows). The definitions are Renault data: they never go into this
repository (SAFETY.md rule 1).

```
cargo build --release -p kline-cli
mkdir -p ~/.local/bin && cp target/release/kline ~/.local/bin/    # or ~/bin; must be in PATH
kline setup --defs-from ~/Documents/Clio/ddt-extract/ecus         # copies *.xml, once
```

`kline setup --defs-from DIR` copies every `*.xml` file of `DIR` into the definitions
directory (`--defs` if given, else `$KLINE_DDT_XML`, else the default), refuses a destination
inside a git work tree (exit 8; symlinks are resolved, and if the default directory itself is
in a repository, e.g. one at your home folder, the message says to pick a folder outside it
with `--defs` or `KLINE_DDT_XML`), opens no port, and lists which `--ecu` files are now present.
Without it, a missing definition is exit 8 with the fix in the message
(`kline setup --defs-from DIR`, `--defs DIR` or `KLINE_DDT_XML=DIR`, and the default path).
If `~/.local/bin` is not in `PATH`: `export PATH="$HOME/.local/bin:$PATH"` (add it to `~/.zshrc`).

## Common options

| Option | Meaning |
|---|---|
| `--json` | one JSON object per line on stdout (errors too: `{"event":"error","code":N,"kind":..,"message":..}`). Notes go to stderr. `kline-agent` always uses JSON |
| `--trace` | every action with a time stamp on stderr: DTR, break edges, sleeps, break loopback, flushes (with the bytes discarded), every TX frame and every RX read (`RX got/wanted in N ms`). Also saved as `<capture>.trace.txt` |
| `--port PATH` | serial port. Default: the single `/dev/cu.usbserial-*` (macOS AppleUSBFTDI), FTDI `/dev/ttyUSB*` (Linux) or FTDI `COMn` (Windows; `COM10` and above work as typed); none or several is exit 3. A macOS callin device `/dev/tty.*` is refused (exit 3; it blocks on carrier detect): use its `/dev/cu.*` twin |
| `--sim-control SOCKET` | `--port` is then kline-sim's pty, break/DTR go over this control socket ([sim.md](sim.md#pty-transport-how-break-and-dtr-are-represented)) |
| `--rts leave\|low` | RTS after the port is opened, before the first init (also `selftest`). `leave` (default): as the OS opens the port (usually asserted on POSIX). `low`: DTR high, then RTS low, once, like DDT2000's IOTBDSetup; traced as `PORT dtr=1 rts=0` (`leave`: `PORT dtr=1 rts=left as opened`). Whether the KKL cable uses RTS is NOT VERIFIED: compare `kline selftest --echo-rtt --rts low` and `--rts leave` on a bench with a looped K line before changing the default |
| `--kw-window MS` | read window for the `55 KW1 KW2` and ~address reads of a 5-baud init, 100..=2000 (else exit 2); default: the definition's KW2Timeout + 100 ms (DDT2000; 400 ms for the car's multipoint ECUs). Reading starts in the middle of the 100 ms stop-bit wait, so the budget after the stop bit is about MS - 100 ms: short for an ECU with W1 near the ISO 9141 maximum of 300 ms plus W2/W3 and macOS FTDI latency (~16 ms). **If connect fails with no keywords received, retry with `--kw-window 700`** (the error says so). The window is traced with the effective W4, P3 and inter-byte time (`INIT 5-baud address byte 7A, keyword window 400 ms, W4 25 ms, P3 5 ms, inter-byte 0 ms`; a value set by an option is marked, e.g. `P3 55 ms (--p3)`). NOT VERIFIED on the car whether any ECU needs it |
| `--p3 MS` | P3, the minimum gap between the end of a reply and the next request, 0..=1000 (else exit 2); default: the definition's `ReplyToRequest` (5 ms on the car's ECUs; ISO 14230-2 P3min is 55). Overrides the definition and Multipoint. It does not change the definition's FirstRequest delay after the init (20 ms). Try `--p3 55` when the init and the first request work and the next request is not answered (final critique F09). NOT VERIFIED on the car |
| `--inter-byte MS` | tester inter-byte time (ISO 14230-2 P4), 0..=20 (else exit 2); default: the definition's `InterByte` (0: a frame in one write; Multipoint 7). Overrides the definition and Multipoint; the TX trace shows it (`(inter-byte 5 ms)`) |
| `--w4 MS` | W4, the wait after the 5-baud key bytes before ~KW2, 25..=50 (ISO 14230-2; else exit 2); default: the definition's KW2 delay (25) |
| `--dtr high\|low\|leave` | DTR after the port is opened, before the first init (also `selftest`, to compare the loopback with DTR high and low, final critique F15). `high` (default, unchanged behaviour), `low`, or `leave` it as the OS opened the port; traced as `PORT dtr=1`, `dtr=0` or `dtr=left as opened`. The inits drive DTR themselves afterwards (low during the 5-baud bits, high before fast init). Whether the KKL cable uses DTR is NOT VERIFIED |
| `--capture-dir DIR` (alias `--captures`) | default `$KLINE_CAPTURE_DIR`, else `~/.local/share/kline/captures` (Windows `%APPDATA%\kline\captures`) |
| `--defs DIR` | definitions for `--ecu`. Default `$KLINE_DDT_XML` (deprecated fallback `$CLIO_DDT_XML`, read for one release), else `~/.local/share/kline/ecus` |
| `--transport serial\|ble` | `serial` (default): the KKL cable or the simulator pty. `ble`: `read` through the Clio Link bridge, Tier 0 only, decoded with the definition as usual, JSON events with `"transport": "ble"`; only the injection (0x7A) and the BMT (0x26); every other command is exit 2. Needs a build with the cargo feature `ble` (else exit 2) and a BLE radio backend, which is not in this build yet (exit 3). [bridge-protocol.md](bridge-protocol.md#host-transport-kline---transport-ble) |

## Selecting the ECU

These options apply to every command that talks to an ECU, and to `risk`, `prepare` and `replay`.

| Option | Meaning |
|---|---|
| `--ecu sirius` | `SIRIUS32___EMS3132___Soft_RE1_maj.xml` (0x7A) |
| `--ecu bmt` | `BMT_X65_ph1_p.xml` (0x26); `BMT_SAGEM.xml` is an extra candidate if present: the definition choice stays unresolved, also when only ph1_p is present, so config writes are refused ([Choosing the BMT definition](#choosing-the-bmt-definition)) |
| `--ecu bmt-ph1p` | `BMT_X65_ph1_p.xml` only: the BMT definition choice made (config writes need one) |
| `--ecu bmt-sagem` | `BMT_SAGEM.xml` only |
| `--xml PATH` | any DDT2000 ECU definition; repeat for `ident` candidates (the first one sets up the link) |
| `--address 0xNN` | overrides the definition's `<Function Address>` (also the safety target) |
| `--vehicle PATH` | vehicle addressing file (e.g. `vehicles/x65_addressing.xml`): function name, protocols, tool address |
| `--variant auto\|single\|multi\|fast` | init variant ([protocol.md](protocol.md#init-variants-single-ecu-multipoint-fast-init)). `auto` tries FastInit, SingleEcu (DDT2000's detection order), then MultiEcu, skips variants that would run the identical init, and logs each attempt (`init_attempt` events with `round`). In human output a failed attempt that another one follows is information, not an error: `init fast   fast init not answered (normal for many Renault K-line ECUs), trying 5-baud byte 7A`; the trace has `INIT attempt fast (round 1) failed: no reply from ECU` |
| `--init-attempts N` (global: before or after the command; ignored by commands that open no session: ports, setup, selftest, risk, replay; printed in the exact command of `risk`/`prepare`) | rounds over the variants, 500 ms apart (default 3, DDT2000; after an attempt the ECU answered but that failed, the next one waits 5000 ms, P3max, [protocol.md](protocol.md#detection-order---variant-auto)). For an ECU that does not answer at all, `--init-attempts 1` gives up after one round (about 6 s instead of 19 s) |
| `--nrc-tries N` | `ident`: tries of a request answered `7F xx 21`/`23` (default 10; 200/500 ms) |
| `--periodic-tester-present` | `3E` every 2000 ms with a required reply (DDT2000 screen option) instead of after 3000 ms idle |
| `--strict-loopback` | 5-baud: abort with "K line inactive" when RX shows nothing during the start-bit break, like DDT2000 (which waits for the Windows `EV_BREAK` event). Default: a `warning` event (`break_loopback_not_seen`) and the init goes on, because a POSIX tty may deliver no byte for a break ([protocol.md](protocol.md#5-baud-init-iso-9141-2-slow-init)) |
| `--session auto\|none\|a0\|81` | after the init: `auto` sends `10 A0` if the definition has it, else `10 81` if it has that (KWP only). A negative answer is reported (`note: not retried with another session`) and the command continues; no other session is tried (e.g. `10 C0`, seen in other Renault 0x7A logs, is an Expert request only) |

Protocol and timing come from the definition's `<K>` section through kline-core (`spec::k_timing`):
ISO8 5-baud, KWP 5-baud (with ~KW2 handshake), KWP fast init. A definition with only `<CAN>`
is refused: "CAN not supported by a KKL cable yet" (exit 8).

### Choosing the BMT definition

The car's BMT (0x26) has two DDT2000 definitions, `BMT_X65_ph1_p.xml` and `BMT_SAGEM.xml`. With
`--ecu bmt` both are loaded: `ident` says `AMBIGUOUS` and lists the bits in which they differ,
and `read`/`live` print `definition choice unresolved: ...; values below decoded with ...`
plus both decodings of every item that differs. That is fine for reads. A config write
(`exec 3B D2 ..`) is refused (exit 7, before anything is sent) until one definition is named:
`--ecu bmt-ph1p`, `--ecu bmt-sagem` or `--xml PATH`. This holds also when only one of the two
files is in the definitions directory: `--ecu bmt` never counts as a choice.

kline cannot make that choice from `21 80`, and neither could DDT2000: both files declare the
same `21 80` layout (same items, positions and data definitions: version, calibration,
supplier, hardware, serial, type), the same address and `<K>` section, and no `<AutoIdents>`;
they differ there only in DDT2000's access profiles. Neither file holds a value the ECU's
identification could be compared with, so no rule can be derived from the XMLs (checked
2026-11-22 against the owner's copies). **The owner decides**, at home, from the raw
`21 A1` line (it is in the capture and in `<capture>.out.txt`):

| `21 A1` reply (byte 1 = `61`) | `BMT_X65_ph1_p.xml` | `BMT_SAGEM.xml` |
|---|---|---|
| byte 3, mask `0x80` | `CARCLIENT` | `CONDROUL` |
| byte 4, mask `0x01` | `CONDROUL` | (not defined) |
| byte 4, mask `0x02` | `ALARMELEDCPE` | (not defined) |
| `3B D2` index 7 (write) | `CARCLIENT` | `CONDROUL` |
| `3B D2` index 8 / 9 (write) | `CONDROUL` / `ALARMELEDDCPE` | reserved |

`TRF` (byte 3, mask `0x08`, `3B D2` index 3) is the same in both, so the RF/infrared answer
does not depend on the choice. Compare the bits with what the car does. `CONDROUL` most likely
means "condamnation en roulant", doors lock by themselves when the car starts moving (inferred
from the name and from `CONDAROUL` "lock while driving" in `21 A3`; NOT VERIFIED). If byte 3
bit `0x80` is set and the car locks while driving, SAGEM fits; if it is set and the car does
not, ph1_p (`CARCLIENT`) fits. If the bit is 0, these bits do not decide. When unsure, write
nothing.

## Commands

| Command | Does | Notes |
|---|---|---|
| `ports` | lists ports, marks candidates `*`, says which cable would be used; a macOS `/dev/tty.usbserial-*` whose `/dev/cu.*` twin is listed is marked `(not used, use /dev/cu.usbserial-..)` (JSON `use_instead`) | |
| `setup --defs-from DIR` | copies `DIR/*.xml` into the definitions directory ([above](#installing-on-the-mac-binary-and-definitions)) | `kline` only; opens no port |
| `selftest [--echo-rtt] [--set-latency MS]` | without the car: first measures the host's sleep precision with no port open (`selftest_timing`: 10 sleeps each of 1, 25 and 200 ms, late min/mean/max and early count against +-1 ms, +-1 ms and +-10 ms; [platforms.md](platforms.md#timing-precision)), so it reports even without a cable (then exit 3). Then opens the port at 10400, shows the USB id and the baud the driver reports (`baud_reported_source`: on macOS the termios output speed read back with `tcgetattr` after `IOSSIOSPEED`, the tty layer's record, not the FTDI divisor), toggles break 20 times (10 ms half-period, nothing written) and reports the edges against the 5-baud tolerance (+-10 ms; each edge against its ideal time, which includes the break ioctl once) and the pulse lengths against the fast-init TiniL tolerance (+-1 ms; end of one break call to the end of the next, so a constant ioctl cost cancels and only its variation and late wake-ups count, re-check R06; human line `pulse length max deviation`, JSON `max_pulse_dev_us`) (on a port without a USB id, i.e. `--sim-control`, a pty or a built-in port, it says `this port is not a known FTDI KKL cable (...): timing verdicts below are not meaningful`, JSON `kkl_cable`/`timing_meaningful` false, and blames no USB latency; a USB adapter of another make, e.g. a CH340 or PL2303 KKL cable, gets `this port is not a known FTDI KKL cable (usb vid:pid): the verdicts below are for this host and adapter ...`, `kkl_cable` false, `timing_meaningful` true and keeps `fast_init_cause`); when TiniL fails, `fast_init_cause` says whether the break call itself was slow (USB round trip, e.g. VM passthrough) or the sleeps woke late. `--echo-rtt` also writes a single `FF` byte 10 times and times its echo (USB latency, `kw2_handshake_ok` when every byte is back in < 25 ms; a write the driver does not take stops it with `write_error` and exit 0); it needs a looped-back K line (cable on the car's OBD socket, no init sent, or TX wired to RX). Linux: also reports the ttyUSB port's FTDI `latency_timer`; `--set-latency 1` writes it first (root or udev rule, [platforms.md](platforms.md#ftdi-latency-timer); elsewhere exit 2) | `kline` only; without `--echo-rtt` unplug the cable from the car |
| `connect` | init (variants), session start, Stop | reports sync, KW1 KW2, ~address, key-byte match; a sync byte other than 55 is only a warning (`sync byte XX, expected 55 (baud or line problem?)`, JSON event `warning` kind `sync_byte`, `connected.sync_ok` false; DDT2000 does not check it either) |
| `ident` | `21 80` (KWP; else Tier 0 requests named like "ident"; NRC 21/23 retried like DDT2000), chooses the definition | DDT2000 AutoIdent ladder (exact, unique, soft, diag, soft+diag; DiagVersion numeric, Supplier/Soft/Version whitespace-normalized, case-sensitive) when the candidates have `<AutoIdents>`; else the first candidate whose key bytes match and whose identification requests decode. A reply shorter than MinBytes is decoded anyway and flagged; `ddt2000_diagfailures_accepts` says whether DiagFailures (exactly 26 bytes) would take it |
| `read REQ [REQ ..] [--set ITEM=VALUE ..]` | one or more requests (definition name or hex) read in order in ONE session, each decoded (every one is checked as a Tier 0 read before the port is opened; a negative or undecodable reply does not stop the others, the exit code is the first such failure; a line error (no reply, bad frame after the retry, port) ends the rest, is the exit code instead (5) and replaces an earlier 6/10; JSON `read_error` {request, kind, message} per failed request when several were given, and `read_skipped` {requests, reason: `line error` or `Ctrl-C`} naming the requests not sent; Ctrl-C between requests stops the rest (Stop is sent, exit 0 unless an earlier request failed); `--set` only with a single request). A reply with a bad checksum or cut short is retried once after 400 ms, visibly (`21 A3: bad reply frame: checksum mismatch ..; retrying once in 400 ms`, JSON `read_retry`); a missing reply or a negative answer is not retried. Known BMT flags carry a one-line meaning in human output (`TRF  1  (1 = RF remote (inferred, NOT VERIFIED))`, `TIR RF RECU`, `TIR RF VAL`, `TIR RF OK`, `TRP RECU`, `TRP VAL`, `TRPOK`, `INTCLE`, `VERLOGI`; table `FLAG_MEANINGS` in `crates/kline-host/src/defs.rs`, also used by `live`, `ident` and `replay`) | Tier 0 only; a non-read is refused before the port is opened (exit 7) and again by kline-core. `--set` (also on `risk`, `exec`, `prepare`): builds the named request from its `<Sent>` data items (`Request::build`); VALUE is a number in physical units (must lie on the item's scaling grid), a list label, or `hex:..`; unknown items and off-grid values are exit 8 before anything is opened |
| `live --frames A,B --rate HZ [--cycles N] [--duration SECS] [--items NAME,..] [--compact]` | all frames once per cycle, `HZ` cycles/s. `--items` shows only those decoded items (names case-insensitive, also in JSON; an unknown name is exit 8 before the port is opened). `--cycles N` (old name `--count`, never an item filter) stops after N cycles, `--duration SECS` after that many seconds, whichever comes first, both with a clean Stop; if no cycle got a positive answer before they end, exit 5 (`ECU rejected every request (7F ..)` when every answer was negative, else `no communication with the ECU`); this also holds for `--count`/`--cycles N`, which used to exit 0 with no output. `--compact` prints one line per cycle, `#3 t=4512 ms Engine speed=850 rpm; ...`: only the values that changed since the previous cycle (all in the first), with or without `--items`, else `(no change)`; human output only | TesterPresent when idle >= TesterPresent delay (or every 2000 ms with `--periodic-tester-present`); Ctrl-C stops cleanly (`stopped by Ctrl-C, Stop sent`; JSON `stopped` has `ctrl_c`); a cycle in which every request failed is retried after 400 ms, the 4th in a row is exit 5 (DDT2000 "Read Data : Retry n/3"): `ECU rejected every request (7F 21 12 subFunctionNotSupported-invalidFormat) in 4 cycles` when every answer was negative, else `no communication with the ECU` |
| `dtc [--show-clear]` | `<Failures Id="1">` (Sirius 32): reads the Tier 0 frames that carry `FailureFlag="1"` items of `<Devices>` and lists the set flags with their device (DDT2000 DiagFailures); otherwise the frames whose name says `panne`/`defaut`/`fault`/`dtc`, non-zero items; otherwise (ISO8 Safir: `Failures Id="0"`, which DDT2000 shows as no fault list) a `dtc_note` saying so and the non-zero fault-named items of its Tier 0 reads (Safir `33 50`/`33 51`/`33 52` "Panne presente ..."). A frame laid out in numbered slots (at least two fault-named items `... 1`, `... 2` each with other items of the same number: Bosch 5.3 ABS `58`) counts one fault per set slot and lists the slot's other items with it (`context`). Each frame and the summary say `N set (present: a, memorized: b)` (by item name; `, other: c` for the rest; just `N set` when no name says present or memorized). `--show-clear` adds the clear plan (what `kline clear-dtc` would send); JSON `dtc_summary` always has `clear_plan`, plus `present`/`memorized` | never clears |
| `clear-dtc [--ddt2000-clear]` | Tier 1 clear: prints the exact bytes (default: `ClearDiagnosticInformation.*` if the definition has one, else the definition's own `14 ..` request; Sirius `14 00 00`) and what DDT2000's global clear would send (`14 FF 00` for the Sirius), then the `exec` approval | `kline` only; `--ddt2000-clear` sends DDT2000's choice |
| `scan --range 21:B0-C5 [--interval MS]` | kline-core `ScanPlan`: `21`/`1A` only, >= 200 ms apart, stops after 3 missing replies or a security NRC (exit 12) | `kline` only; not offered by kline-agent |
| `risk REQ [--hold S --watch REQ]` | tier and `RiskInfo` of a request, nothing sent; for an allowed non-read also the exact `kline exec` command (`command:`, JSON `command`) with the given `--hold`/`--watch`; for the BMT remote learning without `--hold` it suggests `--hold 30 --watch 21A3` | exit 7 when forbidden, refused by the name guard, or `--watch` is not a read |
| `exec REQ [--set ..] [--stop REQ [--stop-set ..]] [--hold S [--watch REQ [--watch-interval MS]]]` | ONE non-read request; `--stop` offers a second request afterwards (end of an actuator test) with its own `RiskInfo` and code (not on the ISO8 unit at `01`, ABS: refused before the port is opened, one approved request per session there); `--hold` keeps the session open after a positive reply, `--watch` reads one frame meanwhile | `kline` only, see below |
| `replay FILE [--ecu/--xml]` | decodes a capture offline: wake-ups, each request with its replies (echo stripped, 0x78 sequences split), decoded with the definition if given | |
| `prepare REQ` | `kline-agent` only: `RiskInfo` plus the exact `kline ... exec ...` command for a human | never sends |

## exec: sending a service request with approval

`exec` refuses, before opening anything, when `--json` is given or stdin/stdout is not a
terminal (exit 7). Otherwise it shows the request, its definition names and its `RiskInfo`, opens
the session, and for Tier 2 (config write) first reads `21 80` (the definition must be the one
`ident` chooses, unambiguously) and the backup read (`21 A1` for BMT `3B D2`, else `21 lid`).
It then makes the kline-core challenge, shows it on `/dev/tty` with a random code (6
characters, 10 for expert tier) and reads the answer from `/dev/tty` (not stdin), sending
TesterPresent while it waits (default 120 s, `--confirm-timeout`). A mistyped code voids that
challenge and gives one more attempt with a new code for the same request (nothing is sent in
between, the session stays open; Neon critique F1); a second wrong code, a timeout or Ctrl-C is
exit 9 and nothing is sent. This holds for the request and for its `--stop` request. Definition names on these screens are printed sanitized
(control characters, escape sequences and bidi overrides shown as `\u{..}`).

`--stop REQ` (e.g. an actuator test's stop payload): checked before anything is opened (a refused
stop refuses the whole command); after the request was sent, whatever its reply, kline shows a
second challenge for the stop request with its own code, keeps the session alive, and sends the
stop when that code is typed. If it is not typed in time the stop is NOT sent (said on stdout);
the session then ends with StopCommunication, which on most ECUs also ends an output test
(NOT VERIFIED on this car). One challenge per session at a time (kline-core), so the stop cannot
be approved before the request is sent. A Tier 2 (config write) stop request needs the session's `21 80`
identification and its backup read like any config write: both are read before the request is
sent, and if they fail the whole command is refused with nothing sent (Neon critique F1).

`--hold SECONDS` (1 to 60; final critique F01): without it the session ends with
StopCommunication a few ms after the reply, which would end a learning window the request
opened (BMT `31C7000000`/`31C8000000`: about 10 s). With it, after a **positive** reply kline
keeps the session open for SECONDS, sending only TesterPresent when the line has been idle for
the definition's TesterPresent delay (3000 ms on the BMT), then Stop. `--watch REQ` (a Tier 0
read, name or hex, e.g. `21A3`: only `21 xx` or `1A xx` of exactly 2 bytes, `17` or `18`;
a non-read is refused before the port is opened, exit 7, and the Tier 0 frames that are not
reads, `10 xx` and `3E`, exit 2 (re-check R13); kline-core checks it again on every send) makes that read the only request while holding,
every `--watch-interval` ms (default 500, at least the definition's P3 gap, else exit 2). The
first read prints every item with the known flag meanings, later reads only the items that
changed, e.g. `+2000 ms 21 A3  TIR RF RECU 0 -> 1  (1 = a remote frame was received)` (JSON
`watch` with `changed`). A failed watch read is printed and the hold goes on. Ctrl-C or SIGTERM
ends the hold early: Stop is sent and K released (`stopped by Ctrl-C, Stop sent`; a second
Ctrl-C releases K at once). The approved request is sent exactly once: never again while
holding. After a negative reply there is no hold. After a lost, damaged or late reply (exit 13
below) the hold still runs for SECONDS with only the watch read and TesterPresent, because the
ECU may have run the request and opened its window (re-check R11); the exit code stays 13. `--hold` cannot be
combined with `--stop`. `kline risk ... --hold 30 --watch 21A3` prints the exact command.

When the approved request **was written** to the line but its reply is lost, damaged or timed
out (final critique F06), kline says so instead of a plain `no reply from ECU`:
`the request 31 C7 00 00 00 WAS SENT to the ECU; its reply was not received correctly (no reply
from ECU); do NOT repeat it blindly: first read the state (e.g. kline read --ecu bmt 21A3)`,
exit 13 (JSON event `approved_reply_lost` with `request_sent: true`; in the capture's
`.out.txt`). The ECU may have run it: read the state first. With `--stop` the stop is still
offered; with `--hold` the hold (and `--watch`) still runs, then Stop. A request that never reached the line (wrong code, port error before the write) is
not reported as sent.

The code is an anti-accident gate: an automated agent that drives a pseudo-terminal can read and
type it, and the owner permits that when he chooses. It is not a defence against such an agent.

## Name guard

Name guard (kline-core, all ECUs, Tier 1-3; `Session` applies it with the definition the CLI
loaded, `kline_core::spec::definition_guard`; with several candidate definitions, the union of
their guards): a request is refused (exit 7) when a definition request with those bytes, or one
of its sent or received data items (name, description, comment, mnemonic), contains an
immobilizer/transponder/key keyword (`kline_core::safety::immobilizer_keyword`: anti-demarrage,
transpond, immo, verlog, verrou, apptrp, ap1cle, code cle, key code, wegfahr, code manuel,
schlussel/schluessel, and the words cle/cles/clef/clefs/key/keys; accents are folded, so "cléf"
and "Schlüssel" match). The exact BMT remote-learning payloads `31C7000000`/`31C8000000` on a
BMT/UCH/decoder (0x26/0xA6 or a BMT/UCH/decoder definition) are exempt (their names contain
"CLE"). "verrou" also matches central locking; that is refused too.

What "those bytes" are: the request's fixed sent bytes up to its first sent data item, when
that is at least two bytes; otherwise its whole SentBytes template (e.g. BMT "LANCEMENT DES
ROUTINES" `3100000000`, whose "MSB/LSB Code manuel" items hit "code manuel", is refused only
as `31 00 00 00 00 ..`, not every `31`). A service byte alone never flags anything, so a
definition whose template is one byte cannot be flagged by name (the byte rules still apply).

An enumerated sent parameter counts as well: when a text of its `<List>` has such a keyword,
the request with that value is refused (fixed bytes plus the value byte; the request as above
when the parameter is not one whole byte), e.g. BMT actuator outputs `A6` "Ligne Transpondeur"
and `AB` "Led Verlog"; the other outputs stay available.

A request flagged by a received item is a read whose reply holds an immobilizer field: the
exact read (`21 xx`, 2 bytes) stays Tier 0, but the same bytes with anything after them are
refused, not Expert. This is intended: a `21 xx` with extra bytes can write (BMT `21 A1` with a
third byte writes CALTERMI), so an unknown variant of a flagged read is not offered
(security review L2, kept).

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
| 9 | approval not given: wrong code twice, timeout, terminal closed (nothing sent) |
| 10 | reply does not decode with the definition |
| 11 | capture/replay file problem |
| 12 | scan stopped early (3 missing replies or a security NRC) |
| 13 | an approved request WAS SENT but its reply was lost, damaged or timed out: read the state, do not repeat it blindly |

## Capture files

Every session writes `<UTC>-<ecu>-<command>-<pid>.jsonl` in the capture directory: first a
header line that names the build (tool, version, git commit: short hash, `-dirty` when the
work tree had changes, `unknown` without git; re-check R10, final critique F11), then one line
per TX/RX, flushed as written, in the kline-sim replay format
([sim.md](sim.md#replay-log-jsonl)) plus `unix_ms`. `replay` and kline-sim skip the header:

```
{"header":{"tool":"kline","version":"0.1.0","commit":"5e0d5ff","command":"read","label":"sirius","unix_ms":1795342272000}}
{"t_ms":300,"dir":"tx","kind":"five_baud","hex":"7A","unix_ms":1795342272000}
{"t_ms":2383,"dir":"rx","hex":"55D08F","unix_ms":...}
{"t_ms":2408,"dir":"tx","hex":"70","unix_ms":...}
```

Next to it, `<capture>.out.txt` holds what the command printed for a human: the init lines,
`connected:`, the chosen definition, decoded values, notes, errors and the final `done (exit N)`
(also with `--json`, where it holds the human text of the same events; the trace stays in
`<capture>.trace.txt`). No `script` session is needed to keep the decoded values. The
`capture` JSON event names it in `out`.

In human mode every command ends with `done (exit N)` on stderr. With `--json` stderr gets no
such line and stdout stays pure JSON (the error is an `error` event), but `<capture>.out.txt`
still has the error text and ends with `done (exit N)`.

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

This is the planned first hardware-in-the-loop session, reads only. Nothing here has been run on
the car yet. Do the steps in order: the engine ECU (Sirius, 0x7A) first, then the BMT (0x26);
`scan` is optional and comes last. Do **not** run `exec` or `clear-dtc` on the first visit.
The same order is in the owner's record (`PROJECT-RECORD/NEXT-STEPS.md` step 10 and
`REHEARSAL.md`).

Every command keeps its capture (`<capture>.jsonl`), the trace with `--trace`
(`.trace.txt`) and what it printed (`.out.txt`, decoded values included) in the capture
directory (`--capture-dir`, `$KLINE_CAPTURE_DIR`, default `~/.local/share/kline/captures`), and
ends with `done (exit N)` on stderr. No `script` log or `echo $?` is needed.

Before: kline installed and the definitions copied once with `kline setup --defs-from DIR`
([Installing](#installing-on-the-mac-binary-and-definitions)); cable in the OBD socket and the
Mac; ignition ON, engine off; nothing else (DDT2000, another tester) on the line.

0. At home, cable in the Mac only: `kline selftest`. Expect `5-baud bits (+-10 ms, edges): OK`; if the
   fast-init line says `TOO IMPRECISE`, fast init may fail (the auto order tries it first, then
   5-baud).
1. `kline ports`
   - OK: a line `* /dev/cu.usbserial-XXXX  usb 0403:6001 ...` and `cable: /dev/cu.usbserial-XXXX`.
     The `/dev/tty.usbserial-XXXX` line says `(not used, ...)`: ignore it.
   - Fail: `no KKL cable found` (exit 3): replug, check `ls /dev/cu.*`; several cables: pass `--port`.
2. First contact, init only: `kline --trace connect --ecu sirius --session none`. `connect`
   normally opens a diagnostic session after the init (`--session auto`: `10 A0` with the Sirius
   definition, then the Stop); with `--session none` the only frames after the init are the
   init's own and the StopCommunication, so the very first contact changes nothing in the ECU
   (final critique F13). Expect the same init lines as below, `connected: sirius at 7A ...`,
   no `session 10 A0` line, then the Stop. Then run the normal connect:
   `kline --trace connect --ecu sirius`
   - OK (exit 0): `init fast   fast init not answered (normal for many Renault K-line ECUs),
     trying 5-baud byte 7A` (information, not an error), `BREAK LOOPBACK seen`, about 2 s of
     `BREAK on/off` + `SLEEP 200 ms`, `RX 3/3 ...: 55 D0 8F`, `TX 70`, `RX 2/2 ...: 70 85`,
     `connected: sirius at 7A (kwp, kwp 5-baud) with variant single; sync 55 KW D0 8F, ~address 85`,
     `session 10 A0: ok 50A0`, then the Stop. If fast init answers instead:
     `connected: ... with variant fast`. Note which variant worked.
   - `BREAK LOOPBACK NOT seen` / `K line inactive` (exit 4): the break does not reach RX. Check
     ignition, the cable's K line (OBD pin 7). How the FTDI reports a looped break under macOS is
     NOT VERIFIED: send the trace.
   - No answer at all (exit 4 after about 19 s: 3 rounds of fast, single, multi): check ignition
     and the plug, wait 10 s, then retry once with one round only:
     `kline --trace connect --ecu sirius --init-attempts 1` (one round of fast, single and multi:
     about 6 s instead of 19 s).
   - `RX 1/3` or `2/3`, or key bytes other than `D0 8F`: keep the capture, stop, report it.
   - Only if 5-baud on 7A got no keywords: `--variant multi` (`TX FA`, `~address 05`) or
     `--variant fast` (`TX 81 7A F1 81 6D`, expects `C1 ..`).
3. `kline ident --ecu sirius`: `21 80 -> 61 80 ...` and `definition: ...RE1_maj.xml (DDT2000
   AutoIdent: ...)`. `note: 24 bytes; DDT2000's DiagFailures identification needs exactly 26`
   is only about DDT2000's fault screen. `NONE fits` (exit 10): keep the capture, report it.
4. `kline live --ecu sirius --frames 21A9,21AA --rate 2` (engine off: rpm 0, coolant about the
   outside temperature, battery 12.0 to 12.8 V), then start the engine and watch rpm
   (800 to 900) and voltage (13.5 to 14.7 V). `--items "Régime moteur,Tension batterie mesurée"
   --compact` gives one line per cycle. Ctrl-C ends it: `stopped by Ctrl-C, Stop sent`.
5. `kline dtc --ecu sirius`: `N set (present: a, memorized: b)` per frame (exit 0 even when
   some are set). Clear nothing today.
6. `kline --trace connect --ecu bmt`
   - OK: as step 2 with `byte 26`, `RX 2/2 ...: 70 D9`,
     `connected: bmt at 26 (kwp, kwp 5-baud) with variant single; ... ~address D9`; no
     `session` line (the BMT definition has no `10 A0`).
   - No keywords on 26 but `init multi  byte A6: ok` (`~address 59`) is also fine.
   - Failures as in step 2 (`--variant multi`: `TX A6`; `--variant fast`: `TX 81 26 F1 81 19`).
7. `kline ident --ecu bmt`: `AMBIGUOUS` and `definition choice unresolved: BMT_X65_ph1_p,
   BMT_SAGEM ...` are expected (both definitions decode `21 80`): fine for reads; the note lists
   the bits in which they differ. The choice is made later, at home
   ([Choosing the BMT definition](#choosing-the-bmt-definition)). Keep the `21 80 -> ...` line.
8. BMT reads, each about 3.5 s:
   - `kline read --ecu bmt 21A1` (configuration): `TRF  1  (1 = RF remote (inferred, NOT VERIFIED))`
     is expected. The polarity (1 = RF) is inferred from the Technical Note, not confirmed: if
     the RF remote works with `TRF 0`, the polarity is the other way round. Only if the remote is
     dead and `TRF 0` would the inference point at the TRF option; do not write it before the
     polarity is confirmed. Keep the raw `21 A1 -> ...` line (the
     backup before any later write); the `decoded differently by the candidates` lines show the
     SAGEM reading of the same bytes.
   - `kline read --ecu bmt 21A3`, then `kline live --ecu bmt --frames 21A3 --rate 2 --compact`
     while pressing the remote 3 times, 3 s apart, 1 to 2 m from the car: `TIR RF RECU`
     (`1 = a remote frame was received`), `TIR RF VAL`, `TIR RF OK` should change. Ctrl-C.
   - `kline read --ecu bmt 21A4` (transponder status only, key in, ignition ON): `INTCLE`,
     `TRP RECU`, `TRP VAL`, `TRPOK`, `VERLOGI` with their meanings.
9. `kline dtc --ecu bmt`.
10. Optional, last, only if everything above passed: `kline scan --ecu sirius --range 21:B0-BF`
    (about 8 s; `21` reads only, 200 ms apart). `negative 12` is normal (no such frame); exit 12
    means it stopped by itself (3 missing replies or a security NRC): nothing happened, note it.
11. Offline: `kline replay <capture>.jsonl --ecu sirius` should show the same values. Send the
    capture directory for the [protocol.md "Not verified"](protocol.md#not-verified-needs-the-car) list.

## Remote learning on the car (BMT)

Only after the reads above (step 8: `21 A1`, `21 A3` while pressing the remote) and when the
owner decides to try it (record: `07-car-session/remote-pairing-plan.md` step 5). Ignition off
(Technical Note), doors closed, remote 1 with a good battery in the hand, near the centre
console. Each command shows its RiskInfo and a code; type the code only when ready to press the
remote.

1. `kline read --ecu bmt 21A3`: note `CODIRAPP1`, `CODIRAPP2`, `APPTIR`.
2. Remote 1 (erases all remotes, opens the learning window):
   `kline exec --ecu bmt 31C7000000 --hold 30 --watch 21A3`
   - After `sent 31 C7 00 00 00: positive reply 71 C7 ...` and `holding the session for 30 s`,
     press remote 1 once (within about 10 s; if nothing changes, press it once more).
   - Success looks like `TIR RF RECU 0 -> 1`, `TIR RF VAL 0 -> 1` and `TIR RF OK 0 -> 1` lines
     (frame received, valid, accepted), the doors lock and unlock, the red LED goes out;
     `CODIRAPP1 0 -> 1` and `APPTIR` back to 0 are expected (inferred from the names, NOT
     VERIFIED). `RECU` 1 but `VAL`/`OK` 0: the frame arrives but is not accepted. `RECU` stays
     0: the BMT hears nothing (receiver, supply, TRF), learning will not help.
   - Exit 13 (`WAS SENT ... do NOT repeat it blindly`): first `kline read --ecu bmt 21A3`,
     then decide.
   - `7F 31 ..` (exit 6): nothing to repeat, note the NRC and stop.
3. Remote 2, only with remote 1 learned (`CODIRAPP1 1`, or a working remote 1):
   `kline exec --ecu bmt 31C8000000 --hold 30 --watch 21A3`, press remote 2 when the hold
   starts; success as above with `CODIRAPP2 0 -> 1`.
4. Check: both remotes lock and unlock, also 10 s later; the key still starts the engine.

`kline risk --ecu bmt 31C7000000` prints the same command (`command (remote learning: ...)`).

## ABS (Bosch 5.3) on the car

The car's ABS (0x01, ISO8, key bytes `DC 83`) has no usable X65 definition (the DDT2000 X65
Bosch 5.3 files are empty stubs); `X24_ABS_Bosch5.3.xml` (Renault Master) has the same ISO8
link and the same `51`/`58` layout as the car's replies (2026-11-23). Use it with
`--variant single` (plain 5-baud `01`, no handshake):

1. `kline --trace connect --xml X24_ABS_Bosch5.3.xml --variant single`
2. `kline ident --xml X24_ABS_Bosch5.3.xml --variant single`: `51` decoded (Renault part
   number, software, Bosch number).
3. `kline dtc --xml X24_ABS_Bosch5.3.xml --variant single`: `58`, one line per set fault slot
   with its context, for the car's reply of 2026-11-23 (simulated with the same bytes):
   `Défaut mémoire 1 = signal capteur AVG  (compteur mémoire 1 1, Etat contacteur stop 1
   Pédale relâchée, Etat régulateur ABS 1 Inactif, Tension 1 13.56 V, Vitesse de référence 1
   16. km/h)`. The fault texts are the X24 list (NOT VERIFIED for the
   X65 software). JSON: each `active` entry has `context` (the slot's other items).
4. `kline read --xml X24_ABS_Bosch5.3.xml --variant single 51 58`: both raw frames decoded.
5. Only when the owner decides: `kline clear-dtc --xml X24_ABS_Bosch5.3.xml --variant single`
   sends `45` (MES_RAZ, Tier 1, brake-system warnings and prerequisites), then `kline dtc`
   again to see whether the slot stays empty.

`41` downloads and `71` routine runs (live values, valve tests, factory fill, bleeding,
coding) are **forbidden** in kline-core on the Bosch 5.3 ABS (owner decision 2026-11-23,
[SAFETY.md](SAFETY.md) "ABS / brake ECU"): a `71` can leave a valve or the pump energized, kline
has no sequence executor that sends the OFF / FIN frame by itself, and whether the ECU releases
its outputs on `3F` or keep-alive loss is NOT VERIFIED. `kline risk` and `exec` refuse them
before the port is opened (exit 7); the factory fill / bleed bytes are named in the refusal
("permanently forbidden: factory fill rig / brake bleed; use a garage"). They are refused on any
ISO8 unit at `01`, whatever key bytes the definition declares or the ECU answers. `exec --stop` with a non-read stop request is refused
on the ISO8 unit at `01` (kline-core allows one approved request per session there).
`11 FF`, `31 A0 00`, `32 A0 00` and `30 lid ..` are forbidden there too (no ABS requests).
A lost reply to `45` points at `kline read .. 58`.

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
