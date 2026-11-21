# K-line protocol as implemented in clio-core

What `crates/clio-core` sends and expects on the K line, and why. This is a behaviour spec,
not a description of DDT2000's code.

Source tags:

- **RE: IOTBD32V3 ...**: DDT2000's native K-line DLL (serial port, init, echo, reply reading).
- **RE: KWP2000Diag4 ...**: DDT2000's protocol classes (framing, reply checks, per-protocol defaults).
- **XML <K>**: the ECU definition file.
- **ours**: a choice of this project where DDT2000 has no equivalent, or where we deliberately differ.

The reverse-engineering notes themselves are not in this repository. Nothing here is verified
on the car yet. See "Not verified" at the end.

## The car's ECUs

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

## Timing (`KTiming`, all ms)

| Field | IOTBD default | ISO8 class default | KWP class default | Multipoint (`MultiEcu` only) | XML attribute |
|---|---|---|---|---|---|
| `kw2_delay_ms` (W4, before ~KW2) | 25 | 25 (not set by the class) | 25 | forced 25 | `KWP/KW2_@Delay` |
| `first_request_ms` (after 5-baud init) | 20 | 200 | 20 | forced 20 | `ISO8/FirstRequest@Delay` |
| `inter_byte_ms` (TX byte gap) | 0 | 2 | XML | **forced 7** | `InterByte@Delay` |
| `reply_to_request_ms` (P3) | 5 | 100 | 5 | XML | `ReplyToRequest@Delay` |
| `request_to_reply_timeout_ms` (each read) | 300 | 100 | 300 | forced 300 | `RequestToReply@Timeout` |
| `tester_present_ms` (idle before TP) | 3000 | 3000 | 3000 | forced 3000 | `TesterPresent@Delay` |
| `on_error_ms` (gap after an error) | 300 | 1000 | 300 | XML | `OnError@Delay` |
| `kw2_timeout_ms` | 300 | 1000 | 300 | forced 300 | `ISO8/KW2@Timeout` |

Sources: RE: IOTBD32V3 context defaults; RE: KWP2000Diag4 ISO8Ecu / KWP2000Ecu constructors and
the KWP2000Ecu Multipoint init. Class defaults apply only when the XML attribute is absent
(`spec::k_timing`). `LinkConfig::timing` holds the declared values;
`LinkConfig::effective_timing()` applies the Multipoint column only under `MultiEcu`.
XML values are assumed to be ms.

Fixed constants (`timing::consts`):

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

## 5-baud init

Common part (RE: IOTBD32V3 5-baud routine):

1. (ours) idle 300 ms.
2. DTR low. Break on (K low) for 200 ms: the start bit.
3. Check that RX saw the break (the KKL cable loops TX to RX). If not: break off, DTR high,
   error **K line inactive**.
4. 8 data bits of the address byte, **LSB first, no parity bit**, 200 ms each: bit 0 = break
   on, bit 1 = break off.
5. Break off, wait 100 ms (stop bit; reading starts in the middle of it).
6. DTR high. **Flush RX** (break residue and the echo of the bits).
7. Read 3 bytes `55 KW1 KW2` within `kw2_timeout_ms + 100`. Fewer: error **no keywords**.
   The sync byte and the key bytes are reported, not checked (RE: IOTBD32V3 does not check
   them). With `reject_keyword_mismatch` (ours, default off) a mismatch with the definition's
   key bytes fails the init.

Then it depends on the transport flag DDT2000 sets (RE: IOTBD32V3 worker; the nested
`<KWP><ISO8>` element only describes the init, the transport flag stays off for KWP):

- **ISO8 (Safir):** no handshake. Wait `first_request_ms`. Done.
- **KWP 5-baud (Sirius 32, BMT), any variant:** wait `kw2_delay_ms`, send ~KW2
  (one byte, 0x70 for KW2 0x8F), read 2 bytes within the same window: the echo of ~KW2 (must
  match, else **collision**) and ~address (logged, **not** validated; fewer than 2 bytes is
  error **no address complement**). Wait `first_request_ms`. Done.

