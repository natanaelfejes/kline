# kline-sim: a K-line ECU simulator for testing without a car

`crates/kline-sim` behaves like the car on the K line: it echoes bytes like a KKL cable, detects
5-baud and fast-init wake-ups from break edges, and answers KWP2000 and ISO 9141 requests from a
DDT2000 definition. It lets the protocol core and the `kline` CLI be tested end to end without
the car. Nothing in it is measured on the car, and the last section says plainly what that
means.

## Contents

- [Pieces](#pieces)
- [Wire behaviour](#wire-behaviour)
- [pty transport: how break and DTR are represented](#pty-transport-how-break-and-dtr-are-represented)
- [Replay log (JSONL)](#replay-log-jsonl)
- [What the simulator proves and what it cannot](#what-the-simulator-proves-and-what-it-cannot)

## Pieces

| Module | What it does |
|---|---|
| `bus` | The K line of a dumb FTDI KKL cable: echo of every TX byte, break loopback, one 0x00 "break residue" on break release (optional), 5-baud and fast-init wake-up detection from the break edges, time-stamped delivery of the ECU's bytes. |
| `ecu::SpecEcu` | An ECU answering from a kline-spec `EcuDef` and a `Scenario`. Profiles: Sirius 32 (0x7A, KWP 5-baud, `55 D0 8F`, ~KW2 handshake), BMT (0x26, same), Safir 2 (0x7A, ISO8, `55 C2 85`, no handshake), or `EcuProfile::from_def`. |
| `scenario` | Values per `<Data>` name; the positive response builder. |
| `classify`, `journal` | The simulator's own read/dangerous classification and the **ECU write log**, plus timing violations and notes. |
| `replay::ReplayEcu` | Plays back a JSONL capture as the ECU. |
| `line::VirtualLine` | In-process duplex with a virtual millisecond clock (a 5-baud init costs no real time). |
| `pty::PtySim` | The bus behind a pseudo-terminal plus a control socket (Linux/macOS). |
| `driver::Driver` | Runs kline-core's `FiveBaudInit` / `FastInit` / `Session` over any `Line`, optionally capturing the JSONL log. A test driver; the CLI uses its own runner in `kline-host` (`link`). |

## Wire behaviour

### Wake-up and handshake

- **Wake-up.** A falling edge starts a frame. A rising edge within the fast-init window
  (strict 24..26 ms, lenient 15..40) is a fast init. Otherwise, in the middle of the stop bit
  (start + 1900 ms) the byte is decoded by sampling the middle of each 200 ms bit; strict mode
  also requires every edge within +-10 ms of the 200 ms grid (ISO 9141-2: 5 baud +-5 %) and
  300 ms idle before the start bit (ISO 14230-2 W5). The ECU answers `55` at stop-bit end + W1
  (60), KW1 after W2 (10), KW2 after W3 (10).
- **Which 5-baud byte wakes the ECU**: `AddressAccept::Plain` (0x7A / 0x26, `InitVariant::SingleEcu`),
  `Xor80` (0xFA / 0xA6, `MultiEcu`) or `Both`. Fast init is accepted only with
  `accept_fast_init`.
- **KWP handshake.** ~KW2 must come within W4 (strict 25..50 ms after KW2, lenient 0..1000);
  the ECU sends `~(received address byte)` W4 (30) later: 0x85 / 0xD9 plain, 0x05 / 0x59 XOR'ed.
### Requests and answers

- **Requests.** Frames are parsed by the simulator's own codec (`frame`, not kline-core's).
  Bad checksum, length or a non-physical format byte: ignored (violation `BadFrame`). A KWP
  header whose target is not the ECU address is ignored. Strict mode ignores a request that
  starts less than P3min (`ReplyToRequest@Delay`, 5) after the ECU's last byte, or whose bytes
  are more than P4max (20) apart. Replies start after P2 (25 ms).
- **Answers.**
  - built in: KWP `81` -> `C1 KW1 KW2`, `82` -> `C2` then asleep, `3E` -> `7E`; ISO8 `36` ->
    nothing by default, `3F` -> nothing then asleep;
  - a request matching a definition (`SentBytes`, sent-item bytes free): SID + 0x40, the echo
    of the request bytes before the first received item, zeros up to
    `max(MinBytes, last item end)`, then every scenario value inserted with
    `DataDef::encode_exact` + `insert`. All definitions with the same bytes (BMT `2180`) are
    answered in one frame;
  - no received items: SID + 0x40 and `sent[1..]`, padded to MinBytes (`10 A0` -> `50 A0`);
  - unknown service: `7F sid 11`; known service, unknown parameters: `7F sid 12`.
  - The ECU sleeps after 5000 ms of silence (ISO 14230-2 P3max), so TesterPresent matters.
### Failure modes and the ECU write log

- **Failure modes.** Init: `Silent`, `WrongKeywords`, `NoAddressComplement`. Replies (per payload
  prefix, optionally once): `BadChecksum`, `ResponsePending { count }`, `Negative { nrc }`,
  `DropBytes { count }`, `Slow { delay_ms }`, `NoReply`. Timing: `Tolerance::strict` /
  `lenient`; `EcuTiming` sets the ECU's own W1..W4, P1, P2.
- **ECU write log.** Every request that is not a read (by the simulator's own list, written
  from [SAFETY.md](SAFETY.md) rules 2 and 3) is recorded and printed to stderr as `kline-sim: ECU WRITE LOG:`,
  whether or not the definition knows it; this includes BMT `21 A4 ..` with extra bytes (the exact
  2-byte transponder *status* read is Tier 0 and not logged) and the
  approved BMT routines. `Journal::assert_no_writes()` panics with the whole log.

## pty transport: how break and DTR are represented

A pty has no K line. On Linux (6.18, serialport 4.10) `set_break`/`clear_break` on the slave
succeed but the master cannot see them, and DTR fails with ENOTTY. Wake-ups are made of break
edges, so they go over an explicit side channel: a Unix socket at `PtySim::control_path()`, one
command per line, each answered with one line:

| Command | Reply |
|---|---|
| `BREAK 1` / `BREAK 0` | `OK` (edge applied at arrival time, before the reply) |
| `DTR 1` / `DTR 0` | `OK` (logged) |
| `RTS 1` / `RTS 0` | `OK` (logged; `--rts low`) |
| `LOOPBACK?` | `1` / `0`: was the last break seen on RX |

Data bytes go through the pty and are echoed back, like the KKL cable. A CLI transport for the
simulator forwards `set_break`, `set_dtr` and the loopback check to `pty::ControlClient` and
everything else to the serial port (see `tests/pty.rs`, `SerialLine`). This is deliberately not
a timing guess on the byte stream: a 5-baud init on the real cable produces no bytes at all,
only break edges, so there is nothing in-band to detect. Wall-clock jitter of a few ms is
normal on the pty; use lenient tolerance there.

Pace the break edges from deadlines, not with one plain sleep per bit. The bus samples each
5-baud bit in its middle on a 200 ms grid counted from the start bit, so lateness that adds up
over the bits shifts the decoded address. On the GitHub macOS runner (macos-26-arm64, a
`VirtualMac2,1` VM, 2026-11-22) `thread::sleep(200 ms)` woke up to 83 ms late and
`thread::sleep(1 ms)` took 6.6 ms median; a tester that slept 200 ms after each edge drifted by
more than half a bit from the second data bit on, the bus decoded 0x7A as 0xF4, the ECU did not
accept it and sent no keywords (the init failed with `NoKeywords { received: [] }`). `SerialLine`
in `tests/pty.rs` therefore sleeps each bit until the previous deadline plus 200 ms, in OS sleeps
of at most 5 ms and a busy-waited 8 ms tail, so an edge is late only by its own sleep.
`kline_host`'s `KklLine` paces the same way, with chunked OS sleeps and an adaptive spin tail,
and aborts an init attempt whose wake-up sleep woke up beyond the tolerance (docs/platforms.md
"Wake-up pacing and the init timing guard"). When an init
over the pty fails, the test prints the simulator's notes, which name the address the bus
decoded.

## Replay log (JSONL)

One object per line: `{"t_ms":300,"dir":"tx","kind":"five_baud","hex":"7A"}`.
`dir` is `tx` (tester wrote; `t_ms` = start of the write) or `rx` (tester read; `t_ms` = when the
read returned; the echo of the preceding `tx` may be included and is stripped). `kind` marks a
wake-up: `five_baud` (hex = address byte, `t_ms` = first break) or `fast_init`. `Driver::capturing`
writes this format; the CLI's captures use the same format plus `unix_ms`, after a first
`{"header":{..}}` line (tool, version, git commit) that the reader skips ([cli.md](cli.md#capture-files)). On replay the ECU waits for
each `tx` (same wake-up or same bytes), then sends the following `rx` bytes at the recorded
delays. A difference is a `ReplayMismatch` violation and the replay goes silent.

## What the simulator proves and what it cannot

**It proves**, for the paths it exercises:

- kline-core's machines, driven over a byte-level line with echo and real waiting semantics,
  complete a Sirius / BMT / Safir session and produce frames that an independent parser
  accepts; the timing the core produces lies inside the ISO windows the strict ECU checks
  (5-baud bit grid, W5 idle, W4, P3min = 5, P4).
- The core's handling of each failure mode on the wire (silence, wrong key bytes, missing
  ~address, bad checksum, 0x78 sequences and their bound, 7F, truncated replies, replies after
  P2 and recovery after them).
- `decode_checked(sim(request))` returns the scenario values, for the hand-written fixtures and,
  structurally, for every allowed read of the real SIRIUS32 and BMT_X65 definitions.
- The InitVariant fallback can be tested against an ECU that accepts only the plain, only the
  XOR'ed address byte, or only fast init.
- Nothing dangerous reached the ECU in a test: the write log is a second, independent safety
  check behind kline-core's allowlist.
- A serialport-based tester can drive the pty like a serial device.

**It cannot prove anything about the real car.** It encodes the same assumptions as the core
(both come from [protocol.md](protocol.md) and the same reverse-engineering notes), so where those are wrong, both are
wrong together:

1. 5-baud byte format: 8 raw bits LSB first, no parity, 200 ms bits, one stop bit.
2. Which address byte each ECU accepts (plain vs ^0x80) and whether fast init works on 0x7A.
3. That the ECU answers ~KW2 with ~(address byte) and that a KWP header target is the plain
   address.
4. Key bytes `D0 8F` (Sirius, BMT), `C2 85` (Safir 2); the length-in-format rule from KW1; the
   StartCommunication key bytes after fast init (copied from the 5-baud ones).
5. Framing: header layout, 8-bit sum checksum, ISO8 `[N][data][cs]`; Safir status replies are
   not simulated.
6. Positive reply = SID + 0x40 + echo of the request bytes before the first item, length from
   MinBytes and the items; replies to requests without received items (`50 A0`, `7E`, `C2`,
   `71 C8 00 00 00`) are guesses.
7. The ECU's timing: W1 60, W2/W3 10, W4 30, P2 25, P1 0 ms, P3max 5000, P3min = XML
   `ReplyToRequest` (5, where ISO says 55), P4max 20. The real ECUs may differ; e.g. an ECU with
   W1 near the ISO maximum (300) overruns the core's keyword window
   (`timing::late_w1_overruns_the_core_keyword_window`).
8. That TesterPresent `3E` is answered `7E`, StopCommunication `82` with `C2`, that Safir
   ignores ISO8 `36`, and how 0x78 sequences are spaced.
9. That 1 ms per byte (0.96 ms at 10400 baud) is close enough; baud 10400 vs 10417 is not
   modelled at all (no bit-level UART).
10. The cable: echo always exact, break looped back, one 0x00 residue byte. Real FTDI behaviour
    under macOS (break latency, residue count, DTR effect) is not modelled.
11. Not modelled at all: collisions (ECU and tester transmitting together), electrical faults,
    noise, a second ECU on the line, the L line, battery voltage.
12. The scenario data layout comes from the definitions; if the DDT XML misdescribes a frame,
    the simulator agrees with it. ddt4all cross-checks only cover decoding.

Only hardware-in-the-loop tests on the car ([protocol.md "Not verified"](protocol.md#not-verified-needs-the-car)) can settle these. A capture from
the car, replayed with `ReplayEcu`, is the step between the two.
