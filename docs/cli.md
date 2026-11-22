# clio and clio-agent: command line

Three crates:

| Crate | What | Approvals |
|---|---|---|
| `crates/clio-host` | shared runtime: transports (FTDI KKL cable, simulator pty), link runner, JSONL capture, `--trace`, ECU definitions, the Tier 0 commands | none: depends on clio-core **without** `approved-routines` |
| `crates/clio-cli` | binary `clio`: everything above plus `exec` | the only crate that enables clio-core's `approved-routines` |
| `crates/clio-agent` | binary `clio-agent` for AI use: reads plus `prepare` | none (`tests/agent.rs` runs `cargo tree -p clio-agent -e features` and fails if `approved-routines` or `clio-cli` appears) |

Build each binary on its own when you deploy them:

```
cargo build --release -p clio-cli      # target/release/clio
cargo build --release -p clio-agent    # target/release/clio-agent
```

Cargo unifies features across the packages of one build. `cargo build -p clio-cli -p clio-agent`
(or `--workspace`, or `--all-features`) compiles clio-core once, with `approved-routines`, and
links that copy into clio-agent too. clio-agent still has no code path to an approval (it has no
`exec`, and clio-host has none), but in such a build the approval types are present in the binary.
clio-agent prints a warning on stderr when this happened (`safety::APPROVALS_COMPILED`).

## Common options

| Option | Meaning |
|---|---|
| `--json` | one JSON object per line on stdout (errors too: `{"event":"error","code":N,"kind":..,"message":..}`). Notes go to stderr. `clio-agent` always uses JSON |
| `--trace` | every action with a time stamp on stderr: DTR, break edges, sleeps, break loopback, flushes (with the bytes discarded), every TX frame and every RX read (`RX got/wanted in N ms`). Also saved as `<capture>.trace.txt` |
| `--port PATH` | serial port. Default: the single `/dev/cu.usbserial-*` (macOS AppleUSBFTDI); none or several is exit 3 |
| `--sim-control SOCKET` | `--port` is then clio-sim's pty, break/DTR go over this control socket (docs/sim.md) |
| `--capture-dir DIR` | default `$CLIO_CAPTURE_DIR`, else `~/Documents/Clio/captures` |
| `--defs DIR` | definitions for `--ecu`. Default `$CLIO_DDT_XML`, else `~/Documents/Clio/ddt-extract/ecus` |

ECU selection (every command that talks to an ECU, and `risk`/`prepare`/`replay`):

| Option | Meaning |
|---|---|
| `--ecu sirius` | `SIRIUS32___EMS3132___Soft_RE1_maj.xml` (0x7A) |
| `--ecu bmt` | `BMT_X65_ph1_p.xml` (0x26); `BMT_SAGEM.xml` is an extra `ident` candidate if present |
| `--xml PATH` | any DDT2000 ECU definition; repeat for `ident` candidates (the first one sets up the link) |
| `--address 0xNN` | overrides the definition's `<Function Address>` (also the safety target) |
| `--vehicle PATH` | vehicle addressing file (e.g. `vehicles/x65_addressing.xml`): function name, protocols, tool address |
| `--variant auto\|single\|multi\|fast` | init variant (docs/protocol.md "Init variants"). `auto` tries SingleEcu, MultiEcu, FastInit in that order, skips variants that would run the identical init, and logs each attempt (`init_attempt` events) |
| `--session auto\|none\|a0\|81` | after the init: `auto` sends `10 A0` if the definition has it, else `10 81` if it has that (KWP only). A negative answer is reported and the command continues |

Protocol and timing come from the definition's `<K>` section through clio-core (`spec::k_timing`):
ISO8 5-baud, KWP 5-baud (with ~KW2 handshake), KWP fast init. A definition with only `<CAN>`
is refused: "CAN not supported by a KKL cable yet" (exit 8).

## Commands