Example, Sirius 32 single ECU (as tested in `tests/init_traces.rs`): bits of 0x7A, read
`55 D0 8F`, wait 25 ms, send `70`, read `70 xx` (xx logged), wait 20 ms.

## Init variants (`InitVariant`)

The XML marks both the Sirius 32 and the BMT `Multipoint="1"`, but whether DDT2000 actually
uses Multipoint depends on how many ECUs it opens. Two RE sources:

- RE: KWP2000Diag4 KWP2000Ecu Multipoint init: with Multipoint the 5-baud address is
  `address ^ 0x80`, inter-byte 7 ms, and fixed timings (table above).
- RE: KWP2000Diag4 DiagProtocols connect: when a session opens exactly **one** KWP/ISO8 ECU,
  DDT2000 clears Multipoint during the init. A Sirius 32 opened alone gets the plain address
  and the XML inter-byte gap (0).

Both keep the ~KW2 handshake and the 25 ms / 20 ms waits.

| Variant | 5-baud byte (Sirius / BMT) | Inter-byte | Timings | Expected ~address |
|---|---|---|---|---|
| `SingleEcu` (**default**) | 0x7A / 0x26 | XML (0) | XML / class defaults | 0x85 / 0xD9 |
| `MultiEcu` | 0xFA / 0xA6 | 7 ms | fixed Multipoint values | 0x05 / 0x59 |
| `FastInit` (fallback, KWP only) | none: fast init + StartCommunication | XML (0) | XML | none |

`MultiEcu` has no effect on an ECU whose XML has no Multipoint. The 5-baud byte is always
8 raw bits, LSB first, no parity bit (RE: IOTBD32V3).

**The car must confirm which variant each ECU needs.** Test order (alone on the line):
SingleEcu, then MultiEcu, then FastInit; the same for the BMT.

## Fast init (KWP)

RE: IOTBD32V3 fast init: (ours) idle 300 ms, DTR high, 1 ms, break 25 ms, break off 25 ms,
then the StartCommunication exchange below with payload `81`. With KW1 still unknown, the
length goes in the format byte (RE: KWP2000Diag4 LenInFmt default): `81 7A F1 81 6D` for the
Sirius. The reply must be positive `C1 KW1 KW2`; anything else is an error. DDT2000's reader
only handles a length in the format byte; we also accept a separate length byte.

## Framing

Checksum: 8-bit sum of all preceding bytes, both framings (RE: KWP2000Diag4 builders).

**ISO8** (RE: KWP2000Diag4 ISO8 builder and parser): `[N][data..][cs]`, `N = len(data) + 1`,
no addresses. `33 50` -> `03 33 50 86`; TesterPresent `36` -> `02 36 38`; Stop `3F` -> `02 3F 41`.
Reply checks: at least 3 bytes, `N` = byte count - 1, checksum. A one-byte payload 0x15, 0x37,
0x38 or 0x39 is a status reply (`Reply::Iso8Status`, meaning unknown).

**KWP2000** (RE: KWP2000Diag4 KWP builder and parser): `[0x80+n][tgt][F1][data..][cs]` for
`n <= 0x3F`, else `[80][tgt][F1][n][data..][cs]` (`n <= 255`). Target is the ECU address,
never XOR'ed. The separate length byte is also used for short frames when KW1 is neither 0xD0
nor odd (RE: KWP2000Diag4 KWP2000Ecu init); D0 keeps the length in the format byte.
`21 A3` to the BMT -> `82 26 F1 21 A3 5D`; `21 A0` to the Sirius -> `82 7A F1 21 A0 AE`;
TesterPresent `3E` -> `81 26 F1 3E D6`; StopCommunication `82` -> `81 26 F1 82 1A`.
Reply checks: format byte must be physical (`fmt & 0xC0 == 0x80`, ours: stricter than
DDT2000), checksum, byte 2 = 0xF1, byte 3 = ECU address, length consistent with the format or
length byte.

Payloads returned to callers start at the response SID.

## Exchange (one request)

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
   - KWP, `fmt & 0x3F == 0`: read 3 (`tgt src len`), then `len + 1` (ours: DDT2000's reader
     does not handle this).
   A read that stops short: **incomplete reply**.
