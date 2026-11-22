# kline: Renault K-line diagnostics over a KKL cable

kline is an open-source diagnostic tool for older Renault ECUs that talk on the K line
(ISO 9141-2 and ISO 14230 / KWP2000), using a cheap "dumb" FTDI KKL USB cable. It is meant for
people who own a late-1990s or early-2000s Renault and want to read identification, live data
and fault codes without a Windows laptop running DDT2000. It reads the DDT2000 ECU definition
files you already have, does the ISO 5-baud init and the KWP2000 fast init itself, and runs on
macOS, Linux and Windows. It is written in Rust.

The worked example is a 1999 Renault Clio II (Phase 1, X65) with a Siemens Sirius 32 injection
ECU at address 0x7A and a BMT body computer at 0x26. Nothing in the code is limited to that car,
but that is the only one the project is built against.

> **Status: not yet tested on a real car.** Everything so far is verified with unit tests, an
> ECU simulator, recorded frames and the real cable on the desk (no car). Treat it as
> experimental and do not rely on it for anything that matters yet. Details in
> [What is verified and what is not](#what-is-verified-and-what-is-not).

## Contents

- [What kline does](#what-kline-does)
- [Supported ECUs and cars](#supported-ecus-and-cars)
- [What is verified and what is not](#what-is-verified-and-what-is-not)
- [Hardware you need](#hardware-you-need)
- [Install and build](#install-and-build)
- [Quickstart](#quickstart)
- [ECU definitions: you supply your own](#ecu-definitions-you-supply-your-own)
- [Safety model](#safety-model)
- [FAQ](#faq)
- [Documentation](#documentation)
- [Project layout](#project-layout)
- [Contributing](#contributing)
- [License](#license)

## What kline does

- **Connects to an ECU** with ISO 9141 5-baud init, KWP2000 5-baud init with the inverted-KW2
  handshake, or KWP2000 fast init. With `--variant auto` it tries them in the same order as
  DDT2000 and tells you which one worked.
- **Reads identification** (`21 80`) and picks the matching ECU definition from several
  candidates, using DDT2000's AutoIdent rules when the definitions have them.
- **Reads live data**: any documented read request, decoded with the scaling, units and lists
  from the definition, once (`read`) or repeatedly (`live`).
- **Reads fault codes** (`dtc`), following the definition's failure model.
- **Runs a few service functions** (clear fault codes, actuator tests, remote key-fob learning,
  configuration writes) only after it shows you exactly what will be sent and you type a
  confirmation code. See [Safety model](#safety-model).
- **Records every session** as a JSONL capture and can decode a capture later, offline
  (`replay`).
- **Checks your cable and computer** without a car (`selftest`): timer precision, break timing
  and, on a looped-back line, the USB round trip.
- Ships a **read-only binary**, `kline-agent`, for scripts and AI assistants: JSON output, reads
  only, and it cannot send anything beyond the documented reads.

## Supported ECUs and cars

kline is driven by DDT2000 XML definitions, so "supported" means: the protocol variant is
implemented and the definition's requests decode. These are the ECUs the code and tests are
built around.

| ECU | Address | Protocol | What has been checked |
|---|---|---|---|
| Siemens Sirius 32 injection (Clio II D7F, the worked example) | 0x7A | KWP2000, 5-baud init with ~KW2 handshake (key bytes `D0 8F`) | simulator end to end, every allowed read of the real definition decodes (structurally); **not on a car** |
| BMT / UCH body computer (Clio II) | 0x26 | KWP2000, 5-baud init with ~KW2 handshake (`D0 8F`) | simulator end to end, real definitions decode; **not on a car** |
| Sagem Safir 1 / Safir 2 injection | 0x7A | ISO 9141 5-baud ("ISO8" in DDT2000), no handshake (`1C 86` / `C2 85`) | simulator (Safir 2 profile), definitions decode; **not on a car** |
| KWP2000 fast-init ECUs (for example the S2000 injection definition) | per definition | KWP2000 fast init + StartCommunication | definition parses and decodes; fast init tested in the simulator with hand-written fixtures; **not on a car** |
| Anything CAN-only | | | refused: a KKL cable cannot talk CAN |

**Cars.** The 1999 Clio II is the reference. Other Renaults of the same era (roughly 1996-2004:
Kangoo, Megane I, Scenic I, Twingo, Laguna, and Dacia models built on the same parts) use
related K-line ECUs and DDT2000 definitions. They are **untested**. They may work if you have
the right definition file for your ECU and it speaks one of the protocols above.

kline is not a generic OBD-II / EOBD code reader. It speaks the manufacturer diagnostic
services that DDT2000 uses, as described by the definition file, not the generic EOBD PIDs.

## What is verified and what is not

Verified:

- 345 automated tests (`cargo test --all-features`, with the definition files present). CI runs
  the suite on Linux, macOS and Windows; there the tests that need definition files skip.
- End-to-end runs of every command against the built-in ECU simulator, both in-process (all
  operating systems) and over a pseudo-terminal (Linux, macOS). The simulator also keeps an
  independent log of anything that would change the ECU, and the tests assert it stays empty
  where it should.
- Decoding against the author's own DDT2000 definition files for the Sirius 32, Safir 1/2, S2000
  and both BMT variants (these tests skip when the files are absent, as in CI).
- The real FTDI FT232R cable with `kline selftest`, plugged into a computer but not a car:
  on macOS (M4 Pro) and in a Windows 11 ARM64 virtual machine. Numbers are in
  [docs/platforms.md](docs/platforms.md#timing-precision).

Not verified:

- Anything on a real car: which init variant each ECU needs, the real key bytes, real reply
  timing, whether the ECUs answer TesterPresent, how the cable behaves with a powered K line.
- The real cable on Linux and on bare-metal Windows.

Each doc ends with its own "Not verified" list. The first on-car procedure is in
[docs/cli.md](docs/cli.md#first-test-on-the-car).

## Hardware you need

### A KKL cable with an FTDI chip

You need a "dumb" USB to K-line cable: a USB serial chip plus a K-line level shifter, with no
microcontroller in between. These are usually sold as **"VAG-COM KKL 409.1"** or "KKL USB OBD"
cables. kline drives the serial port directly: it holds the line low with a serial break to send
the 5-baud address bit by bit and to make the 25 ms fast-init pulse, and it reads back every
byte it sends.

| Chip | Status |
|---|---|
| FTDI FT232R / FT232RL | the cable kline is developed with; measured with `selftest` on macOS and in a Windows VM, not yet on a car |
| Other FTDI chips (vendor id `0403`) | auto-detected on Linux and Windows; untested |
| CH340, CP2102 and other USB serial chips | not auto-detected (pass `--port`), untested. Break timing may differ |
| ELM327 (USB, Bluetooth, Wi-Fi) | does **not** work, see the [FAQ](#does-it-work-with-an-elm327-adapter) |

Some cheap cables carry counterfeit FT232 chips, which some FTDI driver versions disable. That
is a driver matter, not something kline can fix.

### Which connector and pin

kline does not care about the plug; it talks to whatever K line the cable is wired to. A
standard KKL cable has a 16-pin OBD socket plug with the K line on **pin 7**, plus ground and
+12 V from the car. The Clio II worked example assumes pin 7.

Older Renaults vary here. Some cars from before about 2001 have the 16-pin socket but, reportedly,
not every ECU on pin 7 (some cables have a 7/15 switch for this), and older models have a
Renault 12-pin diagnostic socket that needs an adapter. kline has not been tested on any of
these. **Check the wiring for your car** before you assume an ECU is dead.

### A computer

macOS, Linux or Windows (x86_64 or ARM64). Fast init needs a 25 ms pulse accurate to about
1 ms, so a busy computer or a virtual machine can miss it; `kline selftest` tells you. See
[docs/platforms.md](docs/platforms.md).

## Install and build

There are no prebuilt binaries yet. You need Rust stable from https://rustup.rs. On Linux and
macOS nothing else is required (macOS needs the Xcode command line tools for the linker). On
Windows you need the MSVC toolchain: Visual Studio Build Tools with "Desktop development with
C++" (plus the ARM64 build tools on an ARM PC).

```
git clone https://github.com/natanaelfejes/kline
cd kline
cargo build --release -p kline-cli      # target/release/kline (kline.exe on Windows)
cargo build --release -p kline-agent    # target/release/kline-agent (read-only)
```

Build `kline-agent` on its own as shown. If you build it in the same cargo invocation as
`kline-cli` (`--workspace` or `--all-features`), cargo unifies the approval feature into it and
`kline-agent` refuses to start (exit 7). See [docs/cli.md](docs/cli.md#building-kline-and-kline-agent).

Per-OS setup before first use:

| OS | Cable shows up as | Driver | Before first use |
|---|---|---|---|
| Linux | `/dev/ttyUSB0` | `ftdi_sio` (in the kernel) | join the `dialout` group (`uucp` on Arch); optionally `kline selftest --set-latency 1` |
| macOS | `/dev/cu.usbserial-*` | AppleUSBFTDI (built in) | nothing; never use `/dev/tty.*` |
| Windows | `COM3`, `COM12`, ... | FTDI VCP (Windows Update or ftdichip.com) | Device Manager > Port > Advanced > Latency Timer 1 ms |

## Quickstart

```
kline ports                              # find the cable
kline selftest                           # cable in the computer only, not in the car
kline connect --ecu sirius               # init the Sirius 32, open a session, stop
kline ident --ecu sirius                 # read 21 80, pick the definition
kline live --ecu sirius --frames 21A9,21AA --rate 2
kline dtc --ecu sirius                   # read fault flags (never clears)
```

Every command accepts `--json` (one JSON object per line) and has fixed exit codes. Add
`--trace` to see every DTR change, break edge, sleep and byte with a time stamp. Captures go to
`~/.local/share/kline/captures` (Windows: `%APPDATA%\kline\captures`); change it with
`--captures DIR` or `KLINE_CAPTURE_DIR`.

The full command reference, exit codes and a step-by-step first test at the car are in
[docs/cli.md](docs/cli.md).

## ECU definitions: you supply your own

kline reads DDT2000 ECU XML definition files. Those files belong to Renault. They are **never**
shipped with kline and must never be committed to this repository
(`scripts/check-no-renault-data.sh` checks every commit in CI). Copy the files from your own
DDT2000 installation into a directory and point kline at it:

```
kline --defs /path/to/ecus ...           # or: export KLINE_DDT_XML=/path/to/ecus
kline --xml /path/to/one-ecu.xml ...     # a single definition
```

The default directory is `~/.local/share/kline/ecus` (Linux, macOS) or `%APPDATA%\kline\ecus`
(Windows). `--ecu sirius` and `--ecu bmt` are shortcuts for the Clio II file names inside it.
`CLIO_DDT_XML` is still read as a deprecated fallback for one release.

## Safety model

The safety policy lives in `kline-core`, not in the command line front end. It is default-deny
and decides on the exact request bytes and on the ECU address the init actually woke.

| Tier | What | How it is allowed |
|---|---|---|
| 0 Read | documented reads: identification, live data, fault codes | always, also from `--json` and `kline-agent` |
| 1 Service | clear fault codes, output tests, remote learning, ... | you approve each request |
| 2 Config write | writing configuration | approval, plus a backup read and identification first |
| 3 Expert | any other request that is not forbidden ("unknown effect"), including security access, flashing services and other sessions, with explicit "may brick the ECU" warnings | approval with a longer confirmation code |
| Forbidden | immobilizer, transponder and key functions | never, not even in expert mode |

Approvals only exist in the interactive `kline` binary, which reads the confirmation code from
the terminal itself (`/dev/tty`, or the console on Windows), never from stdin. `kline-agent` is
built without approvals and can only *prepare* a request: it prints the risk and the exact
command for a human to run.

**What the confirmation code is, honestly:** an anti-accident gate. It stops pipes, scripts,
`--json` and programs without terminal access from sending a non-read by mistake. It is *not*
protection against an automated agent that drives a terminal: such an agent can read the code
off the screen and type it. If you do not want that, do not give such a tool a terminal that can
run `kline`.

The full rules are in [docs/SAFETY.md](docs/SAFETY.md); the exact bytes per tier are in
[docs/protocol.md](docs/protocol.md#safety-policy).

## FAQ

### Does it work with an ELM327 adapter?

No. An ELM327 is a microcontroller that speaks AT commands: it does the init and the framing
itself and only hands you the data. kline needs a raw serial port it fully controls. It
bit-bangs the 5-baud address with serial break (200 ms per bit), makes the 25 ms fast-init low
pulse, toggles DTR, and reads its own echo byte by byte to detect collisions. None of that is
possible through an ELM327, whether USB, Bluetooth or Wi-Fi. You need a KKL cable.

### Is it a DDT2000 or ddt4all alternative?

For the K-line part, partly. kline reads the same DDT2000 XML definitions and follows DDT2000's
init order, timing defaults, retries and decoding rules. Its definition parser can also decode
exactly the way ddt4all does, which is how the decoder is cross-checked. It is a command line tool, not a GUI: there are no DDT2000 screens, no CAN, and
no ELM327 support. Use it if you want a small, scriptable, cross-platform tool for a K-line
Renault; use DDT2000 or ddt4all if you need screens or CAN.

### Does it work on other brands?

Not out of the box. The protocol layer is plain ISO 9141-2 / ISO 14230 (5-baud init, fast init,
KWP2000 framing) and is not Renault specific. But everything above it comes from DDT2000
definitions, which cover Renault-group ECUs. For another brand you would need:

- definition files in the DDT2000 XML format describing that ECU's requests and data, or a new
  parser for another format;
- a review of the safety policy, whose immobilizer rules are keyed on Renault ECU addresses,
  identifiers and names;
- the brand's own protocol if it is not KWP2000 or ISO 9141 (for example, VAG's older KW1281 is
  not implemented).

### Where do I get ECU definitions?

From your own DDT2000 installation. kline never distributes Renault data, and definition files
must not be attached to issues or pull requests. Small hand-written test fixtures with invented
values are fine.

### Can it touch the immobilizer?

No, never, by design. Reading transponder *status* (BMT `21 A4`, exactly two bytes) is an
ordinary read. Writing, learning, resetting or disabling anything immobilizer, transponder or
key related is not implemented and is refused in every tier, including expert mode. The guard
works on addresses, exact request bytes, and the names in the loaded definition. The only
exception is the BMT's two exact remote-control learning routines, which are allowed with
approval.

### Is it safe to use on my car?

It has not been tried on a car yet. Reads are low risk, but no diagnostic tool is risk-free on
a 25-year-old ECU. Start with `selftest`, then follow the [first test at the
car](docs/cli.md#first-test-on-the-car), which uses reads only. Using it on a vehicle is at your
own risk (see [NOTICE](NOTICE)).

### Can I use it on CAN cars?

No. A KKL cable only does K line. A definition with only a `<CAN>` section is refused with exit
code 8.

## Documentation

| Document | What is in it |
|---|---|
| [docs/protocol.md](docs/protocol.md) | K-line in plain words, then the exact 5-baud init, fast init, KWP2000 and ISO 9141 framing, timing and safety bytes |
| [docs/cli.md](docs/cli.md) | every command and option, exit codes, captures, the first test at the car |
| [docs/platforms.md](docs/platforms.md) | Linux, macOS and Windows: drivers, permissions, latency timer, timing measurements |
| [docs/architecture.md](docs/architecture.md) | how the crates fit together, the sans-IO design, where safety is enforced |
| [docs/sim.md](docs/sim.md) | the ECU simulator and what it can and cannot prove |
| [docs/spec-semantics.md](docs/spec-semantics.md) | how DDT2000 XML definitions are read and decoded |
| [docs/SAFETY.md](docs/SAFETY.md) | the safety rules enforced in code |
| [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md) | checks to run before a pull request |

## Project layout

| Path | What |
|---|---|
| `crates/kline-spec` | DDT2000 XML to a typed model, data decode and encode. Pure, no I/O |
| `crates/kline-core` | `no_std` + alloc protocol engine (sans-IO state machines) and the safety policy |
| `crates/kline-host` | shared runtime: serial and simulator transports, timing, capture, the read commands |
| `crates/kline-cli` | the interactive `kline` binary (the only one with approvals) |
| `crates/kline-agent` | the read-only `kline-agent` binary |
| `crates/kline-sim` | ECU simulator for end-to-end tests, in-process (all OSes) and over a pty (Linux, macOS) |
| `scripts/check-no-renault-data.sh` | fails if anything that looks like a Renault definition is tracked |

More in [docs/architecture.md](docs/architecture.md).

## Contributing

Bug reports with a capture (`.jsonl`) and a `--trace` log are the most useful thing you can send,
especially from a real car. Remove anything personal first, and never attach definition files.
Checks and rules: [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md).

## License

Copyright (C) 2026 Natanael Feješ. Licensed under the [GNU GPL v3.0 or later](LICENSE), with one
additional term from GPL section 7(b) in [NOTICE](NOTICE): forks and derived works must keep the
NOTICE and credit the original author ("Based on kline by Natanael Feješ"). If you distribute a
modified version, you must publish its source under the same license.

This is an independent project, not affiliated with or endorsed by Renault.
