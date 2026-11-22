# Contributing

## Gates

```
cargo fmt --check
cargo clippy --all-targets --all-features -- -D warnings
cargo test --all-features
cargo build -p kline-core --no-default-features
cargo build --release -p kline-agent
scripts/check-no-renault-data.sh
```

## No Renault data

Renault's DDT2000 ECU XML files must never be committed or attached to issues and pull
requests. Only hand-written fixtures that you author yourself (small, invented values) may live in
`crates/*/tests/fixtures/`. Run `scripts/check-no-renault-data.sh` before every commit: it fails
if a tracked file is under `ddt-xml/`, has a known Renault definition file name, or is an XML
larger than 8 KB whose root is an ECU/Target with Requests and Datas. Real definitions stay
in the gitignored `ddt-xml/` directory.

## Safety policy

Changes to the tiers in `kline-core` need tests in `crates/kline-core/tests/`. `kline-agent` and
any future MCP crate must never enable `approved-routines` (`crates/kline-agent/tests/agent.rs`).

## License

Contributions are licensed MIT OR Apache-2.0, like the project.
