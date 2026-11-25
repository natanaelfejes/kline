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
- [Extra (live extras)](#extra-live-extras)
- [Body read at a stop](#body-read-at-a-stop)
- [Battery estimate](#battery-estimate)
- [Roles, pairing, counters, rate limits](#roles-pairing-counters-rate-limits)
- [Approvals (Tier 1 to 3 from the phone)](#approvals-tier-1-to-3-from-the-phone)
- [Parked LOCK and UNLOCK (fob output)](#parked-lock-and-unlock-fob-output)
- [Lock-when-moving](#lock-when-moving)
- [Bridge behaviour](#bridge-behaviour)
- [Power states](#power-states)
- [Host transport (`kline --transport ble`)](#host-transport-kline---transport-ble)
- [Test vectors](#test-vectors)
- [Limits](#limits)
- [kline-core changes for the bridge](#kline-core-changes-for-the-bridge)
- [Not verified](#not-verified)

## Service and characteristics

One custom 128-bit service. All characteristics require an encrypted,
authenticated (LE Secure Connections, MITM) link from a bonded peer.

| Characteristic | UUID | Properties | Content |
|---|---|---|---|
| Service | `b3340ce9-0893-4302-b20a-d4f96eb056ee` | | |
| Live | `b3340cea-0893-4302-b20a-d4f96eb056ee` | notify | [Live](#live); also [Extra](#extra-live-extras) frames while live extras are on |
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

- `type` is the message: `01` Live, `02` Status, `03` Command, `04` Response, `05` Raw, each on
  its own characteristic, and `06` Extra, on the Live characteristic (only while live extras are
  on, so a client that never sends `LIVE_EXTRAS` never sees one).
- `N` is the body length; the frame is exactly `3 + N + 2` bytes.
- CRC-16/CCITT-FALSE (poly 0x1021, init 0xFFFF, not reflected, no final XOR; check value of
  `"123456789"` is `0x29B1`) over version, type, N and body.
- All multi-byte fields in bodies are little endian.
- A frame is at most 180 bytes, so the central must negotiate an ATT MTU of at least 183
  (iOS and macOS negotiate 185 or more by themselves). No fragmentation in v1.
- A frame with a bad CRC, another version, a wrong length or an unknown type is refused as a
  whole; nothing in it is executed (Response `BadFrame` with counter 0).

## Live

Notified after every decoded Sirius `21AA` or `21A9` reply (drive profile), 20-byte body:

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
| 19 | flags | u8 | 0x01 21AA valid, 0x02 21A9 valid, 0x04 engine running, 0x08 lock-when-moving armed, 0x10 doors locked by lock-when-moving, 0x20 live extras on |

Byte positions and scales are from the 2026-11-22 car captures; `d[i]` is data byte `i` after `61 lid`.

## Status

Notified on change and every second while a client is connected; also the answer to
`GET_STATUS` (in Response data). 16-byte body (firmware before 2026-11-25 sent the first 14
bytes; decoders accept both, the battery estimate is then unknown):

| Offset | Field | Type | Meaning |
|---|---|---|---|
| 0 | fw_major | u8 | |
| 1 | fw_minor | u8 | |
| 2 | state | u8 | 0 idle, 1 init, 2 session, 3 backoff, 4 yielded, 5 sleep |
| 3 | ecu | u8 | address of the session or init, 0 none |
| 4 | profile | u8 | 0 off, 1 drive, 2 parked, 3 diagnostics |
| 5 | batt_cv | u16 | bridge's own divider, centivolts, 0 = not measured |
| 7 | lock | u8 | 0 unknown, 1 locked, 2 unlocked (BMT `21A3` d0 bit 7) |
| 8 | flags | u8 | 0x01 lwm armed, 0x02 lwm locked, 0x04 engine running, 0x08 approvals in this build, 0x10 front door open, 0x20 rear door open (BMT `21A0` d2 0x02 / 0x04, last parked read), 0x40 battery below 12.0 V, 0x80 lock-when-moving gave up an unlock and disarmed itself (doors may be locked; cleared only by arming again or LWM_ACK) |
| 9 | last_error | u8 | last non-OK [result code](#response-and-result-codes) |
| 10 | uptime_s | u32 | |
| 14 | soc_pct | u8 | battery state of charge estimated by the bridge, percent; `FF` unknown ([Battery estimate](#battery-estimate)) |
| 15 | soc_conf | u8 | 0 none, 1 approximate (rested 1 to 4 h, or still settling), 2 rested (4 h or more, settled) |

## Command

Body: `[counter u32][opcode u8][arguments]`. The counter must be greater than the last one the
bridge accepted from this bond ([counters](#roles-pairing-counters-rate-limits)).

| Opcode | Name | Arguments | Who | What |
|---|---|---|---|---|
| `01` | START_LIVE | profile u8 (1 drive, 2 parked) | owner, agent | poll this profile continuously (parked: `LwmActive` while lock-when-moving needs the Sirius) |
| `02` | STOP_LIVE | | owner, agent | stop polling |
| `03` | REQUEST | ecu u8 (`7A`, `26`, `01`, `2C`), payload (1..32 bytes) | owner, agent | one request; must be Tier 0 |
| `04` | PREPARE | ecu u8, payload | owner | approval challenge for one Tier 1 or Tier 3 request (Tier 2: `IdentificationRequired`) |
| `05` | CONFIRM | challenge id u32, code (1..16 printable ASCII) | owner | send the prepared request |
| `06` | LOCK | | owner | lock the parked car with the fob output |
| `07` | UNLOCK | | owner | unlock the parked car with the fob output |
| `08` | YIELD_KLINE | | owner, agent | take a lease on the line: `Accepted`, then `Ok` once the session is stopped and the transceiver is off (`LwmActive` while lock-when-moving needs the Sirius) |
| `09` | RESUME_KLINE | | the client that yielded, or an owner | end the lease, take the K line back |
| `0A` | SET_TIME | Unix seconds u32 | owner, agent | for logs only |
| `0B` | LWM_ARM | | owner | arm lock-when-moving (two challenges); `LwmActive` while armed or while an unlock is owed (disarm first, and wait for the unlock), `VehicleMoving` unless parked with the engine off (the moving rule for a BMT request) |
| `0C` | LWM_DISARM | | owner | disarm; with an unlock owed the bridge then reads BMT `21A3` once (one such read queued at a time); only an observed locked-then-unlocked change ends the duty |
| `0D` | RAW_STREAM | 0 or 1 | owner, agent | Raw notifications off or on |
| `0E` | GET_STATUS | | owner, agent | Status frame in the Response data |
| `0F` | PREPARE_WRITE | ecu u8, identification length u8 (1..64), identification (the `61 80 ..` reply the app chose its definition from), payload | owner | approval challenge for one Tier 2 config write |
| `10` | LWM_ACK | | owner | acknowledge a lock-when-moving give-up: clears status flag `0x80`; nothing else changes (still disarmed) |
| `11` | LIVE_EXTRAS | every u8 (0 off, 1..16) | owner, agent | the "advanced" drive profile: one extra Sirius read after every `every` drive cycles and the raw replies as [Extra](#extra-live-extras) frames; reads only; `Ok` at once; a value above 16 does not decode (`BadFrame`) |
| `12` | READ_BODY_ONCE | | owner, agent | one BMT body read at a stop with the engine running ([Body read at a stop](#body-read-at-a-stop)): `Accepted`, then `Ok` once BMT `21A0` and `21A3` are read and sent as Extra frames; `Ok` at once when the drive profile does not run (the parked profile reads the body anyway); `VehicleMoving` unless a fresh Sirius speed is 0; `LwmActive` while lock-when-moving owes an unlock; `RateLimited` for a second one in the same stop |

ECU bytes: `7A` Sirius, `26` BMT, `01` Bosch 5.3 ABS (ISO8), `2C` airbag (maintainer decision
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
| `12` | VehicleMoving | a non-read while the car may move or the engine may run, by the moving rule ([Bridge behaviour](#bridge-behaviour)), at PREPARE, when its job runs and again at CONFIRM; also when the Sirius sample taken before a PREPARE does not come |
| `13` | LwmActive | lock-when-moving needs the Sirius now; also the CONFIRM of a challenge dropped because the car may have started while armed |
| `14` | ReplyTooLong | the ECU reply does not fit one Response frame (never truncated) |
| `15` | SessionLost | the session the challenge was made on ended (line error, keep-alive lost); nothing was sent, PREPARE again |
| `16` | IdentificationRequired | a Tier 2 config write needs PREPARE_WRITE |

A Response never carries more than 170 data bytes; a longer reply gives `ReplyTooLong`. Raw
splits long reads into several frames.

## Raw

Off by default. Body: `[t_ms u32][dir u8: 0 tx, 1 rx][ecu u8][bytes]`, the same meaning as a
kline JSONL capture line (rx includes the echo). Broadcast to every client that subscribed.

## Extra (live extras)

The "advanced" drive profile, for the app's advanced dashboard and alerts. Off by default and
after every reset (the setting is in RAM); `LIVE_EXTRAS every` turns it on (1..16) or off (0)
for every client, and it applies whenever the drive profile runs (requested or automatic).
Reads only: every extra request is a Tier 0 `21 lid` that kline-core's policy checks like every
poll.

- The drive schedule `21AA, 21A9, 21AA, 21AA` is unchanged; after every `every` completed
  cycles one read of the extra list follows, in turn: Sirius `21A0` (input faults, present and
  stored), `21A1` (output faults), `21A2` (input and state flags), `21A4` (output states: fan,
  fuel pump relay, warning lamps), `21A8` (fault summary), `21AB` (idle regulation), `21AD`
  (OBD status: misfire, oxygen sensor heaters). The other Sirius frames are copies of these,
  constant, or not useful (2026-11-22 drive analysis) and are never read. Lock-when-moving keeps
  its `21A9` samples: with `every` 1 a cycle takes one read more (about 25 % longer between two
  `21A9`), well inside its 1.5 s sample gap.
- A frame the ECU answers negatively, or does not answer, is skipped for the rest of the session
  (a new session tries again). A timed-out extra read is not a Sirius failure: it never counts
  toward the ignition-off detection, a re-init, or a restart of the lock-when-moving hold.
- Notified as an Extra frame (type `06`, on the Live characteristic), to every subscribed
  bonded client: every extra reply, every `21A9` reply and the first `21AA` reply of each cycle
  (the Live struct does not carry throttle, oxygen sensors, advance and the other fields of
  those two frames). Body:

| Offset | Field | Type | Meaning |
|---|---|---|---|
| 0 | seq | u16 | +1 per Extra notification (own counter) |
| 2 | t_ms | u32 | bridge uptime when decoded |
| 6 | ecu | u8 | `7A` |
| 7 | n | u8 | request length (1..32) |
| 8 | request | n bytes | e.g. `21 A0` |
| 8 + n | reply | rest | the positive reply payload from the response SID (`61 A0 ..`) |

The Extra frame of a `21A9` or `21AA` reply is notified just before the Live notification of
the same reply (same `t_ms`). A reply that does not fit one frame is not sent (never truncated).
Live notifications and the Live flags are unchanged except flag `0x20`.

The same Extra frame also carries BMT replies (`ecu` `26`): the transponder status `21A4`
in the parked profile, the pre-drive door read `21A0`, and the body read at a stop (`21A0`,
then `21A3`). These are sent whether live extras are on or not.

## Body read at a stop

Owner decision 2026-11-25: the doors and locks while the engine runs, at a stop. The drive
profile cannot read the BMT (one master on the K line), so the bridge pauses the Sirius for a
few seconds: StopCommunication, BMT init, `21 A0` (doors) and `21 A3` (locks), each sent as an
Extra frame (`ecu` `26`), then back to the Sirius. Tier 0 reads only; nothing is written.

- On request: `READ_BODY_ONCE` (`12`, no arguments). Body `[counter u32][12]`.
- Automatically: stopped (fresh Sirius speed 0) for more than 3 s with the engine running
  (rpm above 0), when the last pre-drive door read said a door was open, nothing else queued,
  and no unlock owed by lock-when-moving.
- At most once per stop (a speed above 0 starts a new stop), never while moving, never while
  lock-when-moving owes an unlock (it must keep watching the Sirius for the engine stop). Armed
  lock-when-moving without a lock yet does not refuse it: with speed 0 it does not lock, and
  the Sirius is back before the car can move far (the lock waits for the next 21A9 samples).
- Response: `Accepted` (queued), then `Ok` after the `21A3` reply (`LinkError`,
  `NegativeReply` or `Yielded` as for REQUEST). Unbonded: `NotAllowedForRole`.

## Battery estimate

The bridge estimates the battery state of charge itself (`crate::soc`, owner decision
2026-11-25); the app only shows it.

- Method: the open-circuit voltage of a rested flooded 12 V lead-acid battery at 20 C:
  11.89 V 0 %, 12.06 V 25 %, 12.24 V 50 %, 12.45 V 75 %, 12.65 V 100 %, linear in between,
  rounded to 5 %.
- Rest time: the bridge's own clock since it last saw rpm above 0 or a charging voltage. After a
  reset it is unknown until the engine has run again (no guess).
- Voltage history: one sample of the bridge's divider every 5 minutes while the engine is off
  and nothing charges (also at sleep wake-ups), the last 24. Readings below 11.5 V (a load, a
  start, a bad reading) and readings within 60 s after a dip below 12.0 V are not sampled. The
  estimate uses the median of the last three samples (the newest alone while there are fewer).
- A charging voltage counts as the engine running for the rest time, also after a reset (a
  charger then starts the rest time like a drive).
- Confidence: none below 1 h of rest, while charging or with the engine running; approximate
  from 1 h (surface charge reads high, about +-20 %); rested from 4 h with the last three samples
  within 20 mV (about +-10 %).
- Caveats: the bridge knows no temperature, and a cold battery reads low (about 5 mV per degree
  below 20 C); a load while parked lowers the voltage; AGM or EFB batteries need another
  table; the divider's accuracy is NOT VERIFIED.

## Roles, pairing, counters, rate limits

- **Roles** are fixed per bond at pairing time (max 2 bonds): the phone is
  **owner**, the Mac is **agent**. The firmware passes the role of the writing bond to
  `Bridge::on_command`; nothing in a frame can claim a role. An unbonded or unencrypted peer is
  `Unbonded` and gets `NotAllowedForRole` for everything.
- **Agent = Tier 0 only, always**: REQUEST of a read, live profiles,
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
  and at least 5 s apart.

## Approvals (Tier 1 to 3 from the phone)

Build feature `approvals` (off by default; SAFETY.md rule 2 "Bridge firmware"). By design
(2026-11-23): the owner gets Tier 1, Tier 2 and Expert over BLE with the same codes and flow as the
CLI; forbidden stays forbidden; the agent stays Tier 0.

1. Owner writes `PREPARE ecu payload`. The bridge checks: owner role, the kline-core tier with
   the bridge's guard ([Limits](#limits)) is 1, 2 or 3 (forbidden is `Forbidden`), battery at
   or above 12.0 V, and the moving rule (`VehicleMoving`; when nothing
   recent decides it, the job first reads `21A9` from the Sirius).
2. The bridge opens (or keeps) a session with that ECU and asks kline-core for the challenge
   of the request's tier on the session's real target: `service_challenge`,
   `config_write_challenge` (Tier 2, from PREPARE_WRITE: the app reads `21 80` and the backup
   `21 xx` with REQUEST first, chooses its definition from that `21 80` reply and sends it with
   the request; kline-core refuses unless it equals the session's latest positive `21 80`
   reply, like the CLI) or `expert_challenge` (10-character code).
   kline-core binds the code to that session, request id, target, key bytes, bytes, tier and
   `RiskInfo`.
3. Response `Challenge`: id, tier, code, to the owner's phone only (never broadcast, never in
   Raw). The app shows the exact bytes, the `RiskInfo` text (the app computes it with the same
   kline-core function, `safety::assess`) and the code; the owner types the code.
4. Owner writes `CONFIRM id code`. The bridge gives the typed code to kline-core
   `Approval::confirm` and sends with `Session::request_approved`, which re-checks everything.
   The challenge is consumed by the first CONFIRM, right or wrong, and expires after 120 s.
   While it is pending the session stays open on that ECU with the definition's keep-alive
   interval (3 s KWP, 7 s for the ABS). If that session ends anyway, CONFIRM answers
   `SessionLost` and writes nothing: a challenge never carries over to a new session. CONFIRM
   checks the battery and the moving rule again. While lock-when-moving is armed, a challenge
   for another ECU is dropped as soon as the car may have started (its CONFIRM gets
   `LwmActive`), so the bridge goes back to the Sirius.

**What the code is:** the same anti-accident gate as the CLI's `/dev/tty` code ([SAFETY.md](SAFETY.md)
rule 2): it stops the app, a script or the agent link from sending a Tier 1 request by mistake. It
is not an authentication against software on the owner's phone, which sees the code. What
keeps an agent out is the role of its bond, not the code.

## Parked LOCK and UNLOCK (fob output)

For the parked car: a PhotoMOS across the
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

Automaton `kline_bridge::lwm`:

- Lock once when the Sirius `21A9` speed is above 15 km/h for 3 s, from fresh samples only (a
  gap of more than 1.5 s between samples restarts the 3 s).
- Lock again only after more than 60 s at speed 0, or after an owner UNLOCK and a new
  drive-off.
- Unlock when rpm is 0, or when the bridge has *tried* the Sirius and failed for 3 s with no
  sample in between (ignition off). Samples that stop because the bridge is busy
  elsewhere do not count.
- 5 s between actuations; off by default; armed until the owner disarms in the app (owner
  decision 2026-11-23; a reset disarms, since arming is in RAM). Arming allows a lock on the
  next drive-off.
- Once it has (possibly) locked, it still unlocks at engine stop after a disarm. A lock that was
  written but whose reply is lost counts as possibly executed and keeps the unlock duty; a lock that was not written or that the BMT refused is not retried. A failed unlock is
  retried up to 3 times; every lock starts with all 3. After the third failure the
  automaton gives up: it disarms itself and sets status flag `0x80`; no lock follows until the
  owner arms again. The flag stays until the owner arms again or sends LWM_ACK; lock-state
  reads never clear it.
- The BMT lock memory may not follow the K-line `31 A0`/`31 A1` and its polarity is NOT
  VERIFIED, so a bare "unlocked" read proves nothing. An owed unlock ends only by
  a positive reply to `31 A1 00 01`, by the give-up, or by an observed change: after our lock
  completed, a positive `21A3` read shows "locked" and a later one "unlocked" (the owner unlocked
  by hand, or the app UNLOCK read "locked", pulsed and saw the bit change). An app UNLOCK that
  finds "already unlocked" sends no pulse and ends nothing. After LWM_DISARM with an unlock owed
  the bridge reads `21A3` once, which can be the "locked" or the "unlocked" half of such a
  change. So nothing stays stuck: once the engine stops, the unlock is sent or given up within
  the attempts. The host tests run in two simulator models, a lock memory that follows the
  K-line actuators and one that never does (`bmt_following` / `bmt_ecu`).
- While it needs the Sirius (armed with the engine running, or an unlock owed) the bridge stays
  on the drive profile even without a charging voltage and refuses parked START_LIVE, REQUEST or
  PREPARE for another ECU and YIELD_KLINE (`LwmActive`), so no client can make it leave the
  Sirius and toggle the locks. It never sleeps on a low battery while an unlock is
  owed.

Execution (lock-when-moving, maintainer decision 2026-11-23; build feature
`standing-lock`, off by default): the bridge stops the Sirius session, opens the BMT, sends
`31 A0 00 01` (lock) or `31 A1 00 01` (unlock), stops the BMT and goes back to the Sirius. Both
are Tier 1 BMT actuator requests. Arming (`LWM_ARM`) needs the owner's code for each of the two
requests (two challenges on a BMT session, nothing is sent then). Later the bridge answers
kline-core's challenge itself, only for exactly those payloads, on the BMT, with the `RiskInfo`
the owner confirmed (`standing_allows`; SAFETY.md rule 2 "Standing self-approval"). The owner
accepts that the remote's lock state may get out of step. The parked fob path stays available
for when remote B is repaired.

## Bridge behaviour

- **Profiles**: drive (Sirius `21AA, 21A9, 21AA, 21AA`, with [live extras](#extra-live-extras)
  one more Sirius read every `n` cycles), parked (BMT `21A3, 21A0, 21A2`, and the transponder
  status `21A4` at the start of a session and then every 8th cycle), and diagnostics on request
  (REQUEST/PREPARE open a session with their ECU; it ends when nothing needs it).
- **Profile selection** (owner decision 2026-11-25, `profile::select`): the drive profile
  whenever the engine may run, whatever a client requested; otherwise the requested profile
  (the app requests parked). Diagnostics jobs and a pending challenge take the line first.
  "May run": rpm above 0 in a Sirius frame of the last 3 s, or a charging voltage (13.2 V or
  more) that the Sirius does not explain as a charger. A charger is recognised by the Sirius
  itself: a `21A9` with 0 rpm taken while the voltage was already that high (no dip or rise
  since), or two Sirius inits in a row unanswered (ignition off) while the charging voltage
  looks like a charger (no dip in the 30 s before it began or since, no rise by more than 300 mV
  since it began, no rpm in 120 s). Either holds for 120 s, then the bridge looks at the Sirius
  again. A dip, a rise, or the BMT transponder status saying the ignition is on ends the
  "no answer" proof at once. One missed answer never parks the bridge. Lock-when-moving holding the Sirius always means drive. After the
  engine stops (no rpm for 3 s, voltage below 13.2 V) the bridge returns to the requested
  profile.
- **Pre-drive door read** (owner decision 2026-11-25): when the bridge leaves a BMT session for
  the drive profile, it first reads BMT `21A0` once more and sends the reply as an Extra frame
  (ECU `26`), so the app knows the doors at drive-off.
- **Transponder status** (owner decision 2026-11-25): in the parked profile the bridge reads BMT
  `21A4` (Tier 0, exactly 2 bytes) at the start of each BMT session and every 8th cycle, because
  no other parked frame shows that the ignition came on. It sends the reply as an Extra frame
  (ECU `26`) at the first read of every BMT session, whatever the bit says, and at every change
  of data byte 1 bit 0x01 ("ignition present" in the definition, NOT VERIFIED). The last state
  is kept across sessions, so the first parked session after a drive always reports the
  ignition-off. The bit set also ends the charger's "no answer" proof (the Sirius should answer
  now). Read only; nothing is ever written to the BMT here.
- **One master**: one kline-core `Session` at a time; a session with another ECU starts only
  after `82` StopCommunication. After a session that ended without a clean Stop (link errors,
  port failure) the next init of any ECU waits P3max (5 s), so the old ECU has timed out.
  `YIELD_KLINE` stops and switches the transceiver off, and a yield always wins over the
  automatic drive profile.
- **Keep-alive**: kline-core's TesterPresent when idle (3 s).
- **Reconnect**: after a failed init 0.5 s doubling to 10 s, or P3max 5 s when the ECU
  answered; after 3 failed exchanges in a row the session is dropped and re-initialised.
- **Sleep** (`Output::Sleep` carries the reason, `SleepReason::mode` the kind of sleep): below
  12.0 V for 10 s with the engine off (`LowBattery`, hard off: deep sleep), below 11.8 V for
  5 s (`CriticalBattery`, deep sleep), after 2 min with no client and nothing to do (`Idle`,
  light sleep, wake after 60 s), with no client for 10 min outside the drive profile whatever
  was requested (`NoClient`, the stuck-awake watchdog, light sleep), and once the engine has
  not run for 48 h (`ParkedLong`, deep sleep). The debounce ignores cranking dips and ADC noise.
  Never with the engine running and never while lock-when-moving owes an unlock. See
  [Power states](#power-states).

## Power states

Owner decision 2026-11-25. The bridge is powered from the OBD socket pin 16 (permanent 12 V):
no ignition wire, no 32 kHz crystal for now. Currents are estimates, NOT VERIFIED:

| State | When | What runs | Current |
|---|---|---|---|
| Driving | the drive profile runs | CPU, BLE, K-line transceiver | 30 to 80 mA |
| Parked, awake | a client, a job or a parked profile | CPU, BLE, BMT polling | 25 to 40 mA |
| Light sleep | `Idle`, `NoClient`, at most 48 h after the engine last ran (since boot when it has not run since) | light sleep with BLE advertising, a wake every 60 s for the battery | about 2.3 mA |
| Deep sleep | `ParkedLong` (48 h), `LowBattery` (below 12.0 V rested, hard off), `CriticalBattery` | a timer wake every 30 s for one ADC reading; no BLE | about 0.1 to 0.2 mA |

From deep sleep the firmware resumes only when the battery reads 13.2 V or more at two wakes in
a row (the engine runs, or a charger); otherwise it goes straight back to deep sleep. The wake
resets the chip: lock-when-moving is disarmed and the battery estimate's rest time is unknown
until the engine runs again. The firmware keeps the wake count in RTC memory; the 10 kOhm
pull-downs keep the fob and K_PWR outputs off while their pads float in deep sleep. A hardware
supervisor (a comparator that powers the board at about 13 V) can replace the timer wakes later.
The firmware light-sleeps in slices that feed the watchdog, never in a busy-wait.
- **Moving**: no non-read while a fresh (2 s) Sirius speed is above 0. A
  fresh standstill is enough for a Sirius request (its CONFIRM runs on the Sirius). For the ABS,
  BMT and airbag (their CONFIRM runs on their own session, without a speed) the engine must be
  known off: a Sirius `21A9` with 0 km/h and 0 rpm in the last 120 s and nothing since that says
  the engine started (no battery reading below 12.0 V, the dip of a start; no rise to a
  charging voltage), or no sign at all that the engine runs (no charging voltage, no rpm above 0
  in the last 120 s). Checked at PREPARE, when its job runs and again at CONFIRM
  (`VehicleMoving`). When nothing recent decides it at PREPARE, the job first reads `21A9` from
  the Sirius; if the Sirius does not answer, `VehicleMoving`. So with the engine idling, only a
  Sirius request goes through; switch the engine off for the others. The lock-when-moving requests are the
  exception.
- **Battery charger**: parked with a charger holding 13.2 V or more, the voltage says nothing,
  so the bridge relies on the Sirius `21A9` (engine off: 0 rpm). With the ignition on (engine
  off) this works for prepare and confirm. With the ignition off the Sirius does not answer and
  non-reads are refused (`VehicleMoving`): turn the ignition on, or disconnect the charger. A
  start is seen as a battery reading below 12.0 V (the cranking dip) or as a rise by more than
  300 mV over the voltage of the engine-off sample (the alternator). The firmware reads the
  battery every 100 ms, also inside blocking K-line actions (a 5-baud init blocks for about
  2 s), and hands the lowest reading of an action to the bridge right after it.
  NOT VERIFIED: that every start on this car dips that far for that long, and the divider's
  reading noise (a noisy rise voids the sample: the safe side).
- **Arming**: LWM_ARM only when not armed, no unlock owed and
  the moving rule for a BMT request allows it, that is parked with the engine off (an idling
  engine at a standstill is refused). The confirmed lock risk is kept
  apart until the unlock code is confirmed too; only then are both armed together. A wrong code
  leaves the previous state untouched.

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
| Command #6 PREPARE_WRITE BMT, identification `61 80 41 42`, `3B D2 00 01` | `01 03 0F 06 00 00 00 0F 26 04 61 80 41 42 3B D2 00 01 49 44` |
| Response #2 Ok `61 A9 04 35` | `01 04 09 02 00 00 00 00 61 A9 04 35 FB 93` |
| Response #3 Challenge id 7, tier 1, `ACDEFG` | `01 04 10 03 00 00 00 10 07 00 00 00 01 41 43 44 45 46 47 6A D0` |
| Live seq 1, t 12345, 1077 rpm, 0 km/h, 7412 us, 1400 mL/h, 19 C, 15 C, 406 mbar, 14.33 V, flags 07 | `01 01 14 01 00 39 30 00 00 35 04 00 F4 1C 78 05 13 0F 96 01 99 05 07 9A EA` |
| Status fw 0.1, session, 7A, drive, 14.10 V, lock unknown, engine running, up 60 s, estimate unknown | `01 02 10 00 01 02 7A 01 82 05 00 04 00 3C 00 00 00 FF 00 73 FB` |
| The same, 14-byte Status of older firmware (still decodes) | `01 02 0E 00 01 02 7A 01 82 05 00 04 00 3C 00 00 00 3B A7` |
| Status parked, 12.38 V, locked, up 3600 s, battery 75 % rested | `01 02 10 00 01 00 00 02 D6 04 01 00 00 10 0E 00 00 4B 02 D2 20` |
| Raw tx at 2500 ms, Sirius, `82 7A F1 21 A9 B7` | `01 05 0C C4 09 00 00 00 7A 82 7A F1 21 A9 B7 4C 33` |
| Command #7 LIVE_EXTRAS 4 | `01 03 06 07 00 00 00 11 04 86 22` |
| Command #8 LIVE_EXTRAS 0 (off) | `01 03 06 08 00 00 00 11 00 01 A7` |
| LIVE_EXTRAS 17 (above 16) | `01 03 06 09 00 00 00 11 11 B1 E0` -> BadFrame |
| Command #10 READ_BODY_ONCE | `01 03 05 0A 00 00 00 12 B3 99` |
| Extra seq 1, t 2500, Sirius, `21 A0` -> `61 A0 00 10 00 00 00 00 00` | `01 06 13 01 00 C4 09 00 00 7A 02 21 A0 61 A0 00 10 00 00 00 00 00 BD 71` |
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
- The decoders in `kline_bridge::live` are hand-coded positions for this car's ECUs. Extra
  frames are not decoded on the bridge; the app decodes them.

## kline-core changes for the bridge

1. `approved-routines` no longer implies `std` (`approved-routines = ["dep:getrandom"]`). The
   firmware builds with `--cfg getrandom_backend="custom"` and provides
   `__getrandom_v03_custom`: the ESP32-C3 hardware RNG with the SAR ADC entropy source on for
   the call (esp-hal `TrngSource` + `Trng`; per esp-hal a true RNG then; output statistics NOT
   VERIFIED on the chip).
2. `DefinitionGuard::flagged()`: read access to the flagged prefixes, for the parity test.

Not made: a kline-core `StandingApproval` type. The maintainer allowed the bridge's standing
self-approval for exactly the two lock-when-moving payloads (SAFETY.md rule 2); a core type would move that
check into kline-core and is a possible later cleanup.

## Not verified

- Profile selection on the car: that a charger and a running engine are told apart as
  described, and the time the switch to the drive profile takes after a start.
- The BMT `21A4` "ignition present" bit and the transponder bits; the pre-drive door read; the
  body read at a stop (that the BMT answers with the engine running, and the time the Sirius
  pause takes).
- Power: every current in [Power states](#power-states), BLE advertising in light sleep (not
  made yet), the deep-sleep timer wake and RTC memory on the chip, and the pad state of the fob
  and K_PWR outputs in deep sleep (the pull-downs should hold them).
- The battery estimate's table against this car's battery.
- Live extras on the car: that the Sirius answers `21A0`..`21AD` while driving as it did in the
  2026-11-22 scan (positive replies, about 42 ms each), and the Live rate with `every` 1.
- Everything on real hardware: ESP32-C3 UART at 10400, the BREAK_N bit-bang timing, break
  loopback through the L9637D, BLE MTU and throughput, NVS counters (not written yet).
- Lock memory after K-line `31 A0`/`31 A1`: on the car the BMT's lock memory (`21A3` d0
  bit 7) did not follow them, so lock-when-moving can put the remote's lock state out of step. The
  simulator offers both models (a memory that follows and one that does not).
- The `21A3` polarity (0 locked, 1 unlocked) for the remote and the key cylinder; it was
  observed only with the dashboard button. The `21A0` door bits.
- The fob path: that one press of remote B toggles the lock with the ignition off, and how
  fast `21A3` follows it.
