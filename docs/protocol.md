# Renault K-line protocol: ISO 9141 5-baud init, KWP2000 fast init and framing

This page explains how the K-line diagnostic protocol works on late-1990s Renault ECUs, and then
documents exactly what `kline-core` sends and expects on the line: the ISO 9141-2 5-baud init,
the KWP2000 (ISO 14230) 5-baud and fast init, the framing and checksum, the echo, every timing
value, and the safety policy. It is a behaviour spec, not a description of DDT2000's code.

If you just want the idea, read [K-line in two minutes](#k-line-in-two-minutes). The rest is
reference material with exact bytes and milliseconds.

The Sirius 32, BMT, airbag and ABS paths were checked on a 1999 Clio II on 2026-11-22 and
2026-11-23 ([What the car confirmed](#what-the-car-confirmed)). Everything else on this page comes
from reverse engineering and the simulator; open points are in [Not verified](#not-verified-needs-the-car).

## Contents

- [K-line in two minutes](#k-line-in-two-minutes)
- [Where the numbers come from](#where-the-numbers-come-from)
- [What the car confirmed](#what-the-car-confirmed)
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

## What the car confirmed

Two sessions on a 1999 Clio II phase 1 (D7F engine), FTDI FT232R cable on macOS:

| Item | Result |
|---|---|
| Sirius 32 (0x7A) init | fast init not answered; 5-baud `7A` (`SingleEcu`) answered `55 D0 8F`, ~address `85` |
| BMT (0x26) init | same: 5-baud `26`, `55 D0 8F`, ~address `D9`; it also answers with the ignition off |
| Bosch 5.3 ABS (0x01) init | ISO8 5-baud `01`, `55 DC 83`, no handshake ([below](#bosch-53-abs-0x01-iso8)) |
| Airbag (0x2C) | Bosch (0 285 001 312 by part number cross-reference, not checked on the label). KWP 5-baud; the key bytes match `AB70_X65.xml`, but `AB80_X65_X90_V1.xml` decodes `21 80` too, so the definition choice is open. Identification, fault reads (present and memorized faults decoded) and the impedance and status blocks `21 80`..`21 8A`, `21 B0`, `21 C0`, `21 C1` answered. Nothing was written or cleared |
| Sessions | the Sirius accepts `10 A0` and answers `50` (one byte, no `A0`); `10 81` gets `7F 10 11`. The BMT definition has no session request. StopCommunication is answered `C2` |
| Link quality | about 20 000 requests over two drives with the engine running: no negative reply, no checksum error, no timeout |
| Throughput | one KWP request every 45 to 48 ms (about 21 per second); reply latency 26 to 43 ms. The 16 ms steps in the latency come from the macOS FTDI latency timer |
| Sirius reads | `21 80` (DDT2000 AutoIdent exact match), `21 82`, `21 A0`..`21 AF`, `21 C6`, `21 C7`, `21 F0`, `21 F1`, `21 FC`..`21 FF`. `21 90`..`21 97` and `21 E0` exist but answer `7F 21 22` (conditionsNotCorrect), engine running or not. `1A xx`: none |
| BMT reads | `21 80`, `21 9F`, `21 A0`..`21 A4`, `21 A7`, `21 A8` (`21 9x`, `Bx` and `Cx` mirror `Ax`). `1A xx`: none |
| BMT service | `14 FF 00` answered `54 FF 00`; remote learning `31 C7 00 00 00` answered `71 C7 01` and paired a remote; the lock / unlock outputs `31 A0 00 01` / `31 A1 00 01` work with the key out, also after the BMT had slept for 5 minutes; the indicator, wiper, courtesy light and buzzer output tests were accepted |
| BMT outputs missing on this unit | the door-open lamp `31 B0` and the windows `31 A2` / `31 A3` answer `7F 31 12` (subFunctionNotSupported): this BMT N2 has no window relay |
| Sirius output tests | engine off, one approved request each: `30 80` fuel pump, `30 9E` coolant warning lamp and `30 9F` fan low work; `30 94` (MIL) is accepted but the lamp stays dark (bulb); `30 A0` fan high is accepted with no effect (probably not fitted on cars without A/C); `30 A7` is accepted. One cycle lasts about half a second |
| BMT lock state | `21 A3` data byte 0 bit 7 follows the dashboard lock button (0 locked, 1 unlocked). It does not follow the K-line lock outputs, so after a K-line lock the remote's idea of the lock state can be out of step |

## The Clio II ECUs

| ECU | Address | `<K>` | Init path | KW1 KW2 |
|---|---|---|---|---|
| Injection, **Siemens SIRIUS 32** (this car, from the ECU label) | 0x7A | `KWP Multipoint="1"` + `ISO8` | KWP 5-baud Multipoint, ~KW2 handshake | D0 8F |
| BMT / UCH (body) | 0x26 | `KWP Multipoint="1"` + `ISO8` | KWP 5-baud Multipoint, ~KW2 handshake | D0 8F |
| Injection, Sagem Safir 1 / 2 (not this car; kept as a second path) | 0x7A | plain `ISO8` | ISO8 5-baud, no handshake | 1C 86 / C2 85 |
| Airbag (this car, `AB70_X65.xml`) | 0x2C | KWP | KWP 5-baud | per definition |
| ABS, **Bosch 5.3** (this car; X65 file empty, use X24_ABS_Bosch5.3.xml) | 0x01 | plain `ISO8` | ISO8 5-baud, no handshake (car 2026-11-23) | DC 83 |

The x65 addressing file also lists KWP2000 FastInit MonoPoint for 0x7A. On the car neither the
Sirius nor the BMT answered fast init, so `--variant auto` spends one failed fast-init attempt
before the 5-baud init that works (`--variant single` skips it).

## Line settings

| Item | Value | Source |
|---|---|---|
| Baud | **10400** 8N1, even when the XML says 10417 (Safir). Configurable (`KTiming::baud`) | RE: IOTBD32V3 Setup; the XML baud is never applied before the worker starts. Multipoint forces 10400 (RE: KWP2000Diag4 KWP2000Ecu) |
| DTR | high while idle, low during the 5-baud bits | RE: IOTBD32V3 5-baud routine |
| RTS | DDT2000: not driven (DCB RTS control disabled, so deasserted on Windows); kline: `--rts leave` (default) leaves it as the OS opens the port, `--rts low` raises DTR and then drops RTS once after open (`LinkConfig::rts`, `RtsMode`). NOT VERIFIED whether the KKL cable uses it | RE: IOTBD32V3 Setup |
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

Command-line overrides (ours): `LinkConfig::p3_ms` (`--p3`, 0..=1000),
`inter_byte_ms` (`--inter-byte`, 0..=20, ISO 14230-2 P4 range) and `w4_ms` (`--w4`, 25..=50,
ISO 14230-2 W4 range) replace `reply_to_request_ms`, `inter_byte_ms` and `kw2_delay_ms` in
`effective_timing()`, after the Multipoint column. The defaults stay the definition's values
above: the car's definitions give P3 5 ms and inter-byte 0, below ISO 14230-2 P3min 55 ms and
P4min 5 ms; NOT VERIFIED which values the car's ECUs need. The init trace line names the
effective values and marks an overridden one (`P3 55 ms (--p3)`). `--p3` does not change
`first_request_ms`, the wait after the init before the first request.

DTR after open (ours): `LinkConfig::dtr` (`--dtr high|low|leave`, default
high) is set once right after the port opens, before the first init and before `selftest`'s
break toggles (`PORT dtr=..` in the trace); the inits then drive DTR as below.

### Fixed constants (`timing::consts`)

| Constant | Value | Source |
|---|---|---|
| 5-baud bit | 200 | RE: IOTBD32V3 |
| 5-baud stop-bit wait before reading | 100 | RE: IOTBD32V3 |
| 5-baud read window | `kw2_timeout_ms` + 100 | RE: IOTBD32V3 |
| Fast init | 1 ms, then 25 low, 25 high | RE: IOTBD32V3 |
| Multipoint 5-baud address XOR | 0x80 | RE: KWP2000Diag4 KWP2000Ecu |
| Idle before any init | 300 | ours (ISO 9141/14230 W5/Tidle; DDT2000 relies on its 300 ms worker exit tail, and its scan waits 350 ms after `StopDiag` before the next ECU: DDT2000 behaviour note C3) |
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
   Windows `EV_BREAK` event and aborts with 0x3F0 without it (RE: IOTBD32V3).
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
   overrides this window for both 5-baud reads, ours, default off, CLI `--kw-window MS`;
   NOT VERIFIED whether any ECU's W1 needs it). Reading starts in the middle of the 100 ms
   stop-bit wait of step 5, so the budget after the stop bit is about the window minus 100 ms
   (300 ms by default): short for an ECU with W1 near the ISO 9141 maximum of 300 ms plus
   W2/W3 and USB latency (macOS FTDI ~16 ms). If connect fails with no keywords received,
   retry with `--kw-window 700`. Fewer than 3 bytes: error **no keywords**.
   The sync byte and the key bytes are reported, not checked (RE: IOTBD32V3 does not check
   them); a sync byte other than 0x55 only adds a warning (`InitOutcome::sync_ok`, ours:
   wrong baud or a line problem?). With `reject_keyword_mismatch` (ours, default off) a mismatch with the definition's
   key bytes fails the init.

### ISO8 or KWP: what happens after the key bytes

It depends on the transport flag DDT2000 sets (RE: IOTBD32V3 worker; the nested
`<KWP><ISO8>` element only describes the init, the transport flag stays off for KWP):

- **ISO8 (Safir):** no handshake. Wait `first_request_ms`. Done.
- **KWP 5-baud (Sirius 32, BMT), any variant:** wait `kw2_delay_ms`, send ~KW2
  (one byte, 0x70 for KW2 0x8F), read 2 bytes within the same window: the echo of ~KW2 (must
  match, else **collision**; no byte at all is also **collision**, RE: IOTBD32V3
  returns 0x3ED when the echo count is 0) and ~address (logged, **not**
  validated; echo but no ~address is error **no address complement**). Wait
  `first_request_ms`. Done.

### Addresses with bit 7 set

Addresses with bit 7 set are refused by both init machines (`AliasedAddress`): such a byte is
the Multipoint wake byte of `address & 0x7F` (0xA6 wakes the BMT), so the ECU that answers
could differ from the one the policy checks. The init result
(`InitOutcome`) is opaque, carries the config, init kind and target (derived from the
bit-banged byte, or the StartCommunication header target), and is consumed by `Session::new`.

### Example: Sirius 32, single ECU

As tested in `tests/init_traces.rs`: bits of 0x7A, read `55 D0 8F`, wait 25 ms, send `70`,
read `70 xx` (xx logged), wait 20 ms. On the car `xx` was `85`.

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

On the 1999 Clio II both KWP ECUs need `SingleEcu`. `--variant auto` order (alone on the
line): **FastInit, then SingleEcu** (DDT2000's own detection order for 0x7A and 0x26: DiagScanXml
walks the function's `<ProtocolList>` of x65_addressing.xml in file order, Code 4
"KWP2000 FastInit MonoPoint" then Code 2 "KWP2000 Init 5 Baud Type I", `MultiPoint="0"`), then
our MultiEcu fallback; ISO8 (Safir) only SingleEcu. The whole list is tried up to 3 rounds,
500 ms apart (DDT2000 "Init retry n/3"; `--init-attempts`). Constants:
`kline_core::retry::{DDT2000_DETECTION_ORDER, RetryPolicy}`.

Fast-init retry (ours): a fast init that the host's timing guard aborted
(TiniL or TWuP woke more than 2 ms late, [platforms.md](platforms.md#wake-up-pacing-and-the-init-timing-guard))
is tried once more right away before the next variant; no other failure is retried this way.

Wait after an answered attempt (ours): when a failed attempt got anything
from the ECU (key bytes, ~address, a StartCommunication reply or part of one, or a collision),
the ECU may still be in a half-open session, so the next init starts only after
`RetryPolicy::answered_init_wait_ms` = P3max, 5000 ms (ISO 14230-2,
`timing::consts::P3_MAX_MS`), on top of the normal gap. Silence, a missing echo and a
timing-guard abort get no extra wait. A failed attempt never has an open session in kline (the
`Session` exists only after a complete init), so there is no Stop to send instead. The trace
says `INIT the ECU answered the failed attempt: waiting 5000 ms (P3max) before the next init`,
the attempt line `... after waiting 5000 ms (P3max) ...` (JSON `answered_wait_ms`). Ctrl-C
ends the wait within 100 ms. DDT2000 has no such wait; NOT VERIFIED that the car's ECUs need
it.

## KWP2000 fast init (ISO 14230-2)

RE: IOTBD32V3 fast init: (ours) idle 300 ms, DTR high, 1 ms, break 25 ms, break off 25 ms,
then the StartCommunication exchange below with payload `81`. With KW1 still unknown, the
length goes in the format byte (RE: KWP2000Diag4 LenInFmt default): `81 7A F1 81 6D` for the
Sirius. The reply must be positive `C1 KW1 KW2`; anything else is an error. Then wait
`first_request_ms` (RE: IOTBD32V3 sleeps FirstRequestDelay after
StartCommunication), like the 5-baud init. DDT2000's fast-init reader (RE: IOTBD32V3) only
handles a length in the format byte; we also accept a separate length byte
there, as DDT2000's normal request reader does.

## Framing and checksum: ISO 9141 and KWP2000

Checksum: 8-bit sum of all preceding bytes, both framings (RE: KWP2000Diag4 builders).

### ISO8 (ISO 9141, Safir, Bosch 5.3 ABS)

RE: KWP2000Diag4 ISO8 builder and parser: `[N][data..][cs]`, `N = len(data) + 1`,
no addresses. `33 50` -> `03 33 50 86`; TesterPresent `36` -> `02 36 38`; Stop `3F` -> `02 3F 41`.
Reply checks: at least 3 bytes, `N` = byte count - 1, checksum. A one-byte payload 0x15, 0x37,
0x38 or 0x39 is a status reply (`Reply::Iso8Status`), named after DDT2000's errors.xml
(2022-2025, `iso8code`; `frame::iso8_status_name`): `37` requestUnknown, `38` receiveError,
`39` and `15` invalidRequest. kline reports it as an error with that name (exit 5; JSON
`iso8_status` plus `name` in `replay`).

Positive replies on ISO8 start with the request SID + 0x40 (KWP rule, assumed for Safir) or
with the request SID itself (Bosch 5.3 ABS, car 2026-11-23); `Request::decode_checked`
accepts the echo only for a Bosch 5.3 ABS definition (ISO8 with key bytes KW1=220 / KW2=131,
`DC 83`); other ISO8 definitions (Safir) keep the strict SID + 0x40 rule. DDT2000's ISO8
parser checks no SID at all.

### Bosch 5.3 ABS (0x01, ISO8)

Verified on the car on 2026-11-23 (captures `20261123T1221*-X24_ABS_Bosch5_3-*`, kline
`74b7fe4-dirty` with `--xml X24_ABS_Bosch5.3.xml --variant single`):

- 5-baud `01`, `55 DC 83` about 365 ms after the address byte. No ~KW2, no ~address.
- Frames `[N][data][cs]` exactly as above, every checksum verified. The ECU's reply starts
  30 to 80 ms after the echo of the request; its `N` byte arrives first, the rest about
  16 to 30 ms later.
- `36` (keep-alive) is answered `02 36 38`; `3F` (stop) is answered `02 3F 41`.
- `51` (MES_IDENTIF): `51 05 02 16 77 01 04 64 07 20 02 49 12 02 65 21 67 03 00 00 00 00 00
  00`: ABS, Bosch, after-sales id `16`, Renault part number `7701046407`, index " ",
  software `02 49 12`, Bosch number `0 265 216 703`, dates zero.
- `58` (MES_DIAG): `58 08 00 10 BF 01 00 ..`: three 5-byte slots (code, flags, reference
  speed km/h, voltage x 0.071 V, counter); slot 1 = code 8 (X24 list: "signal capteur AVG",
  front left wheel sensor signal, NOT VERIFIED that the X65 software uses the X24 code list),
  16 km/h, 13.56 V, counted once; slots 2 and 3 empty.
- Keep-alive every 7000 ms (`TesterPresent@Delay`), reply timeout 9300 ms, 300 ms pause
  before each request (`ReplyToRequest@Delay`), all from the X24 definition and accepted by
  the car.

Not yet seen on the car: `45` (fault memory clear), any `41` download or `71` run. `41` and
`71` are forbidden in kline (maintainer decision 2026-11-23, "Always forbidden: Bosch 5.3 ABS
routines" below); whether the ECU releases its outputs on `3F` or keep-alive loss is NOT
VERIFIED.

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
DDT2000, whose parser KWP2000Diag4 checks only the two address bytes; another
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
     and KWP2000Diag4 do the same; only IOTBD32V3's
     fast-init reader does not).
   A read that stops short: **incomplete reply**.
6. Decode and classify: `7F SID NRC` is a negative reply; **`7F SID 78` (responsePending)
   restarts step 5 without a new request**, with a 5000 ms first-byte timeout, at most 10
   times (ours: DDT2000 treats 0x78 as a plain error: RE: KWP2000Diag4 NRC jump table
   sends it to the default error 0x7F8, and nothing reads on; on its D-PDU/VCI path the driver
   handles P2*, independent RE 2 only, not the KKL path).

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
  5-baud branch of KWP2000Ecu init (RE: KWP2000Diag4) it only
  fetches the key bytes; `81` and the KW1 length-byte rule (separate length byte unless KW1 is
  0xD0 or odd) exist only on the fast-init branch. So after a 5-baud init the length stays in
  the format byte whatever KW1 is (`Session::new`).
- Optional `TesterPresentMode::DDT2000_PERIODIC` (`--periodic-tester-present`): `3E` every
  2000 ms since the last TP whatever else is sent, reply required (RE: DdtDiagScrXml,
  "Enable or disable physical TesterPresent request, when idle", `TesterPresent.WithResponse`).

## DDT2000 retry behaviour

Not in DDT2000's protocol layer but in its application; defaults of `kline_core::RetryPolicy`:

| What | Rule | RE |
|---|---|---|
| NRC 0x21 / 0x23 | wait 200 / 500 ms and send again, at most 10 tries (`kline ident`; `--nrc-tries`) | DiagScanXml |
| init | whole variant list up to 3 rounds, 500 ms apart | DdtDiagScrXml, DiagFailures |
| refresh cycle (`live`) | only a cycle where every request failed counts; retry after 400 ms, at most 3 times, then exit 5 | DdtDiagScrXml |

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
| 0 Read | KWP: `3E [x]`, `10 81`, `10 A0` (not on the airbag `2C`), `1A xx`, `21 xx` (exactly 2 bytes, BMT `21 A4` transponder *status* included), `17`/`18` + 1-3 bytes. ISO8 (Safir): `21`, `33 50`, `33 51`, `33 52`, `33 54` (exactly what Safir_1/Safir_2 contain). ISO8 Bosch 5.3 ABS (target ISO8 at 0x01 that answered key bytes `DC 83`): exactly `51` (identification) and `58` (fault memory); `21`/`33` are not reads there. Another ISO8 unit at 0x01 (other key bytes) keeps the generic ISO8 tiers | `Session::request`; autonomous use OK (CLI, `--json`, MCP). Read-only scan via `ScanPlan` (below) |
| 1 Service | (none on the airbag `2C`, see below) KWP: `14 xx xx` clear DTC; not BMT: `30 lid` + 1-4 bytes output control (on a Sirius only exactly `30 id cmd cycles` with the 13 ids of the definition's "Output Temporary Control List" `80 82 83 86 88 8F 94 9E 9F A0 A7 AA AC` as exactly `30 id 00 cc` start with cycles `cc` 1..=10, `30 id 01 00` status or `30 id 11 00` stop; cycles 0 or above 10, other shapes and other ids are Expert), `11 FF` adaptation reset, exactly `32 A0 00` (end of injector inhibition; other `32`, and BMT `30`/`11`/`32`, are Expert, found in a review); Sirius `31 A0 00` injector inhibition (warning `StopsEngine`; "Fonctions particulieres" `31A000` has the function number at byte 2); BMT `31 C7 00 00 00` / `31 C8 00 00 00` remote learning, BMT actuator `31 out param count` with `out` 0xA0..=0xB3 except `A4`/`A5` (unnamed, Expert) and `A6`/`AB`/`AC` (forbidden); Bosch 5.3 ABS (ISO8 at 0x01, `DC 83`) exactly `45` (fault memory clear) | human `Approval` per request; on the ISO8 unit at 0x01 only one approved request per session (`OneApprovedRequestPerSession`) |
| 2 Config write | airbag `2C`: only the dates `3B B0` / `3B C0` + 3 bytes (every other `3B` there is Expert or forbidden, see below); BMT `3B D2 index value` (index 0..=13 except 2, value 0/1; index 3 TRF = remote type RF/infrared, warning `ChangesRemoteType`, also with any other value, then with the text "unknown TRF value"); other KWP ECUs `3B lid` + 1-4 bytes | human `Approval`, plus in the same session: a positive backup read (`21 A1` for BMT `3B D2`, else `21 lid`) and a positive `21 80` whose exact reply the caller says it chose the ECU definition from (BMT config indices differ between BMT_X65_ph1_p and BMT_SAGEM) |
| 3 Expert | any other request that is not forbidden (undocumented service, identifier, parameters or length), including `27` security access (`SecurityAccess`, `MayBrickEcu`), `34`-`37` transfer and `31 81`/`31 82` flash erase/checksum on non-BMT ECUs (`MayBrickEcu`, `MayLeaveProgrammingSession`), `10 xx` other than `10 81`/`10 A0` (`MayLeaveProgrammingSession`), `3D`/`38`-`3A`/`2C`/`23` on non-immobilizer ECUs (maintainer decision 2026-11-22, final) | human *expert* `Approval` (10-character code), `RiskInfo` with "unknown effect" and the specific warnings |
| forbidden | the immobilizer line, the Bosch 5.3 ABS `41`/`71`, the airbag firing-line / crash-data / clear / output / session requests, see below | never, not even in expert mode |

### Always forbidden: the immobilizer

`DenyReason::Immobilizer`, `ImmobilizerNamed`; plus `ProtocolReserved` and `Empty`, which are
not safety rules:

- BMT/UCH/decoder: address 0x26 or 0xA6, or a definition whose `Target@Name`/`<Function Name>`
  says BMT/UCH/UCBIC/decoder/anti-demarrage/immobilizer (`kline_core::spec::ecu_class`), at any
  address and protocol claim: `3D`, `38`, `39`, `3A`, `2C` (memory write / routine by address /
  dynamic define: key memory); `30`/`31`/`32`/`33` with byte 2 in 0xC0..=0xC9 except
  the two exact remote-learning routines; `30`/`31`/`32`/`33` with byte 2 = one of the immobilizer
  outputs `A6`, `AB`, `AC` (both BMT definitions);
  transfer `34`-`37` and flash erase/checksum `31`/`32`/`33` `81`/`82` (key memory; memory read `23` stays Expert); every `2E`/`2F`; every `3B`
  except `3B D2`; `3B D2 02 ..` (key related) and `3B D2 x2 ..` (`12`, `22`, .. `F2`:
  an alias of index 2 if the BMT firmware masks the index to its low nibble,
  NOT VERIFIED; other indices above 0x0F stay Expert); `21 A4` with anything after it.
- Not BMT, any protocol claim (an ISO8 target at 0x7A included, found in a review):
  `31`/`32`/`33`/`38`-`3A` with `A0 A3` (an earlier reading of the backed-up memory reset, kept
  refused).
- Sirius (0x7A or a Sirius definition): `31`/`32`/`33` with `A3` at byte 2, whatever follows
  (function 0xA3, reset of the backed-up memory, may touch the immobilizer data). The Sirius request layout is `31 fn 00` start, `32 fn 00` stop,
  `31 fn 01` status, from `31A000`/`32A000`/`31A001` with the function at FirstByte 2.
- Sirius (0x7A or a Sirius definition): `2E`/`2F`/`3B` on `A0`, `A2`, `AE`, `BD` (the `21`
  frames carrying immobilizer fields). Output control `30` and routines
  `31`-`33`/`38`-`3A` with byte 2 = `A0`/`A2`/`AE`/`BD` are not refused: those are I/O,
  routine and address numbers, not frame identifiers (`30 A0` fast radiator fan, `31 A2 00 00
  00` / `31 A2 01 00 00` start and query the OBD tests). They
  get their normal tier (`30 lid` Tier 1, other routines Expert). The write rule is Sirius
  only: on other ECUs those identifiers mean other things.
- Name guard (`DefinitionGuard`, set on the `Session` by the CLI): Tier 1-3 requests of the loaded
  definition whose names, data items or enumerated parameter values (list texts) look
  immobilizer/transponder/key related
  ([cli.md](cli.md#name-guard)).
- Link frames `81`, `82` (KWP) and `36`, `3F` (ISO8) as requests (`ProtocolReserved`, session
  integrity rather than safety): only the init machines and `Session::tester_present` /
  `Session::stop` send them. ISO8 `36` (keep-alive) and `3F` (stop) are protocol-required
  (RE: KWP2000Diag4 ISO8Ecu init) although no Safir definition lists them (the Bosch 5.3
  definitions list `36` as MES_NOP; it stays reserved).

### Always forbidden: Bosch 5.3 ABS routines (maintainer decision 2026-11-23)

`DenyReason::BoschAbsRoutine { service, names }`, on any ISO8 target at 0x01 whatever key bytes
it answered (the car's Bosch 5.3 answers `DC 83`; another ABS variant must not get `41`/`71` as
Expert either):

- Every `41` (RAM download of 80C196 code) and every `71` (run of the downloaded routine), any
  length. A `71` frame opens inlet / outlet valves or runs the return pump; kline has no
  sequence executor that sends the matching OFF / FIN frame by itself on an error, a lost reply
  or Ctrl-C, and **whether the ECU releases its outputs when it receives `3F` or when the
  keep-alive `36` stops is NOT VERIFIED**. That is the reason for the forbid.
- Named in the refusal: the X24_ABS_Bosch5.3 factory fill (`DOWNLOAD_remplissage_usine`,
  `MES_REMPLISSAGE`, `MES_REMPLISSAGE_Message2`..`8`) and brake bleed (`DOWNLOAD_PURGE`,
  `DOWNLOAD_Y265K14311`, `MES_PURGE_*`: `GARAGE_*`, `SEC_*`, `ECH_*`, `DESACTIVE_*`,
  `CONDITIONS`), downloads by their 9-byte header, `71` frames by exact bytes
  (docs/SAFETY.md "ABS / brake ECU" has the table).
- Not implemented, future path only: a sequence executor that sends only frames pinned by
  exact bytes, in the definition's order, and sends the OFF / FIN frame automatically on any
  error or Ctrl-C. Until it exists and the output release on `3F` / keep-alive loss is
  verified on the car, nothing of this is reachable.

On the ISO8 unit at 0x01 with any key bytes the generic engine forms `11 FF`, `31 A0 00`,
`32 A0 00` and `30 lid` + 1-4 bytes are forbidden (`SafetySystemForbidden`), and `Session`
refuses a second approved request in the same session (`OneApprovedRequestPerSession`), so no
caller can chain a download and a run.

### Always forbidden: airbag ECU (maintainer decision 2026-11-23)

`DenyReason::SafetySystemForbidden { name }` at address 0x2C, any protocol, with or without a
definition (it no longer depends on name-guard keyword matches): `3B D0` (firing line lock /
unlock, passenger airbag inhibit), `3B D2` / `3B D3` (firing line impedance thresholds),
`3B D6` (configuration 2: which airbags, pretensioners, crash output exist), `3B D7`
(robustness counter reset), `3B E0` (crash counter reset, supplier mode); every `14` (fault
clear; on AB80 `14 FF 00` is the EEPROM reset); every `30` (AB80 `30 80` crash output); every
`10` except exactly `10 81` (`10 80` supplier, `10 A0` engineering "BE", `10 B0` production,
`10 C0` after-sales); `11 FF`, `31 A0 00`, `32 A0 00`. Tier 0 there: `21 xx`, `1A xx`,
`17`/`18`, `3E`, `10 81`. Only the dates `3B B0`/`3B C0` + 3 bytes stay Tier 2; `3B D1`/`D4`/`D5`
and any other `3B` are Expert there. With `10 C0` forbidden these writes are unusable until the
maintainer lifts the airbag rule.

Every non-read to the ABS (ISO8 at 0x01) also carries `SafetySystem` first, the brake text
(`RiskInfo::safety_system`) and brake prerequisites (stationary, engine off, handbrake; read
51 and 58 first; afterwards ignition cycle, ABS lamp out, firm pedal, brake test at walking
speed), like the airbag rule.

### RiskInfo

Every non-read request has a machine-readable `RiskInfo { tier, what_it_changes,
what_can_go_wrong, safety_system, reversible, prerequisites, warnings }` (`safety::assess`), from the
definitions and kline-core's own risk texts. Examples: clear DTC "erases the stored fault
codes", `reversible: false`, `ErasesFaultMemory`; BMT `3B D2` "rewrites one BMT configuration
option", `reversible: true` (write the old value from the `21 A1` backup), `EepromWrite`.
`warnings` and `prerequisites` are short inline lists (`StaticList`), so the airbag ECU
(0x2C) can add `SafetySystem`, its text (`safety_system`) and its prerequisites to the
request's own risk without dropping any of it.

### Approvals (feature `approved-routines`)

The feature is **off by default** (`tests/exploits.rs` checks the resolved features).
Without it no approval type or challenge method exists, so a library user, an MCP server or a
`--json` binary can only send Tier 0. Automated, MCP and `--json` callers *prepare* non-read
requests with `safety::assess` / `safety::tier_of`: print the `RiskInfo` and the exact CLI
command for a human, never execute.

Only the interactive `kline` CLI may enable the feature. It must read the confirmation from
`/dev/tty` (not stdin, so a pipe cannot answer) and never offer approvals with `--json`. The code
is an anti-accident gate: an agent driving a pty can type it (allowing that is the user's
choice). `kline-agent` (the read-only binary) depends on `kline-core` without it;
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

- Init variants on other cars and ECUs. On the 1999 Clio II the Sirius and the BMT take plain
  `SingleEcu` and ignore fast init; `MultiEcu` was never needed.
- Effective baud 10400 vs 10417 (10400 works on the Sirius, the BMT and the ABS).
- Break loopback detection on the FTDI cable under Linux and Windows.
- Whether 0x78 (responsePending) ever occurs on these ECUs; it was never seen.
- Whether Safir answers ISO8 `36`, and whether it sends the ISO8 status codes (their meaning
  is now known from DDT2000's errors.xml).
- Bosch 5.3 ABS: what `45` answers and whether it clears; whether the X65 software (51:
  `02 49 12`) has the ROM entry points the X24/J64 download blocks call; whether the X24 fault
  code list applies to this car; whether the ECU releases valves and pump on `3F` or when the
  keep-alive stops (the reason `41`/`71` are forbidden).
- The 5000 ms / 10x responsePending bounds; DTR effect on the KKL cable.
- Fast init on this car: neither 0x7A nor 0x26 answered it.
- Sirius injector inhibition (`31 A0 00`) and the Sirius output tests other than `80`, `94`, `9E`, `9F`, `A0`, `A7`.
- Bosch 5.3 ABS fault clear `45`: implemented (Tier 1), never sent on the car.
- Airbag date writes `3B B0` / `3B C0` (Tier 2): never sent; the dates read `FF FF FF`.
- BMT configuration writes (`3B D2`): none has been sent.
- Whether any ECU needs a wider keyword window than KW2Timeout + 100 (`kw_window_ms`).
- That BMT `3B` identifiers other than `D2`, BMT `2E`/`2F`, and Sirius `2E`/`2F`/`3B` on `A0`,
  `A2`, `AE`, `BD` actually touch immobilizer data: refused as a precaution, not proven.
- Whether `21 A4 xx` has a side effect on the BMT (refused as a precaution, like `21 A1 xx`).
