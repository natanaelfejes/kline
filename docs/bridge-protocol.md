# Clio Link bridge: BLE GATT protocol v1, roles and approvals

The Clio Link bridge is an ESP32-C3 between the car's K line and Bluetooth Low Energy. It is the
only K-line master in the car; the phone app and the Mac (`kline --transport ble`) talk to it
over one GATT service. This page is the protocol spec: characteristics, frame format, every
message with its bytes, roles, how Tier 1 requests are approved, lock-when-moving, and test
vectors. The code is `crates/kline-bridge` (`gatt`, `gate`, `bridge`); the hardware is in
[bridge-hardware.md](bridge-hardware.md).

**Nothing here has run on the real bridge or in the car yet.** It runs on the host against
kline-sim (`crates/kline-bridge/tests`). See [Not verified](#not-verified).

## Contents

- [Service and characteristics](#service-and-characteristics)
- [Frame](#frame)
- [Live](#live)
- [Status](#status)
- [Command](#command)
- [Response and result codes](#response-and-result-codes)
- [Raw](#raw)
- [Roles, pairing, counters, rate limits](#roles-pairing-counters-rate-limits)
- [Approvals (Tier 1 from the phone)](#approvals-tier-1-from-the-phone)
- [Parked LOCK and UNLOCK (fob output)](#parked-lock-and-unlock-fob-output)
- [Lock-when-moving](#lock-when-moving)
- [Bridge behaviour](#bridge-behaviour)
- [Host transport (`kline --transport ble`)](#host-transport-kline---transport-ble)
- [Test vectors](#test-vectors)
- [Limits](#limits)
- [kline-core changes needed](#kline-core-changes-needed)
- [Not verified](#not-verified)

## Service and characteristics

One custom 128-bit service (PLAN-RESEARCH C2). All characteristics require an encrypted,
authenticated (LE Secure Connections, MITM) link from a bonded peer.

| Characteristic | UUID | Properties | Content |
|---|---|---|---|
| Service | `b3340ce9-0893-4302-b20a-d4f96eb056ee` | | |
| Live | `b3340cea-0893-4302-b20a-d4f96eb056ee` | notify | [Live](#live) |
| Status | `b3340ceb-0893-4302-b20a-d4f96eb056ee` | read, notify | [Status](#status) |
| Command | `b3340cec-0893-4302-b20a-d4f96eb056ee` | write with response | [Command](#command) |
| Response | `b3340ced-0893-4302-b20a-d4f96eb056ee` | notify, to the writing client only | [Response](#response-and-result-codes) |
| Raw | `b3340cee-0893-4302-b20a-d4f96eb056ee` | notify, off until `RAW_STREAM 1` | [Raw](#raw) |
| Device Information 0x180A | standard | read | firmware version |

## Frame

Every value on every characteristic is one frame:

```text
[version u8 = 0x01][type u8][N u8][body: N bytes][CRC u16 little endian]
```

- `type` is the characteristic: `01` Live, `02` Status, `03` Command, `04` Response, `05` Raw.
- `N` is the body length; the frame is exactly `3 + N + 2` bytes.
- CRC-16/CCITT-FALSE (poly 0x1021, init 0xFFFF, not reflected, no final XOR; check value of
  `"123456789"` is `0x29B1`) over version, type, N and body.
- All multi-byte fields in bodies are little endian.
- A frame is at most 180 bytes, so the central must negotiate an ATT MTU of at least 183
  (iOS and macOS negotiate 185 or more by themselves). No fragmentation in v1.
- A frame with a bad CRC, another version, a wrong length or an unknown type is refused as a
  whole; nothing in it is executed (Response `BadFrame` with counter 0).

## Live

Notified after every decoded Sirius `21AA` or `21A9` reply (drive profile), 20-byte body
(PLAN-RESEARCH C2):

| Offset | Field | Type | Meaning |
|---|---|---|---|
| 0 | seq | u16 | +1 per notification |
| 2 | t_ms | u32 | bridge uptime when decoded |
| 6 | rpm | u16 | engine speed |
| 8 | speed_kmh | u8 | vehicle speed (21A9 d9) |
| 9 | inj_us | u16 | injection time, us (21AA d5..6 x 4) |
| 11 | fuel_mlh | u16 | consumption, mL/h (21AA d10..11 x 10) |
| 13 | coolant_c | i8 | coolant, C (21A9 d5 x 0.625 - 40) |
| 14 | iat_c | i8 | intake air, C (21A9 d6 x 0.625 - 40) |
| 15 | map_mbar | u16 | manifold pressure (21A9 d3 x 3.7 + 103) |
| 17 | batt_cv | u16 | battery, centivolts (21A9 d2 x 0.032 + 8 V) |
| 19 | flags | u8 | 0x01 21AA valid, 0x02 21A9 valid, 0x04 engine running, 0x08 lock-when-moving armed, 0x10 doors locked by lock-when-moving |

Byte positions and scales are from the 2026-11-22 car captures (SIRIUS-ANALYSIS "21AA
payload", "21A9 payload"); `d[i]` is data byte `i` after `61 lid`.

## Status

Notified on change and every second while a client is connected; also the answer to
`GET_STATUS` (in Response data). 14-byte body:

| Offset | Field | Type | Meaning |
|---|---|---|---|
| 0 | fw_major | u8 | |
| 1 | fw_minor | u8 | |
| 2 | state | u8 | 0 idle, 1 init, 2 session, 3 backoff, 4 yielded, 5 sleep |
| 3 | ecu | u8 | address of the session or init, 0 none |
| 4 | profile | u8 | 0 off, 1 drive, 2 parked, 3 diagnostics |
| 5 | batt_cv | u16 | bridge's own divider, centivolts, 0 = not measured |
| 7 | lock | u8 | 0 unknown, 1 locked, 2 unlocked (BMT `21A3` d0 bit 7) |
| 8 | flags | u8 | 0x01 lwm armed, 0x02 lwm locked, 0x04 engine running, 0x08 approvals in this build, 0x10 front door open, 0x20 rear door open, 0x40 battery below 12.0 V |
| 9 | last_error | u8 | last non-OK [result code](#response-and-result-codes) |
| 10 | uptime_s | u32 | |

## Command

Body: `[counter u32][opcode u8][arguments]`. The counter must be greater than the last one the
bridge accepted from this bond ([counters](#roles-pairing-counters-rate-limits)).

| Opcode | Name | Arguments | Who | What |
|---|---|---|---|---|
| `01` | START_LIVE | profile u8 (1 drive, 2 parked) | owner, agent | poll this profile continuously (parked: `LwmActive` while lock-when-moving needs the Sirius) |
| `02` | STOP_LIVE | | owner, agent | stop polling |
| `03` | REQUEST | ecu u8 (`7A`, `26`, `01`, `2C`), payload (1..32 bytes) | owner, agent | one request; must be Tier 0 |
| `04` | PREPARE | ecu u8, payload | owner | approval challenge for one Tier 1, 2 or 3 request |
| `05` | CONFIRM | challenge id u32, code (1..16 printable ASCII) | owner | send the prepared request |
| `06` | LOCK | | owner | lock the parked car with the fob output |
| `07` | UNLOCK | | owner | unlock the parked car with the fob output |
| `08` | YIELD_KLINE | | owner, agent | take a lease on the line: `Accepted`, then `Ok` once the session is stopped and the transceiver is off (`LwmActive` while lock-when-moving needs the Sirius) |
| `09` | RESUME_KLINE | | the client that yielded, or an owner | end the lease, take the K line back |
| `0A` | SET_TIME | Unix seconds u32 | owner, agent | for logs only |
| `0B` | LWM_ARM | | owner | arm lock-when-moving (two challenges) |
| `0C` | LWM_DISARM | | owner | disarm |
| `0D` | RAW_STREAM | 0 or 1 | owner, agent | Raw notifications off or on |
| `0E` | GET_STATUS | | owner, agent | Status frame in the Response data |

ECU bytes: `7A` Sirius, `26` BMT, `01` Bosch 5.3 ABS (ISO8), `2C` airbag (owner decision
2026-11-23: BLE scope v1 is everything the CLI can do for these four). Any other byte does not
decode. While lock-when-moving needs the Sirius (armed with the engine running, or an unlock
owed), REQUEST and PREPARE for another ECU answer `LwmActive`.

## Response and result codes

Body: `[counter u32][result u8][data]`, sent only to the client that wrote the command.
Commands that need the K line first get `Accepted` (queued), then a second Response with the
same counter and the final result.

| Code | Name | Data |
|---|---|---|
| `00` | Ok | REQUEST/CONFIRM: the positive reply payload (from the response SID) |
| `01` | Accepted | a final Response follows |
| `02` | BadFrame | counter 0 |
| `03` | StaleCounter | |
| `04` | RateLimited | |
| `05` | NotAllowedForRole | agent: anything but Tier 0; unbonded: everything |
| `06` | Forbidden | immobilizer class or protocol frame: never, in any tier |
| `07` | ApprovalRequired | not Tier 0: owner must PREPARE and CONFIRM |
| `08` | ApprovalMismatch | wrong code, other/expired challenge, or kline-core refused the approval |
| `09` | LowBattery | battery below 12.0 V or not measured |
| `0A` | Busy | queue full, or another challenge pending |
| `0B` | LinkError | init or exchange failed |
| `0C` | NegativeReply | the ECU's `7F sid nrc` |
| `0D` | Yielded | |
| `0E` | LockNotConfirmed | lock state unknown, or `21A3` did not follow the pulse |
| `0F` | Unsupported | not in this build (no `approvals`, no `standing-lock`), or PREPARE of a read |
| `10` | Challenge | challenge id u32, tier u8 (1 service, 2 config write, 3 expert), code ASCII (6 characters, 10 for expert) |
| `11` | IgnitionOn | fob path refused: engine running or charging voltage |
| `12` | VehicleMoving | a non-read while a fresh Sirius speed is above 0, or on the Sirius with the engine running and no fresh speed |
| `13` | LwmActive | lock-when-moving needs the Sirius now |
| `14` | ReplyTooLong | the ECU reply does not fit one Response frame (never truncated) |

A Response never carries more than 170 data bytes; a longer reply gives `ReplyTooLong`. Raw
splits long reads into several frames.

## Raw

Off by default. Body: `[t_ms u32][dir u8: 0 tx, 1 rx][ecu u8][bytes]`, the same meaning as a
kline JSONL capture line (rx includes the echo). Broadcast to every client that subscribed.

## Roles, pairing, counters, rate limits

- **Roles** are fixed per bond at pairing time (PLAN-RESEARCH C3; max 2 bonds): the phone is
  **owner**, the Mac is **agent**. The firmware passes the role of the writing bond to
  `Bridge::on_command`; nothing in a frame can claim a role. An unbonded or unencrypted peer is
  `Unbonded` and gets `NotAllowedForRole` for everything.
- **Agent = Tier 0 only, always** (MASTER-PLAN-v2 M12): REQUEST of a read, live profiles,
  status, yield. PREPARE, CONFIRM, LOCK, UNLOCK and LWM_ARM are refused for the role before any
  tier check, and kline-core's `Session::request` refuses any non-read again.
- **Counters**: per bond, strictly increasing; the bridge refuses a counter that is not
  greater than the last accepted one (`StaleCounter`). The firmware stores the last accepted
  value per bond in NVS (`Bridge::counters`, `restore_counters`; W7), so a recorded write
  cannot be replayed after a reset. The host client starts at the Unix time in seconds. A bond
  that reached `u32::MAX` accepts nothing more until it bonds again (`Bridge::on_bond` forgets
  its counter).
- **Unbonded peers** are not clients: no notification is addressed to them and they do not
  keep the bridge awake. The firmware sends broadcast notifications to bonded, subscribed peers
  only.
- **Yield lease**: `YIELD_KLINE` records who yielded. Only that client or an owner can
  `RESUME_KLINE`; a running engine never takes the line back by itself. The firmware saves the
  holder in NVS and restores it at boot (`Bridge::yield_holder`, `restore_yield`; W7), so a
  reset never makes a second master on the line.
- **Rate limits** (ours): 20 commands per second in total; LOCK/UNLOCK at most 6 per 10 minutes
  and at least 5 s apart (MASTER-PLAN-v2 3.1 rule 4).

## Approvals (Tier 1 to 3 from the phone)

Build feature `approvals` (off by default; SAFETY.md rule 2 "Bridge firmware"). Owner decision
2026-11-23: the owner gets Tier 1, Tier 2 and Expert over BLE with the same codes and flow as the
CLI; forbidden stays forbidden; the agent stays Tier 0.

1. Owner writes `PREPARE ecu payload`. The bridge checks: owner role, the kline-core tier with
   the bridge's guard ([Limits](#limits)) is 1, 2 or 3 (forbidden is `Forbidden`), battery at
   or above 12.0 V (MASTER-PLAN-v2 P6), and the car is not moving (`VehicleMoving`).
2. The bridge opens (or keeps) a session with that ECU and asks kline-core for the challenge
   of the request's tier on the session's real target: `service_challenge`,
   `config_write_challenge` (Tier 2; the identification is the session's latest `21 80`
   reply, so the app reads `21 80` and the backup `21 xx` with REQUEST first and chooses its
   definition from that reply, like the CLI) or `expert_challenge` (10-character code).
   kline-core binds the code to that session, request id, target, key bytes, bytes, tier and
   `RiskInfo`.
3. Response `Challenge`: id, tier, code, to the owner's phone only (never broadcast, never in
   Raw). The app shows the exact bytes, the `RiskInfo` text (the app computes it with the same
   kline-core function, `safety::assess`) and the code; the owner types the code.
4. Owner writes `CONFIRM id code`. The bridge gives the typed code to kline-core
   `Approval::confirm` and sends with `Session::request_approved`, which re-checks everything.
   The challenge is consumed by the first CONFIRM, right or wrong, and expires after 120 s.
   While it is pending the session stays open (TesterPresent) on that ECU.

**What the code is:** the same anti-accident gate as the CLI's `/dev/tty` code (docs/SAFETY.md rule
2): it stops the app, a script or the agent link from sending a Tier 1 request by mistake. It
is not an authentication against software on the owner's phone, which sees the code. What
keeps an agent out is the role of its bond, not the code.

## Parked LOCK and UNLOCK (fob output)

For the parked car (MASTER-PLAN-v2 3.1 "Bonus" and PLAN-RESEARCH B6): a PhotoMOS across the
button of remote B. The remote has one button, so a press toggles; the bridge therefore never
pulses blind:

1. Refused unless owner, battery at or above 12.0 V, engine not running and not charging (the
   BMT ignores RF with the ignition on), not yielded, and within the rate limit.
2. Read BMT `21A3` (a value read after the command). Unknown: `LockNotConfirmed`. Already in
   the target state: `Ok`, no pulse.
3. One 300 ms pulse (`FobDriver`, at least 5 s between pulses).
4. Re-read `21A3` until it shows the target state (`Ok`) or 4 s pass (`LockNotConfirmed`). No
   second pulse.

The output is driven low before anything else at boot, ends every pulse low, is driven low on
drop, and has an external pull-down for the time the GPIO floats in a reset
([bridge-hardware.md](bridge-hardware.md#pinout)). Tests: `tests/fob_safety.rs`.

## Lock-when-moving

Automaton `kline_bridge::lwm` (MASTER-PLAN-v2 3.1):

- Lock once when the Sirius `21A9` speed is above 15 km/h for 3 s, from fresh samples only (a
  gap of more than 1.5 s between samples restarts the 3 s; review M5).
- Lock again only after more than 60 s at speed 0, or after an owner UNLOCK and a new
  drive-off.
- Unlock when rpm is 0, or when the bridge has *tried* the Sirius and failed for 3 s with no
  sample in between (ignition off). Samples that simply stop because the bridge is busy
  elsewhere do not count (review B1).
- 5 s between actuations; off by default; armed until the owner disarms in the app (owner
  decision 2026-11-23; a reset disarms, since arming is in RAM).
- Once it has (possibly) locked, it still unlocks at engine stop after a disarm. A lock that was
  written but whose reply is lost counts as possibly executed and keeps the unlock duty (review
  M4); a lock that was not written or that the BMT refused is not retried. A failed unlock is
  retried up to 3 times.
- While it needs the Sirius (armed with the engine running, or an unlock owed) the bridge stays
  on the drive profile even without a charging voltage and refuses parked START_LIVE, REQUEST or
  PREPARE for another ECU and YIELD_KLINE (`LwmActive`), so no client can make it leave the
  Sirius and toggle the locks (review B1). It never sleeps on a low battery while an unlock is
  owed.

Execution, decision D37 (owner decision, recorded in the owner's DECISIONS.md; build feature
`standing-lock`, off by default): the bridge stops the Sirius session, opens the BMT, sends
`31 A0 00 01` (lock) or `31 A1 00 01` (unlock), stops the BMT and goes back to the Sirius. Both
are Tier 1 BMT actuator requests. Arming (`LWM_ARM`) needs the owner's code for each of the two
requests (two challenges on a BMT session, nothing is sent then). Later the bridge answers
kline-core's challenge itself, only for exactly those payloads, on the BMT, with the `RiskInfo`
the owner confirmed (`standing_allows`; SAFETY.md rule 2 "Standing self-approval"). The owner
accepts that the remote's lock state may get out of step. The parked fob path stays available
for when remote B is repaired.

## Bridge behaviour

- **Profiles**: drive (Sirius `21AA, 21A9, 21AA, 21AA`), parked (BMT `21A3, 21A0, 21A2`), and
  diagnostics on request (REQUEST/PREPARE open a session with their ECU; it ends when nothing
  needs it). Drive is chosen by itself when the battery reads 13.2 V or more or the engine
  runs.
- **One master**: one kline-core `Session` at a time; a session with another ECU starts only
  after `82` StopCommunication. After a session that ended without a clean Stop (link errors,
  port failure) the next init of any ECU waits P3max (5 s), so the old ECU has timed out.
  `YIELD_KLINE` stops and switches the transceiver off, and a yield always wins over the
  automatic drive profile.
- **Keep-alive**: kline-core's TesterPresent when idle (3 s).
- **Reconnect**: after a failed init 0.5 s doubling to 10 s, or P3max 5 s when the ECU
  answered; after 3 failed exchanges in a row the session is dropped and re-initialised.
- **Sleep** (`Output::Sleep` carries the reason): below 12.0 V for 10 s with the engine off
  (`LowBattery`, wake after 1 h), below 11.8 V for 5 s (`CriticalBattery`, MASTER-PLAN-v2 P6),
  and after 2 min with no client and nothing to do (`Idle`, wake after 60 s). The debounce
  ignores cranking dips and ADC noise (review M3). Never with the engine running and never while
  lock-when-moving owes an unlock. The firmware sleeps in light-sleep slices that feed the
  watchdog, never in a busy-wait.
- **Moving**: no non-read to any ECU while a fresh (2 s) Sirius speed is above 0, and none to
  the Sirius while the engine runs without a fresh speed (`VehicleMoving`; Sirius `31 A0 00`
  injector inhibition, `30 80` fuel pump and the like). The D37 requests are the exception.

## Host transport (`kline --transport ble`)

`kline --transport ble read --ecu sirius 21A9 21AA` (cargo feature `ble` of kline-cli /
kline-host): the requests are resolved and checked as reads, sent as REQUEST through the bridge
(`kline_host::ble::BridgeClient`), and decoded with the definition as usual (JSON events carry
`"transport": "ble"`). Every other command with `--transport ble` is a usage error: the bridge
owns the K line, so there is no break, echo or init on the host. A build without the feature
says so. To use the KKL cable while the bridge is in the car, send `YIELD_KLINE` first (one
tool on the cable).

The radio backend is the `BleLink` trait. **No real backend is compiled in yet**: `btleplug`
needs `libdbus-1-dev` (pkg-config `dbus-1`) on Linux, which is not on the build machine, so it
was not added; on macOS it uses CoreBluetooth and needs no system library. W7/M12: add a
`ble-btleplug` feature with a `BleLink` over btleplug, built on the Mac. Tests use a link to a
kline-bridge running against kline-sim in the same process
(`crates/kline-host/tests/support/ble_sim.rs`).

## Test vectors

Checked by `crates/kline-bridge/tests/vectors.rs`; the CRCs were cross-checked with Python's
`binascii.crc_hqx(data, 0xFFFF)`.

| What | Frame (hex) |
|---|---|
| Command #1 START_LIVE drive | `01 03 06 01 00 00 00 01 01 B1 FC` |
| Command #2 REQUEST Sirius `21 A9` | `01 03 08 02 00 00 00 03 7A 21 A9 D1 8E` |
| Command #3 PREPARE Sirius `14 00 00` | `01 03 09 03 00 00 00 04 7A 14 00 00 47 CB` |
| Command #4 CONFIRM challenge 7, code `ACDEFG` | `01 03 0F 04 00 00 00 05 07 00 00 00 41 43 44 45 46 47 93 EE` |
| Command #5 LOCK | `01 03 05 05 00 00 00 06 FF AE` |
| Response #2 Ok `61 A9 04 35` | `01 04 09 02 00 00 00 00 61 A9 04 35 FB 93` |
| Response #3 Challenge id 7, tier 1, `ACDEFG` | `01 04 10 03 00 00 00 10 07 00 00 00 01 41 43 44 45 46 47 6A D0` |
| Live seq 1, t 12345, 1077 rpm, 0 km/h, 7412 us, 1400 mL/h, 19 C, 15 C, 406 mbar, 14.33 V, flags 07 | `01 01 14 01 00 39 30 00 00 35 04 00 F4 1C 78 05 13 0F 96 01 99 05 07 9A EA` |
| Status fw 0.1, session, 7A, drive, 14.10 V, lock unknown, engine running, up 60 s | `01 02 0E 00 01 02 7A 01 82 05 00 04 00 3C 00 00 00 3B A7` |
| Raw tx at 2500 ms, Sirius, `82 7A F1 21 A9 B7` | `01 05 0C C4 09 00 00 00 7A 82 7A F1 21 A9 B7 4C 33` |
| Bad CRC (last byte of #1 + 1) | `01 03 06 01 00 00 00 01 01 B1 FD` -> BadFrame |

## Limits

- The firmware has no Renault definition. kline-core's **name guard** (Tier 1-3 requests whose
  definition names look immobilizer related) is compiled in as byte prefixes per ECU address,
  the union over the definitions the CLI loads for this car (`kline_bridge::guard_rules`; no
  names, no XML). `tests/definition_parity.rs` rebuilds the table from `ddt-xml/` and checks
  that the bridge decides every request of those definitions (and its non-read variant with one
  more byte) exactly as the CLI with the definitions loaded: 504 requests on 2026-11-23. It is
  skipped when `ddt-xml/` is absent; when the owner adds a definition, run it again.
- The link settings (`EcuId::link_config`) are the definitions' `<K>` values, checked by the
  same test (airbag: AB70's, the slower one).
- The decoders in `kline_bridge::live` are hand-coded positions for this car's ECUs.

## kline-core changes (made in this round)

1. `approved-routines` no longer implies `std` (`approved-routines = ["dep:getrandom"]`). The
   firmware builds with `--cfg getrandom_backend="custom"` and provides
   `__getrandom_v03_custom` from the ESP32-C3 hardware RNG (`firmware/esp32c3`, feature
   `approvals`; true randomness needs an entropy source running, the radio in W7, NOT
   VERIFIED).
2. `DefinitionGuard::flagged()`: read access to the flagged prefixes, for the parity test.

Not made: a kline-core `StandingApproval` type. The owner allowed the bridge's standing
self-approval for exactly the two D37 payloads (SAFETY.md rule 2); a core type would move that
check into kline-core and is a possible later cleanup.

## Not verified

- Everything on real hardware: ESP32-C3 UART at 10400, the BREAK_N bit-bang timing, break
  loopback through the L9637D, BLE MTU and throughput, NVS counters (not written yet).
- That the car's BMT updates its lock memory (`21A3` d0 bit 7) after K-line `31 A0`/`31 A1`:
  the 2026-11-22 notes say it does not (CONTEXT.md, PLAN-RESEARCH B6), which would make D37
  desynchronise the remote's state; the simulator models it as updating.
- The `21A3` polarity (0 locked, 1 unlocked) and the `21A0` door bits (BMT-ANALYSIS).
- The fob path: that one press of remote B toggles the lock with the ignition off, and how
  fast `21A3` follows it.
