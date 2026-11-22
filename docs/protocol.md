# Renault K-line protocol: ISO 9141 5-baud init, KWP2000 fast init and framing

This page explains how the K-line diagnostic protocol works on late-1990s Renault ECUs, and then
documents exactly what `kline-core` sends and expects on the line: the ISO 9141-2 5-baud init,
the KWP2000 (ISO 14230) 5-baud and fast init, the framing and checksum, the echo, every timing
value, and the safety policy. It is a behaviour spec, not a description of DDT2000's code.

If you just want the idea, read [K-line in two minutes](#k-line-in-two-minutes). The rest is
reference material with exact bytes and milliseconds.

**Nothing on this page has been verified on a real car yet.** See [Not verified](#not-verified-needs-the-car)
at the end.

## Contents

- [K-line in two minutes](#k-line-in-two-minutes)
- [Where the numbers come from](#where-the-numbers-come-from)
- [The Clio II ECUs](#the-clio-ii-ecus)
- [Line settings](#line-settings)
- [Timing values](#timing-values-ktiming)
- [5-baud init (ISO 9141-2 slow init)](#5-baud-init-iso-9141-2-slow-init)
- [Init variants: single ECU, Multipoint, fast init](#init-variants-single-ecu-multipoint-fast-init)
- [KWP2000 fast init (ISO 14230-2)](#kwp2000-fast-init-iso-14230-2)
- [Framing and checksum: ISO 9141 and KWP2000](#framing-and-checksum-iso-9141-and-kwp2000)
- [One request/reply exchange](#one-requestreply-exchange)
- [Session: TesterPresent and Stop](#session-testerpresent-and-stop)
- [DDT2000 retry behaviour](#ddt2000-retry-behaviour)
- [Sans-IO contract](#sans-io-contract)
- [Safety policy](#safety-policy)
- [Not verified (needs the car)](#not-verified-needs-the-car)

## K-line in two minutes

**The wire.** The K line is one wire between the diagnostic socket and the ECUs. It idles high
(at battery voltage) and both sides pull it low to send. Bytes are ordinary UART bytes, 8N1, at
10400 baud on these Renaults. Because there is only one wire, it is half duplex: only one side
talks at a time.

**The cable.** A "dumb" KKL cable is a USB serial chip (usually an FTDI FT232R) plus a level
shifter. Its TX and RX both sit on the K line, so **every byte you send comes straight back on
RX**. That is the *echo*. A tester reads the echo and compares it with what it sent: a
difference means someone else talked at the same time (a collision). No echo at all usually
means the K line is not connected or not powered.

**Waking the ECU: 5-baud init.** An ECU on the K line sleeps until a tester wakes it. The
classic way (ISO 9141-2, also used by KWP2000 as "slow init") is to send the ECU's address at
**5 bits per second**: the tester holds the line low or high for 200 ms per bit, so one address
byte takes 2 seconds. A KKL cable does this with the serial *break* signal (break on = line low).
The ECU answers at 10400 baud with a sync byte `55` and two *key bytes* (KW1, KW2) that say which
protocol variant it speaks. In the KWP2000 variant the tester then sends KW2 inverted (`~KW2`)
and the ECU answers with its address inverted (`~address`). After that the session is open.

**Waking the ECU: fast init.** KWP2000 (ISO 14230-2) adds a faster wake-up. The tester pulls
the line low for 25 ms, releases it for 25 ms, and immediately sends a normal frame with the
StartCommunication service (`81`). The ECU answers `C1 KW1 KW2`. This takes about 50 ms
instead of 2 s, but the 25 ms pulse must be accurate to about 1 ms, which is hard over USB.

**Frames.** After the init, every request and reply is a short frame. A KWP2000 frame looks like
`82 7A F1 21 A0 AE`:

| Byte | Meaning |
|---|---|
| `82` | format byte: physical addressing (`0x80`) plus 2 data bytes |
| `7A` | target: the ECU address (here the Sirius 32 injection) |
| `F1` | source: the tester |
| `21 A0` | data: service `21` (read data by local identifier), identifier `A0` |
| `AE` | checksum: the 8-bit sum of all bytes before it |

The ECU's positive reply uses the service number plus `0x40` (`61 A0 ...`). A negative reply is
`7F`, the service, and a reason code. Reason `78` means "busy, wait", and the tester keeps
listening. The older ISO 9141 framing that DDT2000 calls "ISO8" (Sagem Safir injection) is
simpler: a length byte, the data and the same kind of checksum, with no addresses.

**Timing.** The protocol is full of small gaps: how long the ECU may take to answer (P2), how
long the tester waits before the next request (P3), the gap between bytes, and the wait between
KW2 and `~KW2` (W4). If the line is idle too long (a few seconds), the ECU goes back to sleep,
so the tester sends *TesterPresent* (`3E`) to keep the session alive. When it is done it sends
StopCommunication (`82`).

**Renault specifics.** These ECUs use tester address `F1`, 10400 baud, KWP2000 services such
as `21` (read by local identifier) and `14` (clear fault codes), and the request and data layout
comes from DDT2000's XML definition files. The rest of this page is the exact version of all of
the above, as `kline-core` implements it.

## Where the numbers come from

Every value below carries a source tag:

- **RE: IOTBD32V3 ...**: DDT2000's native K-line DLL (serial port, init, echo, reply reading).
- **RE: KWP2000Diag4 ...**: DDT2000's protocol classes (framing, reply checks, per-protocol defaults).
- **XML <K>**: the ECU definition file.
- **ours**: a choice of this project where DDT2000 has no equivalent, or where we deliberately differ.

"RE" means the behaviour was read from DDT2000's binaries by reverse engineering. The
reverse-engineering notes themselves are not in this repository.

## The Clio II ECUs

| ECU | Address | `<K>` | Init path | KW1 KW2 |
|---|---|---|---|---|
| Injection, **Siemens SIRIUS 32** (this car, from the ECU label) | 0x7A | `KWP Multipoint="1"` + `ISO8` | KWP 5-baud Multipoint, ~KW2 handshake | D0 8F |
| BMT / UCH (body) | 0x26 | `KWP Multipoint="1"` + `ISO8` | KWP 5-baud Multipoint, ~KW2 handshake | D0 8F |
| Injection, Sagem Safir 1 / 2 (not this car; kept as a second path) | 0x7A | plain `ISO8` | ISO8 5-baud, no handshake | 1C 86 / C2 85 |

The x65 addressing file also lists KWP2000 FastInit MonoPoint for 0x7A. The fast-init path
can target 0x7A as a fallback for the car test.

## Line settings

| Item | Value | Source |
|---|---|---|
| Baud | **10400** 8N1, even when the XML says 10417 (Safir). Configurable (`KTiming::baud`) | RE: IOTBD32V3 Setup; the XML baud is never applied before the worker starts. Multipoint forces 10400 (RE: KWP2000Diag4 KWP2000Ecu) |
| DTR | high while idle, low during the 5-baud bits | RE: IOTBD32V3 5-baud routine |
| Tester address | 0xF1 | RE: KWP2000Diag4 KWP2000Ecu default ToolAddress |

## Timing values (`KTiming`)

All values in milliseconds.

| Field | IOTBD default | ISO8 class default | KWP class default | Multipoint (`MultiEcu` only) | XML attribute |
|---|---|---|---|---|---|
| `kw2_delay_ms` (W4, before ~KW2) | 25 | 25 (not set by the class) | 25 | forced 25 | `KWP/KW2_@Delay` |
| `first_request_ms` (after 5-baud init) | 20 | 200 | 20 | forced 20 | `ISO8/FirstRequest@Delay` |
| `inter_byte_ms` (TX byte gap) | 0 | 2 | XML | **forced 7** | `InterByte@Delay` |
| `reply_to_request_ms` (P3) | 5 | 100 | 5 | forced 5 | `ReplyToRequest@Delay` |
| `request_to_reply_timeout_ms` (each read) | 300 | 100 | 300 | forced 300 | `RequestToReply@Timeout` |
| `tester_present_ms` (idle before TP) | 3000 | 3000 | 3000 | forced 3000 | `TesterPresent@Delay` |
| `on_error_ms` (gap after an error) | 300 | 1000 | 300 | forced 300 | `OnError@Delay` |
| `kw2_timeout_ms` | 300 | 1000 | 300 | forced 300 | `ISO8/KW2@Timeout` |

Sources: RE: IOTBD32V3 context defaults; RE: KWP2000Diag4 ISO8Ecu / KWP2000Ecu constructors and
the KWP2000Ecu Multipoint init. Class defaults apply only when the XML attribute is absent
(`spec::k_timing`). `LinkConfig::timing` holds the declared values;
`LinkConfig::effective_timing()` applies the Multipoint column only under `MultiEcu`.
XML values are assumed to be ms.

### Fixed constants (`timing::consts`)

| Constant | Value | Source |
|---|---|---|
| 5-baud bit | 200 | RE: IOTBD32V3 |
| 5-baud stop-bit wait before reading | 100 | RE: IOTBD32V3 |
| 5-baud read window | `kw2_timeout_ms` + 100 | RE: IOTBD32V3 |
| Fast init | 1 ms, then 25 low, 25 high | RE: IOTBD32V3 |
| Multipoint 5-baud address XOR | 0x80 | RE: KWP2000Diag4 KWP2000Ecu |
| Idle before any init | 300 | ours (ISO 9141/14230 W5/Tidle; DDT2000 relies on its 300 ms worker exit tail) |
| Read timeout after a 0x78 reply | 5000 | ours (KWP2000 P2* extended timing) |
| Maximum consecutive 0x78 replies | 10 | ours |

Every read timeout is a **total** timeout for that read, not an inter-byte timeout
(RE: IOTBD32V3 COMMTIMEOUTS). A reply therefore gets 300 ms for its first byte and 300 ms for
the rest.

## 5-baud init (ISO 9141-2 slow init)

### Common part

From RE: IOTBD32V3 5-baud routine:

1. (ours) idle 300 ms.
2. DTR low. Break on (K low) for 200 ms: the start bit.
3. Check that RX saw the break (the KKL cable loops TX to RX). DDT2000 waits for the
   Windows `EV_BREAK` event and aborts with 0x3F0 without it (RE: IOTBD32V3 FUN_10001380).
   We can only infer the break from a byte on RX (a POSIX tty has no portable break event;
   the port is opened with `IGNBRK`, `BRKINT` and `PARMRK` cleared so a break reads as one
   `00`), so by default a missing loopback is a logged warning and the init goes on (ours).
   `--strict-loopback` (`LinkConfig::strict_break_loopback`): break off, DTR high, error
   **K line inactive**, like DDT2000. NOT VERIFIED what the FTDI cable under macOS delivers.
4. 8 data bits of the address byte, **LSB first, no parity bit**, 200 ms each: bit 0 = break
   on, bit 1 = break off.
5. Break off, wait 100 ms (stop bit; reading starts in the middle of it).
6. DTR high. **Flush RX** (break residue and the echo of the bits).
7. Read 3 bytes `55 KW1 KW2` within `kw2_timeout_ms + 100` (`LinkConfig::kw_window_ms`
   overrides this window for both 5-baud reads, ours, default off; NOT VERIFIED whether any
   ECU's W1 needs it). Fewer: error **no keywords**.
   The sync byte and the key bytes are reported, not checked (RE: IOTBD32V3 does not check
   them). With `reject_keyword_mismatch` (ours, default off) a mismatch with the definition's
   key bytes fails the init.

### ISO8 or KWP: what happens after the key bytes

It depends on the transport flag DDT2000 sets (RE: IOTBD32V3 worker; the nested
`<KWP><ISO8>` element only describes the init, the transport flag stays off for KWP):

- **ISO8 (Safir):** no handshake. Wait `first_request_ms`. Done.
- **KWP 5-baud (Sirius 32, BMT), any variant:** wait `kw2_delay_ms`, send ~KW2
  (one byte, 0x70 for KW2 0x8F), read 2 bytes within the same window: the echo of ~KW2 (must
  match, else **collision**; no byte at all is also **collision**, RE: IOTBD32V3
  FUN_100014f0 returns 0x3ED when the echo count is 0) and ~address (logged, **not**
  validated; echo but no ~address is error **no address complement**). Wait
  `first_request_ms`. Done.

### Addresses with bit 7 set

Addresses with bit 7 set are refused by both init machines (`AliasedAddress`): such a byte is
the Multipoint wake byte of `address & 0x7F` (0xA6 wakes the BMT), so the ECU that answers
could differ from the one the policy checks (found in a security review). The init result
(`InitOutcome`) is opaque, carries the config, init kind and target (derived from the
bit-banged byte, or the StartCommunication header target), and is consumed by `Session::new`.

### Example: Sirius 32, single ECU

As tested in `tests/init_traces.rs`: bits of 0x7A, read `55 D0 8F`, wait 25 ms, send `70`,
read `70 xx` (xx logged), wait 20 ms.

## Init variants: single ECU, Multipoint, fast init

The XML marks both the Sirius 32 and the BMT `Multipoint="1"`, but whether DDT2000 actually
uses Multipoint depends on how many ECUs it opens. Two RE sources:

- RE: KWP2000Diag4 KWP2000Ecu Multipoint init: with Multipoint the 5-baud address is
  `address ^ 0x80`, inter-byte 7 ms, and fixed timings (table above).
- RE: KWP2000Diag4 DiagProtocols connect: when a session opens exactly **one** KWP/ISO8 ECU,
  DDT2000 clears Multipoint during the init. A Sirius 32 opened alone gets the plain address
  and the XML inter-byte gap (0).

Both keep the ~KW2 handshake and the 25 ms / 20 ms waits.

| Variant (`InitVariant`) | 5-baud byte (Sirius / BMT) | Inter-byte | Timings | Expected ~address |
|---|---|---|---|---|
| `SingleEcu` (**default**) | 0x7A / 0x26 | XML (0) | XML / class defaults | 0x85 / 0xD9 |
| `MultiEcu` | 0xFA / 0xA6 | 7 ms | fixed Multipoint values | 0x05 / 0x59 |
| `FastInit` (fallback, KWP only) | none: fast init + StartCommunication | XML (0) | XML | none |

`MultiEcu` has no effect on an ECU whose XML has no Multipoint. The 5-baud byte is always
8 raw bits, LSB first, no parity bit (RE: IOTBD32V3).

### Detection order (`--variant auto`)

**The car must confirm which variant each ECU needs.** `--variant auto` order (alone on the
line): **FastInit, then SingleEcu** (DDT2000's own detection order for 0x7A and 0x26: DiagScanXml
0x5004ee50 walks the function's `<ProtocolList>` of x65_addressing.xml in file order, Code 4
"KWP2000 FastInit MonoPoint" then Code 2 "KWP2000 Init 5 Baud Type I", `MultiPoint="0"`), then
our MultiEcu fallback; ISO8 (Safir) only SingleEcu. The whole list is tried up to 3 rounds,
500 ms apart (DDT2000 "Init retry n/3"; `--init-attempts`). Constants:
`kline_core::retry::{DDT2000_DETECTION_ORDER, RetryPolicy}`.

## KWP2000 fast init (ISO 14230-2)

RE: IOTBD32V3 fast init: (ours) idle 300 ms, DTR high, 1 ms, break 25 ms, break off 25 ms,
then the StartCommunication exchange below with payload `81`. With KW1 still unknown, the
length goes in the format byte (RE: KWP2000Diag4 LenInFmt default): `81 7A F1 81 6D` for the
Sirius. The reply must be positive `C1 KW1 KW2`; anything else is an error. Then wait
`first_request_ms` (RE: IOTBD32V3 FUN_100015a0 sleeps FirstRequestDelay after
StartCommunication), like the 5-baud init. DDT2000's fast-init reader (RE: IOTBD32V3
FUN_10001290) only handles a length in the format byte; we also accept a separate length byte
there, as DDT2000's normal request reader does.

## Framing and checksum: ISO 9141 and KWP2000

Checksum: 8-bit sum of all preceding bytes, both framings (RE: KWP2000Diag4 builders).

### ISO8 (ISO 9141, Safir)

RE: KWP2000Diag4 ISO8 builder and parser: `[N][data..][cs]`, `N = len(data) + 1`,
no addresses. `33 50` -> `03 33 50 86`; TesterPresent `36` -> `02 36 38`; Stop `3F` -> `02 3F 41`.
Reply checks: at least 3 bytes, `N` = byte count - 1, checksum. A one-byte payload 0x15, 0x37,
0x38 or 0x39 is a status reply (`Reply::Iso8Status`, meaning unknown).

### KWP2000 (ISO 14230)

RE: KWP2000Diag4 KWP builder and parser: `[0x80+n][tgt][F1][data..][cs]` for
`n <= 0x3F`, else `[80][tgt][F1][n][data..][cs]` (`n <= 255`). Target is the ECU address,
never XOR'ed. The separate length byte is also used for short frames when KW1 is neither 0xD0
nor odd (RE: KWP2000Diag4 KWP2000Ecu init); D0 keeps the length in the format byte.

Examples:

| Request | Frame |
|---|---|
| `21 A3` to the BMT | `82 26 F1 21 A3 5D` |
| `21 A0` to the Sirius | `82 7A F1 21 A0 AE` |
| TesterPresent `3E` | `81 26 F1 3E D6` |
| StopCommunication `82` | `81 26 F1 82 1A` |

Reply checks: format byte must be physical (`fmt & 0xC0 == 0x80`, ours: stricter than
DDT2000, whose parser KWP2000Diag4 0x2305d230 checks only the two address bytes; another
address mode has a different header layout, and Renault ECUs answer physically), checksum,
byte 2 = 0xF1, byte 3 = ECU address, length consistent with the format or length byte.

Payloads returned to callers start at the response SID.

## One request/reply exchange

1. Sleep whatever is left of the gap owed since the last exchange: P3 (`reply_to_request_ms`)
   after a good reply, `on_error_ms` after an error, negative reply or status reply (ours: the
   gap is counted from the end of the last exchange, using the `now_ms` values the caller
   passes in; DDT2000 sleeps right after the reply, same minimum spacing).
2. **Flush RX** before every transmission (RE: IOTBD32V3 send routine).
3. Write the frame; in one write if the inter-byte gap is 0, else byte by byte with the gap
   (7 ms under `MultiEcu` with Multipoint).
4. Read the echo (frame length, `request_to_reply_timeout_ms`). Zero bytes: **K line
   inactive**. A leading 0x00 is break residue: drop it and read one more byte. Any other
   difference: **collision** (RE: IOTBD32V3).
5. Read 1 byte. Nothing: **no reply** (for TesterPresent and Stop a missing reply is fine).
   - ISO8: read `N` more bytes.
   - KWP, `fmt & 0x3F != 0`: read `(fmt & 0x3F) + 3` more bytes (RE: IOTBD32V3).
   - KWP, `fmt & 0x3F == 0`: read 3 (`tgt src len`), then `len + 1` (RE: IOTBD32V3
     FUN_100015a0 cmd 0x404 and KWP2000Diag4 0x2305d230 do the same; only IOTBD32V3's
     fast-init reader FUN_10001290 does not).
   A read that stops short: **incomplete reply**.
6. Decode and classify: `7F SID NRC` is a negative reply; **`7F SID 78` (responsePending)
   restarts step 5 without a new request**, with a 5000 ms first-byte timeout, at most 10
   times (ours: DDT2000 treats 0x78 as a plain error).

## Session: TesterPresent and Stop

- TesterPresent (`36` ISO8, `3E` KWP) is due when the line has been idle for
  `tester_present_ms` since the last exchange. Right after a 5-baud init it is due at once
  (the first frame DDT2000 sends is TP unless a request is queued); after a fast init
  `tester_present_ms - first_request_ms` after the init finished, i.e. `tester_present_ms`
  after StartCommunication (RE: IOTBD32V3 worker: TP-FR, then FR sleep). No extra gap is
  owed before the first request after either init: both waited `first_request_ms`. The caller polls
  `tester_present_due(now)` and starts it.
- TP and Stop replies are read when they come and are optional. DDT2000 skips reading them in
  Multipoint (RE: IOTBD32V3 Multicast flag) and discards them otherwise; we read with the
  normal timeout so a late reply cannot collide with the next request, and tolerate silence
  (ours).
- Stop: `3F` for ISO8, StopCommunication `82` for KWP. The session is closed afterwards.
- The engine sends neither StartCommunication nor StartDiagnosticSession by itself after a
  5-baud init; the CLI decides (Sirius 32 definition: `10 A0`). DDT2000 does the same: on the
  5-baud branch of KWP2000Ecu init (RE: KWP2000Diag4 0x2305a370, `local_8 = 0x5d`) it only
  fetches the key bytes; `81` and the KW1 length-byte rule (separate length byte unless KW1 is
  0xD0 or odd) exist only on the fast-init branch. So after a 5-baud init the length stays in
  the format byte whatever KW1 is (`Session::new`).
- Optional `TesterPresentMode::DDT2000_PERIODIC` (`--periodic-tester-present`): `3E` every
  2000 ms since the last TP whatever else is sent, reply required (RE: DdtDiagScrXml 0x520484a0,
  "Enable or disable physical TesterPresent request, when idle", `TesterPresent.WithResponse`).

## DDT2000 retry behaviour

Not in DDT2000's protocol layer but in its application; defaults of `kline_core::RetryPolicy`:

| What | Rule | RE |
|---|---|---|
| NRC 0x21 / 0x23 | wait 200 / 500 ms and send again, at most 10 tries (`kline ident`; `--nrc-tries`) | DiagScanXml 0x5007c790 |
| init | whole variant list up to 3 rounds, 500 ms apart | DdtDiagScrXml 0x520484a0 (`[0x32] = 0x1F4`), DiagFailures 0x2404f410 |
| refresh cycle (`live`) | only a cycle where every request failed counts; retry after 400 ms, at most 3 times, then exit 5 | DdtDiagScrXml 0x5204ce50 |

## Sans-IO contract

Machines (`FiveBaudInit`, `FastInit`, `Session`) emit one `Action` at a time and need exactly
one `Event` back: `Done` for `SetDtr`, `SetBreak`, `Sleep`, `FlushRx`, `Write`;
`BreakLoopbackSeen(bool)` for `CheckBreakLoopback`; `ReadOk(bytes)` with exactly `count` bytes
or `ReadTimeout(partial)` with fewer for `Read`. Any other event fails the machine
(`UnexpectedEvent`). Time is passed in as `now_ms`; nothing reads a clock. Why it is built this
way: [architecture.md](architecture.md#sans-io-actions-and-events).

## Safety policy

Enforced in `kline-core` ([SAFETY.md](SAFETY.md) rules 2 and 3; the tier policy and the
2026-11-22 decisions are the maintainer's). The core writes only frames it built itself:
`Action::Write` carries a crate-private `Frame` (bytes readable, no public constructor, not
`Clone`), and every request frame comes from a crate-private `Permit`. Decisions use the exact
payload bytes and the target taken from the init that actually ran; the loaded definition's
`DefinitionGuard` (class and immobilizer-named requests) can only add refusals.

### Tiers

| Tier | Requests (exact bytes) | How |
|---|---|---|
| 0 Read | KWP: `3E [x]`, `10 81`, `10 A0`, `1A xx`, `21 xx` (exactly 2 bytes, BMT `21 A4` transponder *status* included), `17`/`18` + 1-3 bytes. ISO8 (Safir): `21`, `33 50`, `33 51`, `33 52`, `33 54` (exactly what Safir_1/Safir_2 contain) | `Session::request`; autonomous use OK (CLI, `--json`, MCP). Read-only scan via `ScanPlan` (below) |
| 1 Service | KWP: `14 xx xx` clear DTC; not BMT: `30 lid` + 1-4 bytes output control, `11 FF` adaptation reset, exactly `32 A0 00` (end of injector inhibition; other `32`, and BMT `30`/`11`/`32`, are Expert, found in a review); Sirius `31 A0 A0` injector inhibition (warning `StopsEngine`); BMT `31 C7 00 00 00` / `31 C8 00 00 00` remote learning, BMT actuator `31 out param count` with `out` 0xA0..=0xB3 | human `Approval` per request |
| 2 Config write | BMT `3B D2 index value` (index 0..=13 except 2, value 0/1); other KWP ECUs `3B lid` + 1-4 bytes | human `Approval`, plus in the same session: a positive backup read (`21 A1` for BMT `3B D2`, else `21 lid`) and a positive `21 80` whose exact reply the caller says it chose the ECU definition from (BMT config indices differ between BMT_X65_ph1_p and BMT_SAGEM) |
| 3 Expert | any other request that is not forbidden (undocumented service, identifier, parameters or length), including `27` security access (`SecurityAccess`, `MayBrickEcu`), `34`-`37` transfer and `31 81`/`31 82` flash erase/checksum on non-BMT ECUs (`MayBrickEcu`, `MayLeaveProgrammingSession`), `10 xx` other than `10 81`/`10 A0` (`MayLeaveProgrammingSession`), `3D`/`38`-`3A`/`2C`/`23` on non-immobilizer ECUs (maintainer decision 2026-11-22, final) | human *expert* `Approval` (10-character code), `RiskInfo` with "unknown effect" and the specific warnings |
| forbidden | the immobilizer line only, see below | never, not even in expert mode |

### Always forbidden: the immobilizer

`DenyReason::Immobilizer`, `ImmobilizerNamed`; plus `ProtocolReserved` and `Empty`, which are
not safety rules:

- BMT/UCH/decoder: address 0x26 or 0xA6, or a definition whose `Target@Name`/`<Function Name>`
  says BMT/UCH/UCBIC/decoder/anti-demarrage/immobilizer (`kline_core::spec::ecu_class`), at any
  address and protocol claim: `3D`, `38`, `39`, `3A`, `2C` (memory write / routine by address /
  dynamic define: EEPROM with key codes); `30`/`31`/`32`/`33` with byte 2 in 0xC0..=0xC9 except
  the two exact remote-learning routines (C0-C8 immobilizer in the capability notes, C9 added by
  maintainer decision); `30`/`31`/`32`/`33` with byte 2 = actuator output `A6` (Ligne
  Transpondeur), `AB` (Led Verlog) or `AC` (Ligne ECM) (both BMT definitions, step 7);
  transfer `34`-`37` and flash erase/checksum `31`/`32`/`33` `81`/`82` (key-code EEPROM,
  maintainer decision step 7; memory read `23` stays Expert); every `2E`/`2F`; every `3B` except `3B D2`; `3B D2 02 ..` (AP1CLE, key
  related); `21 A4` with anything after it.
- Not BMT, any protocol claim (an ISO8 target at 0x7A included, found in a review):
  `31`/`32`/`33`/`38`-`3A` with `A0 A3` (reset of the backed-up memory, may touch the "verrou
  logiciel" area); `2E`/`2F`/`3B` on `A0`, `A2`, `AE`, `BD` (frames carrying immobilizer /
  verrou fields).
- Sirius (0x7A or a Sirius definition): also `30` output control on `A0`/`A2`/`AE`/`BD`, and
  `31`-`33`/`38`-`3A` on `A2`/`AE`/`BD` (`31 A0 A0` / `32 A0 00` stay Tier 1: `A0` there is the
  special-function routine).
- Name guard (`DefinitionGuard`, set on the `Session` by the CLI): Tier 1-3 requests of the loaded
  definition whose names, data items or enumerated parameter values (list texts) look
  immobilizer/transponder/key related
  ([cli.md](cli.md#name-guard)).
- Link frames `81`, `82` (KWP) and `36`, `3F` (ISO8) as requests (`ProtocolReserved`, session
  integrity rather than safety): only the init machines and `Session::tester_present` /
  `Session::stop` send them. ISO8 `36` (keep-alive) and `3F` (stop) are protocol-required
  (RE: KWP2000Diag4 ISO8Ecu init) although no Safir definition lists them.

### RiskInfo

Every non-read request has a machine-readable `RiskInfo { tier, what_it_changes,
what_can_go_wrong, reversible, prerequisites, warnings }` (`safety::assess`), from the
definitions and the capability catalogue. Examples: clear DTC "erases the stored fault
codes", `reversible: false`, `ErasesFaultMemory`; BMT `3B D2` "rewrites one BMT configuration
option", `reversible: true` (write the old value from the `21 A1` backup), `EepromWrite`.

### Approvals (feature `approved-routines`)

The feature is **off by default** (`tests/exploits.rs` checks the resolved features).
Without it no approval type or challenge method exists, so a library user, an MCP server or a
`--json` binary can only send Tier 0. Automated, MCP and `--json` callers *prepare* non-read
requests with `safety::assess` / `safety::tier_of`: print the `RiskInfo` and the exact CLI
command for a human, never execute.

Only the interactive `kline` CLI may enable the feature. It must read the confirmation from
`/dev/tty` (not stdin, so a pipe cannot answer) and never offer approvals with `--json`. The code
is an anti-accident gate: an agent driving a pty can type it (permitted by the owner when he
chooses). `kline-agent` (the read-only binary) depends on `kline-core` without it;
`crates/kline-agent/tests/agent.rs` runs `cargo tree -p kline-agent -e features` and fails if the
feature or `kline-cli` appears. Any future `kline-mcp` crate needs the same check
([cli.md](cli.md#building-kline-and-kline-agent)).

Flow: `Session::service_challenge(payload)` / `config_write_challenge(payload,
identification)` / `expert_challenge(payload)` returns an `ApprovalChallenge` with the exact
payload, target, tier, `RiskInfo` and a random code (OS randomness via `getrandom`, 6
characters, 10 for expert). The CLI shows all of it and the warnings verbatim, reads a line
from `/dev/tty`, and calls `Approval::confirm(challenge, typed)`. `Session::request_approved(
payload, approval, now)` sends only if the approval was made from this session's latest
challenge (random per-session nonce and per-request id), for this target, these exact bytes,
this tier and this `RiskInfo`, and Tier 2 preconditions still hold. The approval is consumed
and the challenge closed whatever happens. `Session`, the init machines, `InitOutcome`,
`Action` and `Frame` are not `Clone`, so nothing can be replayed by copying (compile-fail
doctests in `lib.rs`).

### Read-only scan (`scan::ScanPlan`)

Only `21 xx` or `1A xx`, exactly 2 bytes, over a range; one request at a time (the result must
be reported before the next), at least 200 ms between request starts; stops after 3 missing
replies in total or at the first security NRC (0x33, 0x35, 0x36). Run only as an explicit CLI
command, every request and reply logged. Never scans `30`/`31`/`3B` or anything else.

## Not verified (needs the car)

- Which init variant each ECU needs (plain 0x7A / 0x26 with 0 ms, or 0xFA / 0xA6 with 7 ms,
  or fast init). The two RE sources disagree on what applies in practice.
- Effective baud 10400 vs 10417 for any ECU.
- The ~address value the ECUs return; whether `55 D0 8F` arrives as expected.
- Break loopback detection on the FTDI cable under macOS.
- Whether the ECUs answer TesterPresent and Stop, and whether 0x78 ever occurs.
- Whether Safir answers ISO8 `36`; meaning of ISO8 status codes.
- The 5000 ms / 10x responsePending bounds; DTR effect on the KKL cable.
- Fast init on 0x7A.
- Whether any ECU needs a wider keyword window than KW2Timeout + 100 (`kw_window_ms`).
- That BMT `3B` identifiers other than `D2`, BMT `2E`/`2F`, and Sirius `2E`/`2F`/`3B` on `A0`,
  `A2`, `AE`, `BD` actually touch immobilizer data: refused as a precaution, not proven.
- Whether `21 A4 xx` has a side effect on the BMT (refused as a precaution, like `21 A1 xx`).
