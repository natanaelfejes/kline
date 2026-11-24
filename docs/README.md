# kline documentation

kline is an open-source diagnostic tool for older Renault ECUs on the K line (ISO 9141-2 and
KWP2000 / ISO 14230), used with an FTDI KKL cable. Start with the
[project README](../README.md) for what it does and how to install it.

## Using kline

| Page | Read it when you want to |
|---|---|
| [cli.md](cli.md) | look up a command, option or exit code, run a first session on a car, pair a remote, or read the ABS |
| [platforms.md](platforms.md) | set up the cable on Linux, macOS or Windows, or check timing precision |
| [SAFETY.md](SAFETY.md) | know what kline will and will not send to an ECU, and why |

## How it works

| Page | Read it when you want to |
|---|---|
| [protocol.md](protocol.md) | understand K-line, 5-baud init, fast init and KWP2000 framing, check an exact byte or timing value, or see what the car confirmed |
| [spec-semantics.md](spec-semantics.md) | know exactly how a DDT2000 definition is decoded, and where kline differs from ddt4all |
| [architecture.md](architecture.md) | find your way around the crates and the sans-IO core |
| [sim.md](sim.md) | test without a car, or know what the simulator does not prove |

## ESP32 bridge (experimental)

| Page | Read it when you want to |
|---|---|
| [bridge-protocol.md](bridge-protocol.md) | the BLE GATT protocol of the ESP32-C3 bridge, its roles and approvals |
| [bridge-hardware.md](bridge-hardware.md) | the bridge wiring, parts, power budget and bring-up checklist |

## Contributing

| Page | Read it when you want to |
|---|---|
| [CONTRIBUTING.md](CONTRIBUTING.md) | send a pull request or a capture from your car |
| [CHANGELOG.md](../CHANGELOG.md) | see what changed between milestones |
