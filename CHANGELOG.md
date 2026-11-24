# Changelog

All notable changes to kline are listed here. The format follows
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/). There is no tagged release yet, so the
work so far is grouped by milestone under `Unreleased`, newest first. The crates are at version
0.1.0.

## [Unreleased]

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
