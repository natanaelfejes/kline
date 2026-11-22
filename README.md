# kline

Open-source diagnostics for Renault K-line (ISO 9141 / KWP2000) ECUs over a dumb FTDI KKL cable.
It replaces the parts of DDT2000 you need for reading data and, with explicit human approval,
for a few service functions. Written in Rust.

**Tested target:** 1999 Renault Clio II, Sirius 32 injection ECU + BMT body computer. The Clio is
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

Default: `~/.local/share/kline/ecus`. `--xml FILE` takes a single definition. (`CLIO_DDT_XML` is
still read as a deprecated fallback for one release.) The tests that need real definitions look in
`ddt-xml/` (gitignored) or `$KLINE_DDT_XML` and are skipped when absent.

## Safety tiers

The policy lives in `kline-core`, not in the CLI, and is default-deny on the exact request bytes.

| Tier | What | How |
|---|---|---|
| 0 Read | documented reads: identification, live data, fault codes | autonomous, also from `--json` and the AI binary |
| 1 Service | clear fault codes, output tests, remote learning, ... | human approval per request |
| 2 Config write | writing configuration | human approval plus a backup read first |
| 3 Expert | any other non-forbidden request ("unknown effect") | human approval with a longer confirmation code |
| Forbidden | immobilizer/key functions, security access, memory/programming services | never |

Approvals exist only with the cargo feature `approved-routines`, which only the interactive
`kline` binary enables; it reads the confirmation from `/dev/tty`. `kline-agent` (read-only,
JSON, for AI use) is built without it and can only *prepare* a request. Details:
[docs/protocol.md](docs/protocol.md).

## Build and run

```
cargo build --release -p kline-cli      # target/release/kline
cargo build --release -p kline-agent    # target/release/kline-agent (no approvals)
kline ports
kline selftest                          # cable in the computer only, not in a car
kline connect --ecu sirius
```

Captures (JSONL) go to `~/.local/share/kline/captures` by default; override with
`--captures DIR` (alias `--capture-dir`) or `KLINE_CAPTURE_DIR`. Full command reference:
[docs/cli.md](docs/cli.md). Simulator: [docs/sim.md](docs/sim.md).

Do not build `kline-cli` and `kline-agent` in one cargo invocation if you want the agent without
the approval types compiled in (feature unification; see docs/cli.md).

## Layout

- `crates/kline-spec`: DDT2000 XML to typed model, data decode/encode (pure)
- `crates/kline-core`: `no_std` + alloc protocol engine and safety policy
- `crates/kline-sim`: ECU simulator over a pty for end-to-end tests
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
