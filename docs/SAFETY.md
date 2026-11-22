# Safety rules

kline can send requests that change an ECU, so it has a small set of rules that are enforced in
code and covered by tests. Code comments refer to them as "SAFETY.md rule N". The exact request
bytes for each tier are in [protocol.md](protocol.md#safety-policy).

In short: reads are free, anything else needs a human to approve that exact request in a
terminal, and the immobilizer is never touched.

## Rule 1: Renault data never enters git

`ddt-xml/` and any generated spec JSON are gitignored. Tests that need them read from `ddt-xml/`
at test time and are skipped (not failed) with a clear message if absent. Hand-written minimal
XML fixtures that you author yourself may live in `crates/*/tests/fixtures/`.

## Rule 2: the safety policy is enforced in kline-core, not in the CLI

Default-deny, decided on exact payload bytes and the target of the init that actually ran
(owner-approved tiers, [protocol.md "Safety policy"](protocol.md#safety-policy)):

- **Tier 0 READ**: documented reads only (KWP `21 xx`/`1A xx` exactly 2 bytes, `17`/`18`, `3E`,
  `10 81`, `10 A0`; Safir `21`, `3350`, `3351`, `3352`, `3354`). The only tier library code
  paths, `--json`, MCP and tests against a real port may execute.
- **Tier 1 SERVICE** (clear DTC `14`, output tests `30`, adaptation reset `11 FF`/`32`, Sirius
  `31A0A0`, BMT `31C7`/`31C8` remote learning and `31 out` actuators), **Tier 2 CONFIG WRITE**
  (`3B`, with backup read and `21 80` identification in the same session) and **Tier 3 EXPERT**
  (any other non-forbidden request, "unknown effect"): only with a per-request human `Approval`
  that quotes the request's `RiskInfo`.
- **Approvals.** `Approval` exists only with cargo feature `approved-routines` (off by default).
  Only the interactive `kline` CLI may enable it; it reads the random confirmation code from
  `/dev/tty`, never with `--json`. MCP/`--json` binaries must never enable it; automated and MCP
  callers may only prepare a request (print `RiskInfo` and the exact command for a human).
  `kline-agent` refuses to start if the feature was unified into it.
- **What the code is.** The `/dev/tty` code is an **anti-accident gate**, not an
  authentication: it stops a pipe, `--json`, a script or a non-interactive agent from sending by
  mistake. An automated agent that drives a terminal (pty) can read and type the code; the owner
  permits that when he chooses (owner decision 2026-11-22). Do not present it as protection
  against an agent with terminal access.
- **Expert, not forbidden.** Security access `27`, sessions other than `10 81`/`10 A0`, and on
  non-immobilizer ECUs download/upload/transfer `34`-`37`, flash erase/checksum routines
  `31 81`/`31 82` and memory/routine-by-address services are Tier 3 Expert with specific
  warnings (`MayBrickEcu`, `MayLeaveProgrammingSession`, `SecurityAccess`), not forbidden
  (owner decision 2026-11-22, final; on the BMT see rule 3). BMT memory read `23` stays Expert
  (owner decision, step 7).
- **Protocol frames.** ISO8 `36` (keep-alive) and `3F` (stop), KWP `81`/`82`: sent only by the
  init machines and `Session::tester_present`/`stop`, never as requests.

## Rule 3: never touch the immobilizer or transponder

In any tier, including expert. This is the only forbidden class (owner-approved).

- **On a BMT/UCH/decoder** (address `0x26`/`0xA6`, or any ECU whose loaded definition is a
  BMT/UCH/decoder: `kline_core::spec::definition_guard`): `3D`, `38`-`3A`, `2C`;
  download/upload/transfer `34`-`37` and flash erase/checksum `31`/`32`/`33` `81`/`82` (they
  overwrite or erase the key-code EEPROM, owner decision step 7); `30`/`31`/`32`/`33` C0-C9
  (except exact `31C7000000`/`31C8000000`) or with the immobilizer actuator outputs `A6`
  "Ligne Transpondeur", `AB` "Led Verlog", `AC` "Ligne ECM" (both BMT definitions, step 7);
  any `2E`/`2F`; any `3B` except `3B D2` and `3B D2` index 2 (AP1CLE); `21A4` with extra
  bytes. `3B D2` index 3 (TRF) is the remote type, RF or infrared, not key related: Tier 2 with
  a specific warning (`ChangesRemoteType`).
- **On non-immobilizer ECUs**: `31 A0 A3` (and `32`/`33`/`38`-`3A` `A0 A3`), writes
  `2E`/`2F`/`3B` to `A0`/`A2`/`AE`/`BD`; on a Sirius (0x7A or a Sirius definition) also output
  control `30` and routines on those LIDs.
- **Name guard**: a Tier 1-3 request whose definition names look immobilizer/transponder/key
  related, applied by `Session` in kline-core ([cli.md](cli.md#name-guard)). The texts of an
  enumerated sent parameter count too: the value whose text has such a word is refused (fixed
  bytes plus that byte), or the whole request when the parameter is not one whole byte.
- **Allowed**: reading transponder *status* (BMT `21A4`, exactly 2 bytes) is Tier 0.

Writing, learning, resetting or disabling the immobilizer is never implemented.

## Rule 4: no network, no telemetry

No network calls at runtime. No telemetry.
