# Contributing to kline

Thanks for helping. The most useful contribution right now is a capture from a real car:
the `.jsonl` file from the capture directory plus the `.trace.txt` from a `--trace` run (see
[cli.md "First test on the car"](cli.md#first-test-on-the-car)). Tell us the car, the ECU and
the cable, and remove anything personal first.

## Checks before a pull request

CI runs these on Linux, macOS and Windows. Run them locally first:

```
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo build -p kline-core --no-default-features
cargo build --release -p kline-agent
scripts/check-no-renault-data.sh
```

The second-to-last line matters: `kline-agent` must be built on its own (see
[cli.md](cli.md#building-kline-and-kline-agent)). The `no_std` build checks that `kline-core`
still compiles without the standard library.

## No Renault data

Renault's DDT2000 ECU XML files must never be committed or attached to issues and pull
requests. Only hand-written fixtures that you author yourself (small, invented values) may live
in `crates/*/tests/fixtures/`.

Run `scripts/check-no-renault-data.sh` before every commit. It fails if a tracked file:

- is under `ddt-xml/`,
- has a known Renault definition file name, or
- is an XML larger than 8 KB whose root is an ECU/Target with Requests and Datas.

Real definitions stay in the gitignored `ddt-xml/` directory. Tests that need them skip when it
is absent.

## Safety policy

The rules are in [SAFETY.md](SAFETY.md).

- Changes to the tiers in `kline-core` need tests in `crates/kline-core/tests/`.
- `kline-agent` and any future MCP crate must never enable `approved-routines`
  (`crates/kline-agent/tests/agent.rs` checks this).
- Never add a code path that writes, learns or disables anything immobilizer related.

## License

Contributions are licensed GPL-3.0-or-later with the NOTICE attribution term, like the project.
