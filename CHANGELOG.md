# Changelog

All notable changes to kline are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). There is no tagged release yet, so the
work so far is grouped by milestone under `Unreleased`, newest first. The crates are at version
0.1.0.

## [Unreleased]

### Body read at every stop (2026-11-25, review round 4)

#### Changed

- kline-bridge: the body read at a stop reads the whole BMT live set (`21A2, 21A0, 21A3,
  21A4, 21A7, 21A8`) at every stop over 3 s with the engine running (not only after an open
  door), at most once per stop and per 30 s, never while an unlock is owed or a challenge is
  pending; `READ_BODY_ONCE` reads the same set. A drive-off seen in the BMT speed (`21A2`)
  drops the rest. The engine data pause measured in the sim: 5.3 s.
- kline-bridge: a stop also ends at an engine stop and with a new Sirius session.
- kline-bridge: the last Status before a deep sleep carries `last_error` `0x17` DeepSleep.

#### Fixed

- kline-bridge: a body read (over 5 s away from the Sirius) ended "engine running" at a warm
  idle below 13.2 V, so the bridge fell back to the requested parked profile and
  lock-when-moving lost the Sirius (a drive-off was never locked). The engine now counts as
  running until the Sirius has been tried again.
- kline-bridge: a Sirius PREPARE that waited behind a body read samples `21A9` again instead
  of being refused.
- Firmware: the RTC watchdog goes off as the last step before deep sleep, back on if the sleep
  is rejected; the panic handler resets the chip; the deep-sleep wake honours the bridge's
  wake time.

### Bridge power states and body reads (2026-11-25, review round 3)

#### Added

- kline-bridge: `READ_BODY_ONCE` (`0x12`, no arguments): at a fresh standstill with the
  engine running, once per stop, the bridge pauses the Sirius, reads BMT `21A0` and `21A3`
  (Tier 0) and sends both as Extra frames (ECU `26`). Refused while moving (`VehicleMoving`),
  while lock-when-moving owes an unlock (`LwmActive`) and a second time in the same stop
  (`RateLimited`). Also automatic after 3 s stopped when the pre-drive door read said open.
- kline-bridge `power`: power states for a board powered from OBD pin 16 (light sleep with BLE
  advertising for at most 48 h after the engine last ran, then deep sleep with a 30 s timer
  wake that resumes at 13.2 V read twice; deep sleep below 12.0 V rested), current estimates,
  `SleepReason::{NoClient, ParkedLong}` and `SleepReason::mode`; the stuck-awake watchdog (no
  client for 10 min outside the drive profile: sleep). Firmware: deep sleep with the wake count
  in RTC memory.

#### Changed

- kline-bridge: the charger's "no Sirius answer" proof needs two inits in a row unanswered
  while the charging voltage began over 30 s after the last dip, with no rise and no rpm; a
  dip restarts the count and the BMT ignition bit ends the proof (one missed answer of a
  running engine parked the bridge for 120 s).
- kline-bridge: the transponder status `21A4` is published at the first read of every BMT
  session and at every change of the ignition bit, which is kept across sessions (the
  ignition-off after a drive was never sent); read every 8th parked cycle (was every 7th).
- kline-bridge `soc`: readings below 11.5 V and within 60 s after a dip are not sampled; the
  estimate uses the median of the last three samples.
- kline-bridge: `LowBattery` and `CriticalBattery` sleep deep with a 30 s timer wake (was a 1 h
  light-sleep wake).

### Bridge live extras (2026-11-25)

#### Added

- kline-bridge: `LIVE_EXTRAS` command (`0x11`, every 0..16), the "advanced" drive profile for
  Clio Link: one extra Sirius read (`21A0`, `21A1`, `21A2`, `21A4`, `21A8`, `21AB`, `21AD`, in
  turn) after every n drive cycles, the drive schedule unchanged; the raw replies, every `21A9`
  and one `21AA` per cycle as Extra frames (type `0x06`, on the Live characteristic); Live
  flag `0x20`. Reads only (Tier 0); a frame the ECU refuses is skipped for the session.
  Vectors, end-to-end tests against kline-sim (also with lock-when-moving armed),
  docs/bridge-protocol.md "Extra (live extras)".

