# Safety rules

These rules are enforced in code and tested. Code comments refer to them as "SAFETY.md rule N".

1. **Renault data never enters git.** `ddt-xml/` and any generated spec JSON are gitignored. Tests that need
   them read from `ddt-xml/` at test time and are skipped (not failed) with a clear message if absent.
   Hand-written minimal XML fixtures that you author yourself may live in `crates/*/tests/fixtures/`.
2. **Safety policy is enforced in `kline-core`, not in the CLI.** Default-deny, decided on exact payload
   bytes and the target of the init that actually ran (owner-approved tiers, docs/protocol.md "Safety"):
   - Tier 0 READ: documented reads only (KWP `21 xx`/`1A xx` exactly 2 bytes, `17`/`18`, `3E`, `10 81`,
     `10 A0`; Safir `21`, `3350`, `3351`, `3352`, `3354`). The only tier library code paths, `--json`, MCP and
     tests against a real port may execute.
   - Tier 1 SERVICE (clear DTC `14`, output tests `30`, adaptation reset `11 FF`/`32`, Sirius `31A0A0`, BMT
     `31C7`/`31C8` remote learning and `31 out` actuators), Tier 2 CONFIG WRITE (`3B`, with backup read and
     `21 80` identification in the same session) and Tier 3 EXPERT (any other non-forbidden request,
     "unknown effect"): only with a per-request human `Approval` that quotes the request's `RiskInfo`.
   - `Approval` exists only with cargo feature `approved-routines` (off by default). Only the interactive
     `kline` CLI may enable it; it reads the random confirmation code from `/dev/tty`, never with `--json`.
     MCP/`--json` binaries must never enable it; AI/MCP may only prepare a request (print `RiskInfo` and
     the exact command for a human). `kline-agent` refuses to start if the feature was unified into it.
   - The `/dev/tty` code is an **anti-accident gate**, not an authentication: it stops a pipe, `--json`,
     a script or a non-interactive agent from sending by mistake. An AI agent that drives a terminal
     (pty) can read and type the code; the owner permits that when he chooses (owner decision
     2026-11-22). Do not present it as protection against an agent with terminal access.
   - Security access `27`, download/upload/transfer `34`-`37`, sessions other than `10 81`/`10 A0` and
     memory/routine-by-address services on non-immobilizer ECUs are Tier 3 Expert with specific
     warnings (`MayBrickEcu`, `MayLeaveProgrammingSession`, `SecurityAccess`), not forbidden (owner
     decision 2026-11-22, final).
   - ISO8 `36` (keep-alive) and `3F` (stop), KWP `81`/`82`: protocol frames, sent only by the init
     machines and `Session::tester_present`/`stop`, never as requests.
3. **Never touch the immobilizer/transponder**, in any tier including expert. This is the only forbidden
   class (owner-approved). On a BMT/UCH/decoder (address `0x26`/`0xA6`, or any ECU whose loaded
   definition is a BMT/UCH/decoder: `kline_core::spec::definition_guard`): `3D`, `38`-`3A`, `2C`;
   `30`/`31`/`32`/`33` C0-C9 (except exact `31C7000000`/`31C8000000`); any `2E`/`2F`; any `3B` except
   `3B D2` and `3B D2` index 2 (AP1CLE); `21A4` with extra bytes. On non-immobilizer ECUs: `31 A0 A3`
   (and `32`/`33`/`38`-`3A` `A0 A3`), writes `2E`/`2F`/`3B` to `A0`/`A2`/`AE`/`BD`; on a Sirius (0x7A or a
   Sirius definition) also output control `30` and routines on those LIDs. Plus the name guard: a Tier 1-3
   request whose definition names look immobilizer/transponder/key related, applied by `Session` in
   kline-core. Reading transponder *status* (BMT `21A4`, exactly 2 bytes) is Tier 0.
4. No network calls at runtime. No telemetry.
