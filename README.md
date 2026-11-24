# kline

**Open-source Renault K-line diagnostics: read fault codes, live data and ECU identification
from late-1990s and early-2000s Renaults over a cheap KKL USB cable, without DDT2000.**
Built and tested on a 1999 Clio II (Siemens Sirius 32 engine ECU, Sagem BMT body computer,
Bosch ABS and airbag). It uses the DDT2000 ECU definitions you already have.

[![CI](https://github.com/natanaelfejes/kline/actions/workflows/ci.yml/badge.svg)](https://github.com/natanaelfejes/kline/actions/workflows/ci.yml)
[![License: GPL-3.0-or-later](https://img.shields.io/badge/license-GPL--3.0--or--later-blue.svg)](LICENSE)

```
$ kline connect --ecu sirius
init fast   fast init not answered (normal for many Renault K-line ECUs), trying 5-baud byte 7A
init single byte 7A: ok
connected: sirius at 7A (kwp, kwp 5-baud) with variant single; sync 55 KW D0 8F, ~address 85, key bytes match the definition
session 10 A0: ok 50
```

kline is a command-line tool written in Rust. It runs on macOS, Linux and Windows. It speaks
KWP2000 (ISO 14230) and Renault's "ISO8" variant of ISO 9141, does the 5-baud and fast inits
itself, and decodes replies with the DDT2000 XML definition of the ECU. It is not a generic
OBD-II code reader.

## Contents

- [Features](#features)
- [Supported ECUs and cars](#supported-ecus-and-cars)
- [Hardware](#hardware)
- [Install](#install)
- [Quick start](#quick-start)
- [ECU definitions: bring your own](#ecu-definitions-bring-your-own)
- [Safety model](#safety-model)
- [Testing without a car: the simulator](#testing-without-a-car-the-simulator)
- [ESP32 Bluetooth bridge (experimental)](#esp32-bluetooth-bridge-experimental)
- [How kline compares](#how-kline-compares)
- [FAQ](#faq)
- [Documentation](#documentation)
- [Contributing](#contributing)
- [License](#license)

## Features

- **Init**: ISO 9141 5-baud init, KWP2000 5-baud init with the inverted-KW2 handshake, and
  KWP2000 fast init. `--variant auto` tries them in DDT2000's order and reports which worked.
- **Identification**: reads `21 80` (or the ECU's own identification request) and picks the
  matching definition, with DDT2000's AutoIdent rules when the definition has them.
- **Live data**: any documented read, decoded with the definition's scaling, units and lists,
  once (`read`) or repeatedly (`live`, about 21 requests per second on the Clio).
- **Fault codes**: `dtc` reads the fault frames and lists what is set, present or memorized.
  It never clears; `clear-dtc` is a separate, approved command.
- **Service functions**: fault clear, output tests, BMT remote-key learning and configuration
  writes. Each one shows the exact bytes and its risk, and runs only after you type a
  confirmation code ([Safety model](#safety-model)).
- **Captures**: every session is recorded as JSONL and can be decoded offline (`replay`).
- **Cable check**: `selftest` measures timer precision and break timing without a car.
- **Read-only binary**: `kline-agent` for scripts and AI assistants. JSON output, reads only;
  it can prepare a service request for a human but never send one.
- **Simulator**: an ECU simulator for end-to-end tests without a car.

## Supported ECUs and cars

kline is driven by DDT2000 definitions, so "supported" means the protocol variant is
implemented and the definition decodes. These ECUs were checked on a 1999 Clio II phase 1
(X65, D7F 1.2 petrol) on 2026-11-22 and 2026-11-23:

| ECU | Address | Protocol | Verified on the car |
|---|---|---|---|
| Siemens Sirius 32 engine ECU | 0x7A | KWP2000, 5-baud init (`D0 8F`) | init, identification (exact AutoIdent match), live data while driving, fault codes, read-only scan, output tests (fuel pump, fans, warning lamps) |
| Sagem BMT N2 body computer (UCH) | 0x26 | KWP2000, 5-baud init (`D0 8F`) | init (also with the ignition off), identification, inputs and outputs, fault codes and fault clear, remote-key learning, door lock and unlock outputs, indicator, wiper, courtesy light and buzzer tests |
| Bosch airbag | 0x2C | KWP2000, 5-baud init | identification, fault reads, impedance and status reads; nothing was written or cleared ([why](docs/SAFETY.md#airbag-ecu-address-0x2c-any-protocol-with-or-without-a-definition)) |
| Bosch 5.3 ABS | 0x01 | ISO8 5-baud (`DC 83`) | identification (`51`) and fault memory (`58`). The fault clear `45` exists (Tier 1) but has not been sent on the car |
| Sagem Safir 1 / 2 engine ECU | 0x7A | ISO8 5-baud (`1C 86` / `C2 85`) | not on a car; simulator only |
| KWP2000 fast-init ECUs | per definition | KWP2000 fast init | not on a car; simulator only |
| CAN-only ECUs | | | refused: a KKL cable cannot talk CAN |

Details per ECU, including what did **not** work, are in
[protocol.md "What the car confirmed"](docs/protocol.md#what-the-car-confirmed). Two examples:
the Clio's BMT answers `7F 31 12` to the window and door-lamp output tests (this unit has no
such outputs), and its stored lock state does not follow the K-line lock outputs. The airbag's
`21 80` decodes with two definitions (`AB70_X65`, `AB80_X65_X90_V1`), so which one fits is
still open.

The ddt4all database shipped by default has no definition for this BMT. kline reads it with
the DDT2000 definitions `BMT_X65_ph1_p.xml` and `BMT_SAGEM.xml`.

**Other cars.** Renaults of the same era (roughly 1996 to 2004: Clio II, Kangoo, Megane I,
Scenic I, Twingo, Laguna, and Dacias built on the same parts) use related K-line ECUs. They are
**untested**. They may work if you have the definition for your ECU and it speaks one of the
protocols above. Reports are welcome.

## Hardware

### A KKL cable with an FTDI chip

You need a "dumb" USB to K-line cable: a USB serial chip and a K-line level shifter, with no
microcontroller in between. These are sold as **"VAG-COM KKL 409.1"** or "KKL USB OBD" cables.
kline holds the line low with a serial break to send the 5-baud address and the 25 ms fast-init
pulse, and reads back every byte it sends.

| Chip | Status |
|---|---|
| FTDI FT232R / FT232RL | used on the car from macOS; measured with `selftest` on macOS and in a Windows VM |
| Other FTDI chips (vendor id `0403`) | auto-detected on Linux and Windows; untested |
| CH340, CP2102 and other USB serial chips | not auto-detected (pass `--port`); untested, break timing may differ |
| ELM327 (USB, Bluetooth, Wi-Fi) | not supported, see the [FAQ](#does-it-work-with-an-elm327-adapter) |

Some cheap cables carry counterfeit FT232 chips, which some FTDI driver versions disable.

### Connector

A standard KKL cable has a 16-pin OBD plug with the K line on **pin 7**. On the 1999 Clio II
all four ECUs answered through the 16-pin socket. Some older Renaults have a 12-pin Renault socket
that needs an adapter, and some cables have a 7/15 switch. Check the wiring for your car before
you decide an ECU is dead.

### A computer

macOS, Linux or Windows, x86_64 or ARM64. Fast init needs a 25 ms pulse accurate to about 1 ms,
which a busy computer or a virtual machine can miss; `kline selftest` tells you. The Clio's ECUs
use the 5-baud init, which tolerates 10 ms. See [docs/platforms.md](docs/platforms.md).

## Install

There are no prebuilt binaries yet. Install Rust stable from <https://rustup.rs>, then:

```
git clone https://github.com/natanaelfejes/kline
cd kline
cargo build --release -p kline-cli      # target/release/kline (kline.exe on Windows)
cargo build --release -p kline-agent    # optional: target/release/kline-agent (read-only)
```

macOS needs the Xcode command line tools; Windows needs Visual Studio Build Tools with
"Desktop development with C++". Build `kline-agent` in its own `cargo build` as shown: built
together with `kline-cli`, it gets the approval feature and refuses to start (exit 7).

Per-OS setup before first use:

| OS | Cable shows up as | Driver | Before first use |
|---|---|---|---|
| Linux | `/dev/ttyUSB0` | `ftdi_sio` (in the kernel) | join the `dialout` group (`uucp` on Arch); optionally `kline selftest --set-latency 1` |
| macOS | `/dev/cu.usbserial-*` | AppleUSBFTDI (built in) | nothing; kline refuses `/dev/tty.*`, use the `cu` device |
| Windows | `COM3`, `COM12`, ... | FTDI VCP (Windows Update or ftdichip.com) | Device Manager > Port > Advanced > Latency Timer 1 ms |

Then copy your DDT2000 definitions in once ([below](#ecu-definitions-bring-your-own)):

```
kline setup --defs-from /path/to/your/ddt2000/ecus
```

## Quick start

Ignition on, engine off, cable in the OBD socket.

**1. Find the cable.**

```
$ kline ports
* /dev/cu.usbserial-XXXXXXXX  usb 0403:6001 FT232R USB UART
  /dev/tty.usbserial-XXXXXXXX  usb 0403:6001 FT232R USB UART  (not used, use /dev/cu.usbserial-XXXXXXXX)
cable: /dev/cu.usbserial-XXXXXXXX
```

**2. Wake up the engine ECU.** `--ecu sirius` is a shortcut for the Sirius 32 definition;
for any other ECU use `--xml path/to/definition.xml`.

```
$ kline connect --ecu sirius
init fast   fast init not answered (normal for many Renault K-line ECUs), trying 5-baud byte 7A
init single byte 7A: ok
connected: sirius at 7A (kwp, kwp 5-baud) with variant single; sync 55 KW D0 8F, ~address 85, key bytes match the definition
session 10 A0: ok 50
```

The failed fast init is expected on this car; the 5-baud init takes about 2 seconds.

**3. Read the fault codes.** `dtc` only reads.

```
$ kline dtc --ecu sirius
```

Each fault frame prints `N set (present: a, memorized: b)` and one line per set flag. This is
the Clio's ABS, which had one stored fault (read with `--xml X24_ABS_Bosch5.3.xml`):

```
MES_DIAG (58): 1 set
    Défaut mémoire 1 = signal capteur AVG  (compteur mémoire 1 1, Etat contacteur stop 1 Pédale relâchée, Etat régulateur ABS 1 Inactif, Tension 1 13.56 V, Vitesse de référence 1 16. km/h)
1 fault flag(s) set
```

The item names come from the definition, so they are in French for Renault's files.

**4. Watch live data.** Ctrl-C stops and closes the session.

```
$ kline live --ecu sirius --frames 21AA,21A9 --rate 2
...
#120 t=235888 ms 21 A9
  [Trame 10 - parametres 2]
    Pression collecteur absolue mesurée                  532.2 mb
    Régime moteur                                        2230 tr/mn
    Température eau mesurée                              67.5 °C
    Tension batterie mesurée                             14.112 V
    Vitesse du véhicule                                  43 km/h
```

(Trimmed. Recorded at 43 km/h.) Add `--items NAME,..` and `--compact` for one line per cycle
with only the values that changed.

Every command takes `--json` (one JSON object per line) and has fixed exit codes. `--trace`
prints every DTR change, break edge, sleep and byte with a time stamp. Captures go to
`~/.local/share/kline/captures` (Windows: `%APPDATA%\kline\captures`).

The full command reference, the exit codes and a step-by-step first session at the car, with the
output to expect, are in [docs/cli.md](docs/cli.md#first-session-on-a-car).

## ECU definitions: bring your own

kline reads DDT2000 ECU XML definition files. Those files belong to Renault. They are **never**
shipped with kline and must never be committed to this repository
(`scripts/check-no-renault-data.sh` checks every commit in CI). Copy them from your own DDT2000
installation, where they are usually the XML files in the `ecus` folder of the data directory:

```
kline setup --defs-from /path/to/ecus   # copy once into the default directory
kline --defs /path/to/ecus ...          # or point at a directory (or set KLINE_DDT_XML)
kline --xml /path/to/one-ecu.xml ...    # or use a single file
```

The default directory is `~/.local/share/kline/ecus` (Linux, macOS) or `%APPDATA%\kline\ecus`
(Windows). `--ecu sirius`, `--ecu bmt`, `--ecu bmt-ph1p` and `--ecu bmt-sagem` are shortcuts for
the Clio II file names. Not sure which file is your ECU? Give `kline ident` several
candidates for the same address (`--xml a.xml --xml b.xml`; the first sets up the link) and it
picks the one whose identification matches. How kline decodes a definition, and where it
differs from ddt4all, is in
[docs/spec-semantics.md](docs/spec-semantics.md).

## Safety model

The policy lives in `kline-core`, not in the command-line front end. It is default-deny and
decides on the exact request bytes and on the ECU address the init actually woke.

| Tier | What | How it is allowed |
|---|---|---|
| 0 Read | identification, live data, fault codes | always, also from `--json` and `kline-agent` |
| 1 Service | fault clear, output tests, remote-key learning | you type a 6-character code for that one request |
| 2 Config write | configuration writes | a code, plus a backup read and the identification in the same session |
| 3 Expert | any other request that is not forbidden: undocumented requests, security access, flashing services, other sessions | a 10-character code and a "may brick the ECU" warning |
| Forbidden | immobilizer and key functions; ABS RAM downloads and routines (valves, pump, bleeding); airbag firing lines, crash data and fault clear | never, not even in expert mode |

Approvals exist only in the interactive `kline` binary, which reads the code from the terminal
(`/dev/tty`, or the console on Windows), never from stdin or with `--json`. `kline-agent` is
built without approvals.

**What the code is:** an anti-accident gate. It stops pipes, scripts and `--json` callers from
sending a non-read by mistake. It does not stop an automated agent that drives a terminal: such
an agent can read the code off the screen and type it.

The full rules are in [docs/SAFETY.md](docs/SAFETY.md); the exact bytes per tier are in
[docs/protocol.md](docs/protocol.md#safety-policy).

## Testing without a car: the simulator

`kline-sim` plays the ECU on a virtual K line: it echoes bytes like a KKL cable, detects the
5-baud and fast-init wake-ups, checks the tester's timing against the ISO windows and answers
from a definition. The test suite runs every command against it, in-process on all three
operating systems and over a pseudo-terminal on Linux and macOS. The simulator keeps its own log
of anything that would change the ECU, and the tests check that it stays empty where it should.
Car captures can be replayed as the ECU. What the simulator cannot prove is in
[docs/sim.md](docs/sim.md).

## ESP32 Bluetooth bridge (experimental)

`crates/kline-bridge` and `firmware/esp32c3` are a bridge between the K line and Bluetooth LE
on an ESP32-C3 with an L9637D transceiver, for a phone app and for
`kline --transport ble` (reads only). The protocol logic runs and is tested on the host against
the simulator. **It has not run on hardware or in a car yet**, and `kline --transport ble` needs
a BLE radio backend that is not in this build. See
[docs/bridge-protocol.md](docs/bridge-protocol.md) and
[docs/bridge-hardware.md](docs/bridge-hardware.md).

## How kline compares

| | kline | DDT2000 | ddt4all | pyren |
|---|---|---|---|---|
| What it is | command-line tool | Renault's dealer diagnostic tool | open-source GUI for DDT2000-style databases | open-source scripts for Renault's CLIP database |
| Language | Rust | Windows application | Python | Python |
| Runs on | macOS, Linux, Windows | Windows | macOS, Linux, Windows | macOS, Linux, Windows |
| Adapter | dumb FTDI KKL cable | various, including KKL cables | ELM327 family and others | ELM327 family |
| Bus | K line only | K line and CAN | CAN and K line, depending on the adapter | CAN and K line, depending on the adapter |
| ECU data | your DDT2000 XML files | its own database | a DDT2000-format database | Renault's CLIP database |
| Screens | none: text and JSON | yes | yes | text menus |

Use kline if you have a K-line Renault from before CAN and want a small, scriptable tool that
records every byte. Use DDT2000 or ddt4all if you need screens, CAN, or an ELM327. kline's
decoder can run in ddt4all's mode, and that is how it is cross-checked.

## FAQ

### Does it work with an ELM327 adapter?

No. kline drives the K line itself: it sends the 5-baud address bit by bit with serial break
(200 ms per bit), makes the 25 ms fast-init pulse, toggles DTR, and reads its own echo byte by
byte. That needs a raw serial port. An ELM327 does the init and framing in its own firmware and
takes AT commands, which kline does not implement. Get a KKL cable; they are cheap.

### Is it a DDT2000 alternative?

For the K-line part, yes, for reading and for a few service functions. kline reads the same XML
definitions and follows DDT2000's init order, timing defaults, retries and decoding rules. It has
no DDT2000 screens and no CAN.

### Does it work on other brands?

Not out of the box. The protocol layer is plain ISO 9141-2 / ISO 14230 and not Renault specific,
but everything above it comes from DDT2000 definitions, and the safety rules are keyed on Renault
ECU addresses and names. VAG's older KW1281 protocol is not implemented. A generic OBD-II / EOBD
mode (standard live data and fault codes, modes 01, 03 and 04) is planned, not there yet.

### Where do I get ECU definitions?

From your own DDT2000 installation. kline never distributes Renault data. Do not attach
definition files to issues or pull requests; small hand-written fixtures with invented values
are fine.

### Can it touch the immobilizer?

No, by design. Reading the transponder *status* (BMT `21 A4`, exactly two bytes) is an ordinary
read. Writing, learning, resetting or disabling anything immobilizer, transponder or key related
is not implemented and is refused in every tier. The only exception is the BMT's two exact
remote-key learning routines, which pair the door-lock remote and are allowed with approval.

### Is it safe to use on my car?

Reads are low risk, but no diagnostic tool is risk-free on a 25-year-old ECU. Start with
`selftest`, then follow the [first session](docs/cli.md#first-session-on-a-car), which uses
reads only. Using kline on a vehicle is at your own risk (see [NOTICE](NOTICE)).

### Why does the remote sometimes do the opposite after a K-line lock?

On the Clio's BMT the K-line lock and unlock outputs move the locks but do not update the lock
state the BMT remembers (`21 A3`). The next remote press acts on the old state. Press it again.

### Can I use it on CAN cars?

No. A KKL cable only does K line. A definition with only a `<CAN>` section is refused with exit
code 8.

## Documentation

| Page | What is in it |
|---|---|
| [docs/cli.md](docs/cli.md) | every command and option, exit codes, captures, the first session at the car, remote learning, the ABS |
| [docs/protocol.md](docs/protocol.md) | K-line in plain words, then the exact inits, framing, timing, safety bytes and what the car confirmed |
| [docs/SAFETY.md](docs/SAFETY.md) | the safety rules enforced in code |
| [docs/platforms.md](docs/platforms.md) | Linux, macOS and Windows: drivers, permissions, latency timer, timing measurements |
| [docs/spec-semantics.md](docs/spec-semantics.md) | how DDT2000 XML definitions are read and decoded |
| [docs/architecture.md](docs/architecture.md) | how the crates fit together and where safety is enforced |
| [docs/sim.md](docs/sim.md) | the ECU simulator and what it can and cannot prove |
| [docs/bridge-protocol.md](docs/bridge-protocol.md), [docs/bridge-hardware.md](docs/bridge-hardware.md) | the experimental ESP32-C3 bridge |
| [CHANGELOG.md](CHANGELOG.md) | changes by milestone |

Project layout:

| Path | What |
|---|---|
| `crates/kline-spec` | DDT2000 XML to a typed model, data decode and encode; no I/O |
| `crates/kline-core` | `no_std` sans-IO protocol engine and the safety policy |
| `crates/kline-host` | shared runtime: serial and simulator transports, timing, capture, the read commands |
| `crates/kline-cli` | the interactive `kline` binary (the only one with approvals) |
| `crates/kline-agent` | the read-only `kline-agent` binary |
| `crates/kline-sim` | the ECU simulator |
| `crates/kline-bridge` | the ESP32 bridge logic (experimental) |
| `firmware/esp32c3` | the bridge firmware skeleton (built on its own) |

## Contributing

The most useful thing you can send is a capture (`.jsonl`) and a `--trace` log from a car other
than the Clio II. Remove anything personal first, and never attach definition files. Checks
and rules: [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md).

## License

Copyright (C) 2026 Natanael Feješ. Licensed under the [GNU GPL v3.0 or later](LICENSE), with one
additional term under GPL section 7(b) in [NOTICE](NOTICE): forks and derived works must keep the
NOTICE and credit the original author ("Based on kline by Natanael Feješ"). If you distribute a
modified version, you must publish its source under the same license.

kline is an independent project, not affiliated with or endorsed by Renault.