| Command | Does | Notes |
|---|---|---|
| `ports` | lists ports, marks candidates `*`, says which cable would be used | |
| `connect` | init (variants), session start, Stop | reports sync, KW1 KW2, ~address, key-byte match |
| `ident` | `21 80` (KWP; else Tier 0 requests named like "ident"), chooses the definition | first candidate whose declared key bytes match and whose identification requests all decode with the checked decoder; `ambiguous: true` when several fit. AutoIdent matching is not implemented |
| `read REQ` | one request (definition name or hex), decoded | Tier 0 only; a non-read is refused before the port is opened (exit 7) and again by clio-core |
| `live --frames A,B --rate HZ [--count N]` | all frames once per cycle, `HZ` cycles/s | TesterPresent when idle >= TesterPresent delay; Ctrl-C stops cleanly; 3 failed cycles in a row is exit 5 |
| `dtc` | `<Failures Id="1">` (Sirius 32): reads the Tier 0 frames that carry `FailureFlag="1"` items of `<Devices>` and lists the set flags with their device (DDT2000 DiagFailures); otherwise the frames whose name says `panne`/`defaut`/`fault`/`dtc`, non-zero items. Ends with the clear plan | never clears |
| `clear-dtc [--ddt2000-clear]` | Tier 1 clear: prints the exact bytes (default: `ClearDiagnosticInformation.*` if the definition has one, else the definition's own `14 ..` request; Sirius `14 00 00`) and what DDT2000's global clear would send (`14 FF 00` for the Sirius), then the `exec` approval | `clio` only; `--ddt2000-clear` sends DDT2000's choice |
| `scan --range 21:B0-C5 [--interval MS]` | clio-core `ScanPlan`: `21`/`1A` only, >= 200 ms apart, stops after 3 missing replies or a security NRC (exit 12) | `clio` only; not offered by clio-agent |
| `risk REQ` | tier and `RiskInfo` of a request, nothing sent | exit 7 when forbidden or refused by the name guard |
| `exec REQ` | ONE non-read request | `clio` only, see below |
| `replay FILE [--ecu/--xml]` | decodes a capture offline: wake-ups, each request with its replies (echo stripped, 0x78 sequences split), decoded with the definition if given | |
| `prepare REQ` | `clio-agent` only: `RiskInfo` plus the exact `clio ... exec ...` command for a human | never sends |

### exec (approval)

`exec` refuses, before opening anything, when `--json` is given or stdin/stdout is not a
terminal (exit 7). Otherwise it shows the request, its definition names and its `RiskInfo`, opens
the session, and for Tier 2 (config write) first reads `21 80` (the definition must be the one
`ident` chooses, unambiguously) and the backup read (`21 A1` for BMT `3B D2`, else `21 lid`).
It then makes the clio-core challenge, shows it on `/dev/tty` with a random code (6
characters, 10 for expert tier) and reads the answer from `/dev/tty` (not stdin), sending
TesterPresent while it waits (default 120 s, `--confirm-timeout`). A wrong code, timeout or
Ctrl-C is exit 9 and nothing is sent.

Name guard (all ECUs, Tier 1-3): a request is refused (exit 7) when a definition request with
those bytes, or one of its data items (name, description, mnemonic), contains an
immobilizer/transponder/key keyword (`clio_core::safety::immobilizer_keyword`: anti-demarrage,
transpond, immo, verlog, verrou, apptrp, ap1cle, code cle, key code, wegfahr, and the words
cle/cles/key/keys). The exact BMT remote-learning payloads `31C7000000`/`31C8000000` on 0x26
are exempt (their names contain "CLE"). "verrou" also matches central locking; that is refused too.

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

## Capture

Every session writes `<UTC>-<ecu>-<command>-<pid>.jsonl` in the capture directory, one line per
TX/RX, flushed as written, in the clio-sim replay format (docs/sim.md) plus `unix_ms`:

```
{"t_ms":300,"dir":"tx","kind":"five_baud","hex":"7A","unix_ms":1795342272000}
{"t_ms":2383,"dir":"rx","hex":"55D08F","unix_ms":...}
{"t_ms":2408,"dir":"tx","hex":"70","unix_ms":...}
```

`t_ms` is ms since the port was opened. `clio replay` decodes it; clio-sim's `ReplayEcu` plays it
back as the ECU (`crates/clio-cli/tests/replay.rs` replays a CLI capture and gets the same
decoded values).

## Simulator

```
clio --port /dev/pts/N --sim-control /tmp/sim.ctl connect --xml crates/clio-cli/tests/fixtures/sirius_cli.xml
```

The tests start `clio_sim::pty::PtySim` themselves; there is no standalone simulator binary yet.

## HIL quickstart (first visit at the car)

Nothing here has been run on the car yet. Do the steps in order and keep every capture
(`~/Documents/Clio/captures`, with `.trace.txt` when `--trace` is on). Do **not** run `exec` or
`scan` on the first visit.

Before: definitions in `~/Documents/Clio/ddt-extract/ecus` (or pass `--defs`); cable in the OBD
socket and the Mac; ignition ON, engine off; nothing else (DDT2000, another tester) on the line.

1. `clio ports`
   - OK: a line `* /dev/cu.usbserial-XXXX  usb 0403:6001 ...` and `cable: /dev/cu.usbserial-XXXX`.
   - Fail: `no KKL cable found` (exit 3): replug, check `ls /dev/cu.*`; several cables: pass `--port`.
2. `clio --trace connect --ecu bmt --variant single`
   - OK (exit 0): `BREAK LOOPBACK seen`, about 2 s of `BREAK on/off` + `SLEEP 200 ms`,
     `RX 3/3 ...: 55 D0 8F`, `TX 70`, `RX 2/2 ...: 70 D9`, then
     `connected: bmt at 26 (kwp, kwp 5-baud) with variant single; sync 55 KW D0 8F, ~address D9`
     and `TX 81 26 F1 82 ..` (Stop).
   - `BREAK LOOPBACK NOT seen` / `K line inactive` (exit 4): the break does not reach RX. Check
     ignition, the cable's K line (OBD pin 7). How the FTDI reports a looped break under macOS is
     NOT VERIFIED: send the trace.
   - `RX 0/3` (`no keywords`): the ECU did not wake on 0x26. Go to step 3.
   - `RX 1/3` or `2/3`, or key bytes other than `D0 8F`: keep the capture, stop, report it.
3. Only if step 2 got no keywords: `clio --trace connect --ecu bmt --variant multi`
   (expects `TX A6` as the 5-baud byte and `~address 59`), then `--variant fast`
   (`TX 81 26 F1 81 19`, expects `C1 ..`). Then `clio --trace connect --ecu bmt` (auto) should
   log the failed attempts and connect with the variant that worked.
4. `clio ident --ecu bmt`
   - OK: `21 80 -> 61 80 ...`, `definition: .../BMT_X65_ph1_p.xml` (or BMT_SAGEM). `AMBIGUOUS` means
     both definitions decode the reply: fine for reads, decides nothing for config writes.
   - `7F 21 xx` (exit 6) or `NONE fits` (exit 10): keep the capture, report it.
5. `clio live --ecu bmt --frames 21A3 --rate 2` and press the remote a few times: the remote
   status items should change. Ctrl-C to stop.
6. `clio dtc --ecu bmt`: lists the fault flags that are set (exit 0 even when some are set).
7. `clio --trace connect --ecu sirius --variant single`
   - OK: `RX 3/3 ...: 55 D0 8F`, `TX 70`, `RX 2/2 ...: 70 85`, then `session 10 A0: ok 50A0`.
   - Failures as in step 2; then step 3 with `--ecu sirius` (`FA` / `~address 05`; fast init
     `81 7A F1 81 6D`).
8. `clio ident --ecu sirius`, then
   `clio live --ecu sirius --frames 21A9,21AA --rate 2` (engine off: rpm 0, coolant and battery
   voltage plausible), then start the engine and watch rpm.
9. `clio dtc --ecu sirius`.
10. Offline: `clio replay ~/Documents/Clio/captures/<file>.jsonl --ecu sirius` should show the same
    values. Send the captures and traces for the docs/protocol.md "Not verified" list.

## clio-agent

Same options and commands as above except `scan` and `exec`, always JSON. For a non-read it
offers only `prepare`:

```
$ clio-agent prepare --ecu sirius 11FF
{"event":"prepare","request":"11FF","risk":{"tier":"service (tier 1)",...},
 "command":"clio exec --ecu sirius 11FF","note":"only a human can run this, ..."}
```

`read` of a non-read is exit 7 and nothing reaches the ECU (`crates/clio-agent/tests/agent.rs`
checks the simulator's write log).