6. Decode and classify: `7F SID NRC` is a negative reply; **`7F SID 78` (responsePending)
   restarts step 5 without a new request**, with a 5000 ms first-byte timeout, at most 10
   times (ours: DDT2000 treats 0x78 as a plain error).

## Session

- TesterPresent (`36` ISO8, `3E` KWP) is due when the line has been idle for
  `tester_present_ms` since the last exchange. Right after a 5-baud init it is due at once
  (the first frame DDT2000 sends is TP unless a request is queued); after a fast init after
  `tester_present_ms - first_request_ms` (RE: IOTBD32V3 worker). The caller polls
  `tester_present_due(now)` and starts it.
- TP and Stop replies are read when they come and are optional. DDT2000 skips reading them in
  Multipoint (RE: IOTBD32V3 Multicast flag) and discards them otherwise; we read with the
  normal timeout so a late reply cannot collide with the next request, and tolerate silence
  (ours).
- Stop: `3F` for ISO8, StopCommunication `82` for KWP. The session is closed afterwards.
- The engine sends neither StartCommunication nor StartDiagnosticSession by itself after a
  5-baud init; the CLI decides (Sirius 32 definition: `10 A0`). Whether DDT2000's KWP class
  sends StartCommunication after a 5-baud init is not decoded (RE: KWP2000Diag4 KWP2000Ecu;
  a branch first read as an 0x7A special case was later identified as a CAN flag).

## Sans-IO contract

Machines (`FiveBaudInit`, `FastInit`, `Session`) emit one `Action` at a time and need exactly
one `Event` back: `Done` for `SetDtr`, `SetBreak`, `Sleep`, `FlushRx`, `Write`;
`BreakLoopbackSeen(bool)` for `CheckBreakLoopback`; `ReadOk(bytes)` with exactly `count` bytes
or `ReadTimeout(partial)` with fewer for `Read`. Any other event fails the machine
(`UnexpectedEvent`). Time is passed in as `now_ms`; nothing reads a clock.

## Safety

Enforced in `clio-core` (SAFETY.md rule 2 and 3). The session needs a `Permit` from
`safety::check` before an exchange can be built, so nothing reaches `Write` without passing
the policy. Decisions use the exact payload bytes, never request names.

Allowed (everything else is `Denied`):

| Protocol | Payloads |
|---|---|
| KWP | `81`, `82`; `3E [x]`; `10 81` (default) and `10 A0` (Renault diagnostic session, Sirius 32 `Start Diagnostic Session`; orchestrator decision 2026-11-22); `1A [x]`; `17 ..`, `18 ..` (up to 4 bytes); `21 [x]` |
| ISO8 | `21 [x]`; `33 50`..`33 54`; `36` and `3F` as single bytes (TP and Stop) |

Denied in particular: any other `10 xx` (0x85/0x86 programming etc.), 0x11, 0x14 (clear
DTC), 0x20, 0x27, 0x2E, 0x2F, 0x30 (output control), 0x31 (all routines, including Sirius
`31 A2 01 00 00` OBDFC status), 0x32, 0x34-0x37 on KWP, 0x3B, 0x40, 0x41, 0x45, 0x46, 0x61,
0xFF, empty payloads, longer variants of allowed reads, and **BMT `21 A4`** (transponder state,
rule 3; `21 A4` on the Sirius is an ordinary data frame and is allowed).

The only exception: BMT (`0x26`, KWP) routines `31 C7 00 00 00` (erase all remotes, learn
remote 1) and `31 C8 00 00 00` (learn remote 2), via `Session::request_approved` with an
`Approval`. `Approval` is not `Clone`/`Copy`/`Default`/deserializable and is consumed by use.
Its only constructor is `Approval::from_typed_confirmation(routine, typed)`, which needs the
exact phrase from `ApprovedRoutine::confirmation_phrase()`.

CLI usage (only in interactive mode, never with `--json`, never from MCP, never in tests against
a real port): print what the routine does and its phrase, read one line from the terminal,
pass it to `from_typed_confirmation`, and hand the `Approval` to `request_approved`.

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