- kline-bridge (review round 2026-11-25): Status grows to 16 bytes with the bridge's own battery
  estimate (`soc`: rested OCV table, rest time from the bridge clock, parked voltage history,
  confidence); 14-byte Status still decodes. The BMT transponder status `21A4` is read in the
  parked profile and published as an Extra frame at every ignition change; the BMT doors are read
  once more before the switch to the drive profile and published. `profile::select`.

#### Changed

- kline-bridge: the drive profile runs whenever the engine may run (rpm seen, or a charging
  voltage the Sirius does not show to be a charger), whatever was requested; the app requests
  parked and got no engine data while driving before. A timed-out live-extras read is skipped
  for the session and never counts as a Sirius failure (it restarted the lock-when-moving hold).

#### Fixed

- kline-bridge: the BMT `21A0` door bits are read from data byte 2 (0x02 front, 0x04 rear), the
  definition's position (FirstByte 5); they were read from data byte 5, which is constant 0 in
  every capture, so an open door could never show. Still NOT VERIFIED on the car.

### Standalone simulator (2026-11-24)

#### Added

- `kline-sim` binary (crate `kline-sim`, macOS and Linux only): one simulated K line on a pty
  with any number of ECUs, for running the real `kline` against a simulated car. Built-in
  hand-written sirius, bmt, airbag and abs definitions (`--write-defs DIR` writes them under the
  car's file names, never overwriting); `--ecu ADDR=file.xml` for any definition;
  `--replay ADDR=capture.jsonl`; `--answer` and `--fault` (silence, `7F` codes, after N answers,
  init faults); ECUs off and back on the line and the BMT dashboard lock button over the control
  socket (`kline-sim ctl`); the BMT lock memory with `--bmt-lock-follows true|false` (default
  false, as on the car); the ABS 7000 ms keep-alive window; a transcript of every frame.
- `kline_sim::multi::MultiEcu`, `SpecEcu::with_reply`, `SpecEcu::with_bit_memory`,
  `PtySim::spawn_with_control`, `VirtualLine::from_bus`.
- `crates/kline-cli/tests/session3_rehearsal.rs`: every command of the session-3 script against
  the simulated car, with its exit code; `exec` through the test approval terminal. The section
  tests are `#[ignore]`d (about 2 minutes): `cargo test -p kline-cli --test session3_rehearsal -- --ignored`.
- kline refuses a `kline-sim` built-in definition (target name `kline-sim built-in ..`) on any
  port but the simulator (exit 2, before the port is opened); `risk` warns about one.

### Documentation overhaul (2026-11-24)

#### Changed

- README rewritten: supported ECUs with their verification status, a quick start, a comparison
  with DDT2000, ddt4all and pyren, and an FAQ.
- `docs/protocol.md` has a "What the car confirmed" table; `docs/cli.md` shows the output the
  car gave and has new sections on BMT outputs, Sirius output tests and the ABS.
- The 2026-11-23 car results are documented: airbag identification, fault, impedance and status
  reads; Sirius output tests; BMT output tests and the outputs this BMT N2 lacks.
- Public pages no longer refer to private planning notes.

#### Fixed

- `docs/cli.md`: `kline ports` without a cable exits 0, not 3.
- The tier table in `kline-core` (`safety.rs`) and the protocol page name the airbag date
  writes as the only Tier 2 there, and the Bosch 5.3 ABS `51`/`58` (Tier 0) and `45` (Tier 1).

### ESP32-C3 bridge (2026-11-23 to 2026-11-24)

#### Added

- `kline-bridge`: a sans-IO bridge between the K line and Bluetooth LE, tested on the host
  against the simulator. It has the BLE GATT protocol v1, owner and agent roles, live profiles,
  approvals from the owner's bonded phone (feature `approvals`, off by default) and an optional
  lock-when-moving automaton (feature `standing-lock`, off by default).
- `firmware/esp32c3`: an esp-hal skeleton for the ESP32-C3 with an L9637D transceiver. Not yet
  run on hardware.
- `kline --transport ble` (cargo feature `ble`): Tier 0 reads through the bridge.
- Bridge docs: `docs/bridge-protocol.md`, `docs/bridge-hardware.md`.

#### Changed

- `kline-core`'s `approved-routines` feature no longer needs `std`, so the firmware can use it.

### ABS and airbag (2026-11-23)

#### Added

- Bosch 5.3 ABS on ISO8 at 0x01 (key bytes `DC 83`): `connect`, `ident` (`51`), `dtc` (`58`,
  with numbered fault slots and their context), `read`, and `clear-dtc` (`45`, Tier 1).
  Verified on the car with `X24_ABS_Bosch5.3.xml`.
- ISO8 replies that echo the request SID are accepted for the Bosch 5.3 definition only.
- Simulator profile for the Bosch 5.3 ABS and replays of the car's captures.

#### Security

- Every `41` download and `71` routine run is forbidden on any ISO8 unit at 0x01, including
  the factory fill and brake bleed, which the refusal names. One approved request per ABS
  session.
- Airbag (0x2C): firing-line, impedance, configuration 2 and crash counter writes, every `14`,
  every `30` and every session other than `10 81` are forbidden. Only the two date writes are
  Tier 2; other writes are Expert.
- Sirius output control (`30`) is Tier 1 only for the 13 outputs of the definition, with fixed
  start, status and stop shapes. BMT outputs `A4`/`A5` are Expert.

### First car sessions (2026-11-22 to 2026-11-23)

#### Added

- `read` takes several requests in one session; `live --cycles` and `live --duration`.
- `exec --hold SECONDS --watch REQ` keeps the session open after an approved request, for the
  BMT remote learning window. A lost reply to an approved request is exit 13 and says the
  request was sent.
- Timing overrides `--p3`, `--inter-byte`, `--w4`, `--kw-window`, and `--dtr high|low|leave`.
- `--version` and every capture header name the git commit the binary was built from.

#### Changed

- A mistyped confirmation code gives one more attempt with a new code.
- After an init attempt the ECU answered but that failed, the next attempt waits P3max (5 s).
- A fast init aborted by the host's timing guard is retried once right away.

#### Fixed

- Fast-init pulse timing is paced from the real falling edge.
- K is released on drop, SIGTERM, SIGHUP and a second Ctrl-C.
- `live --compact` honours `--items`.

#### Security

- Sirius `31`/`32`/`33 A3` (reset of the backed-up memory) is forbidden; injector inhibition
  is `31 A0 00`.
- `count FF` on the BMT lock and window outputs carries a "runs until stopped" warning.

### Command line and platforms (2026-11-22)

#### Added

- `kline` and `kline-agent` binaries (renamed from `clio`), with the serial transport for the
  FTDI KKL cable and the simulator transports.
- Commands `ports`, `setup`, `selftest`, `connect`, `ident`, `read`, `live`, `dtc`, `scan`,
  `risk`, `exec`, `clear-dtc`, `replay`, and `prepare` in `kline-agent`.
- DDT2000 detection order, retries, periodic TesterPresent and AutoIdent.
- `selftest` measures timer precision, break timing and, with `--echo-rtt`, the echo round
  trip.
- Every session writes a JSONL capture, a `.out.txt` with the human output and, with `--trace`,
  a `.trace.txt`.
- `--ecu bmt-ph1p` and `--ecu bmt-sagem` to name one BMT definition.
- CI on Linux, macOS and Windows; `scripts/check-no-renault-data.sh`.
- GPL-3.0-or-later license with a section 7(b) attribution notice.

#### Fixed

- macOS: opening the FTDI cable at 10400 baud (EINVAL).
- Wake-up sleeps are paced from deadlines, OS sleeps are chunked, and a late init is aborted.

#### Security

- Default-deny tiered policy in `kline-core`; approvals only with the `approved-routines`
  feature, read from the terminal. `kline-agent` refuses to start if the feature was compiled in.
- The immobilizer is forbidden in every tier: BMT key-code services and outputs, flash
  services, `3B D2` AP1CLE and its aliases, and a name guard over the loaded definition.
- The airbag ECU's non-reads carry a safety-system warning and risk text.

### Core, spec and simulator (2026-11-21 to 2026-11-22)

#### Added

- `kline-spec`: parses DDT2000 ECU XML into a typed model; decodes and encodes data with
  DDT2000 semantics (default) or ddt4all semantics; cross-checked against ddt4all.
- `kline-core`: a `no_std` sans-IO K-line engine with ISO 9141 and KWP2000 5-baud init, KWP2000
  fast init, framing, echo handling, sessions, retries and a read-only scan plan.
- `kline-sim`: an ECU simulator, in-process and over a pty, with an independent log of anything
  that would change the ECU.
