# kline

Open-source diagnostics for Renault K-line (ISO 9141 / KWP2000) ECUs over a dumb FTDI KKL cable.
It replaces the parts of DDT2000 you need for reading data and, with explicit human approval,
for a few service functions. Written in Rust.

**Target vehicle:** 1999 Renault Clio II, Sirius 32 injection ECU + BMT body computer. The Clio is
the worked example in the docs; nothing in the code is limited to it.

## Status: pre-HIL

Not yet tested on a real car. Everything so far is verified against a simulator (`kline-sim`),
hand-written fixtures and recorded frames. The first on-car procedure is in
[docs/cli.md](docs/cli.md). Expect rough edges; do not rely on it for anything that matters yet.

## You must supply your own ECU definitions

kline reads DDT2000 ECU XML definition files. These belong to Renault and are **never**
redistributed here, and must never be committed (`scripts/check-no-renault-data.sh` enforces
this). Put your own copies in a directory and point kline at it:

```
kline --defs /path/to/ecus ...        # or export KLINE_DDT_XML=/path/to/ecus
```

Default: `~/.local/share/kline/ecus` (Linux, macOS) or `%APPDATA%\kline\ecus` (Windows). `--xml FILE` takes a single definition. (`CLIO_DDT_XML` is
still read as a deprecated fallback for one release.) The tests that need real definitions look in
`ddt-xml/` (gitignored) or `$KLINE_DDT_XML` and are skipped when absent.

## Safety tiers

The policy lives in `kline-core`, not in the CLI, and is default-deny on the exact request bytes.

| Tier | What | How |
|---|---|---|
| 0 Read | documented reads: identification, live data, fault codes | autonomous, also from `--json` and the AI binary |
| 1 Service | clear fault codes, output tests, remote learning, ... | human approval per request |
| 2 Config write | writing configuration | human approval plus a backup read first |
| 3 Expert | any other non-forbidden request ("unknown effect"), including security access, flashing services and other sessions, with explicit "may brick the ECU" warnings | human approval with a longer confirmation code |
| Forbidden | immobilizer/transponder/key functions only (BMT/UCH/decoder and Sirius immobilizer fields, plus definition requests named like them) | never |

Approvals exist only with the cargo feature `approved-routines`, which only the interactive
`kline` binary enables; it reads the confirmation from `/dev/tty` (Windows: the console,
`CONIN$`). `kline-agent` (read-only,
JSON, for AI use) is built without it and can only *prepare* a request. Details:
[docs/protocol.md](docs/protocol.md).

**What the confirmation code is, honestly:** an anti-accident gate. It stops pipes, scripts,
`--json` and agents without terminal access from sending a non-read by mistake. It is *not*
protection against an AI agent that drives a terminal: such an agent can read the code off the
screen and type it. The owner allows that deliberately when he lets an agent run `kline exec`. If
you run AI agents and do not want that, do not give them a terminal that can run `kline`.

## Install and build

kline builds and its test suite runs on Linux, macOS and Windows (CI matrix in
`.github/workflows/ci.yml`; per-OS cables, drivers, permissions and timing in
[docs/platforms.md](docs/platforms.md)). You need Rust stable (https://rustup.rs) and nothing else
on Linux and macOS; on Windows the MSVC toolchain (Visual Studio Build Tools, "Desktop development
with C++", plus the ARM64 tools on an ARM PC).

| OS | Cable shows up as | Driver | Before first use |
|---|---|---|---|
| Linux | `/dev/ttyUSB0` | `ftdi_sio` (in the kernel) | join the `dialout` group (`uucp` on Arch); optional `kline selftest --set-latency 1` |
| macOS | `/dev/cu.usbserial-*` | AppleUSBFTDI (built in) | nothing; never use `/dev/tty.*` |
| Windows | `COM3`, `COM12`, ... | FTDI VCP (Windows Update or ftdichip.com) | Device Manager > Port > Advanced > Latency Timer 1 ms |

## Build and run

```
cargo build --release -p kline-cli      # target/release/kline (kline.exe on Windows)
cargo build --release -p kline-agent    # target/release/kline-agent (no approvals)
kline ports
kline selftest                          # cable in the computer only, not in a car
kline connect --ecu sirius
```

Captures (JSONL) go to `~/.local/share/kline/captures` (Windows: `%APPDATA%\kline\captures`) by default; override with
`--captures DIR` (alias `--capture-dir`) or `KLINE_CAPTURE_DIR`. Full command reference:
[docs/cli.md](docs/cli.md). Simulator: [docs/sim.md](docs/sim.md).

Build `kline-agent` on its own (`cargo build --release -p kline-agent`). Built together with
`kline-cli` (one cargo invocation, `--workspace` or `--all-features`), cargo unifies the approval
feature into it and `kline-agent` then refuses to start (exit 7; see docs/cli.md).

## Layout

- `crates/kline-spec`: DDT2000 XML to typed model, data decode/encode (pure)
- `crates/kline-core`: `no_std` + alloc protocol engine and safety policy
- `crates/kline-sim`: ECU simulator, in-process (all OSes) and over a pty (Linux, macOS) for end-to-end tests
- `crates/kline-host`: shared runtime (transports, capture, commands)
- `crates/kline-cli`: `kline` binary
- `crates/kline-agent`: `kline-agent` binary
- `tools/crosscheck`: dev-only differential check against ddt4all (not part of the crates)

## Contributing

See [docs/CONTRIBUTING.md](docs/CONTRIBUTING.md).

## License

Licensed under either of [Apache License 2.0](LICENSE-APACHE) or [MIT license](LICENSE-MIT) at
your option. Copyright 2026 Natanael Feješ.

This is an independent project, not affiliated with or endorsed by Renault.
