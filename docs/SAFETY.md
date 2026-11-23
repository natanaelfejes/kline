# Safety rules

kline can send requests that change an ECU, so it has a small set of rules that are enforced in
code and covered by tests. Code comments refer to them as "SAFETY.md rule N". The exact request
bytes for each tier are in [protocol.md](protocol.md#safety-policy).

In short: reads are free, anything else needs a human to approve that exact request in a
terminal, the immobilizer is never touched, and on the ABS and the airbag nothing that can leave
a brake valve open, disable an airbag or erase crash data is ever sent (rule 5).

## Rule 1: Renault data never enters git

`ddt-xml/` and any generated spec JSON are gitignored. Tests that need them read from `ddt-xml/`
at test time and are skipped (not failed) with a clear message if absent. Hand-written minimal
XML fixtures that you author yourself may live in `crates/*/tests/fixtures/`.

## Rule 2: the safety policy is enforced in kline-core, not in the CLI

Default-deny, decided on exact payload bytes and the target of the init that actually ran
(owner-approved tiers, [protocol.md "Safety policy"](protocol.md#safety-policy)):

- **Tier 0 READ**: documented reads only (KWP `21 xx`/`1A xx` exactly 2 bytes, `17`/`18`, `3E`,
  `10 81`, `10 A0` (not on the airbag `0x2C`); Safir `21`, `3350`, `3351`, `3352`, `3354`;
  Bosch 5.3 ABS (ISO8 at `0x01`, key bytes `DC 83`) exactly `51` and `58`). The only tier library code paths, `--json`, MCP and tests against a
  real port may execute.
- **Tier 1 SERVICE** (clear DTC `14`, output tests `30`, adaptation reset `11 FF`/`32`, Sirius
  `31A000` injector inhibition, BMT `31C7`/`31C8` remote learning and `31 out` actuators; see "Output control ids" below), **Tier 2 CONFIG WRITE**
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
- **Airbag ECU.** Requests to the airbag ECU (`0x2C` in the X65 addressing) that rule 5 does
  not forbid follow the normal tiers, but every non-read carries the `SafetySystem` warning, an airbag risk text
  (`RiskInfo::safety_system`: disabled airbags or pretensioners, airbag lamp, squib on an
  output test) and airbag prerequisites, in addition to the request's own warnings, text and
  prerequisites (none of those is dropped).
- **Protocol frames.** ISO8 `36` (keep-alive) and `3F` (stop), KWP `81`/`82`: sent only by the
  init machines and `Session::tester_present`/`stop`, never as requests.
- **One send per approval.** An approved request is written exactly once. `exec --hold` keeps
  the session open afterwards with TesterPresent only, `--watch` adds one Tier 0 read (checked
  before the port is opened and again by kline-core); neither ever sends the approved request
  again (final critique F01). If the reply is lost after the write, kline says that the request
  WAS SENT and to read the state before repeating it (exit 13, F06).
- **Output control ids (audit 2026-11-23).** On a Sirius (address `7A` or a Sirius definition)
  `30 id cmd cycles` is Tier 1 only when it is exactly 4 bytes, `id` is one of the 13 outputs of
  the "Output Temporary Control List" of SIRIUS32___EMS3132___Soft_RE1_maj.xml (`80` fuel pump relay,
  `82` A/C output, `83` idle valve RR, `86` canister purge, `88` upstream O2 heater, `8F` camshaft 1,
  `94` MIL lamp, `9E` coolant temperature warning lamp, `9F` fan low, `A0` fan high, `A7` downstream
  O2 heater, `AA` thermo-plunger relay 1, `AC` shift lamp) and `cmd` is `00` start, `01` status or
  `11` stop; a start needs cycles 1..=10 (0 or more than 10 is Expert, "long or unlimited actuation"), status and stop need `00` as the 4th byte. Any other id, command or length (including the definition's placeholder id `D0`) is
  Tier 3 Expert, not forbidden. `82` stays Tier 1 but its `RiskInfo` says this car has no A/C.
  Other (non-Sirius, non-BMT) ECUs keep `30 lid` + 1-4 bytes as Tier 1. The S2000 definition (Generic class) lists
  more ids (`87`, `8B`, `8C`, `B5`..): at address `7A` the Sirius allowlist applies to it and those ids are
  Expert; at any other address, with no Sirius definition, it keeps `30 lid` + 1-4 bytes as Tier 1. The A/C text is Sirius-only.
- **ABS / brake ECU (audit 2026-11-23, owner decision 2026-11-23).** The Bosch 5.3 ABS
  (target ISO8 at `0x01` that answered key bytes `DC 83`, car-verified; another ISO8 unit at
  `0x01` keeps the generic ISO8 tiers) has only two reads: `51` identification and `58` fault
  memory, exactly one byte (Tier 0). `45` clears the fault memory (Tier 1, `ErasesFaultMemory`).
  Every `41` RAM download and `71` routine run is forbidden (rule 5). Safir `21`/`33` are not
  reads there (Expert). Every non-read carries `SafetySystem` first, a brake text and brake
  prerequisites (stationary, engine off, handbrake; afterwards ignition cycle, ABS lamp out,
  firm pedal, brake test at walking speed), like the airbag rule. On the ISO8 unit at `0x01`
  (any key bytes) `Session` refuses a second approved request in one session
  (`OneApprovedRequestPerSession`), and `kline exec` refuses a non-read `--stop` there before
  opening the port: no approved request can follow another one (defence in depth).
- **BMT actuator `A4`/`A5`** (unnamed "LO"/"LF" lines) are Tier 3 Expert; the other outputs
  `A0`..`B3` stay Tier 1, `A6`/`AB`/`AC` stay forbidden, `B4`+ stays Expert.
- **BMT actuators.** `31 out param count` stays Tier 1; on the lock motors (`A0`/`A1`) and the
  window (`A2`/`A3`) count `FF` carries "count FF = permanent: lock/window motor may run until
  stopped; use 1-3" (final critique F07, not forbidden).

## Rule 3: never touch the immobilizer or transponder

In any tier, including expert (owner-approved). The other forbidden class is rule 5 (ABS and
airbag).

- **On a BMT/UCH/decoder** (address `0x26`/`0xA6`, or any ECU whose loaded definition is a
  BMT/UCH/decoder: `kline_core::spec::definition_guard`): `3D`, `38`-`3A`, `2C`;
  download/upload/transfer `34`-`37` and flash erase/checksum `31`/`32`/`33` `81`/`82` (they
  overwrite or erase the key-code EEPROM); `30`/`31`/`32`/`33` C0-C9
  (except exact `31C7000000`/`31C8000000`) or with the immobilizer actuator outputs `A6`
  "Ligne Transpondeur", `AB` "Led Verlog", `AC` "Ligne ECM" (both BMT definitions, step 7);
  any `2E`/`2F`; any `3B` except `3B D2`, and `3B D2` index 2 (AP1CLE) or any index whose low
  nibble is 2 (`12`, `22`, .. `F2`: an alias of AP1CLE if the firmware masks the index, NOT
  VERIFIED); `21A4` with extra
  bytes. `3B D2` index 3 (TRF) is the remote type, RF or infrared, not key related: Tier 2 with
  a specific warning (`ChangesRemoteType`); a value other than 0/1 is also Tier 2 with that
  warning and the text "unknown TRF value".
- **On non-immobilizer ECUs**: `31 A0 A3` (and `32`/`33`/`38`-`3A` `A0 A3`), an earlier
  reading of the backed-up memory reset, kept refused.
- **On a Sirius** (0x7A or a Sirius definition): `31 A3 ..`, `32 A3 ..` and `33 A3 ..`, the
  real backed-up memory reset ("Reinit memoire sauvegardee", function 0xA3; the backed-up
  memory holds the verrou logiciel). In the Sirius 32 definition "Fonctions particulieres" is
  `31A000` with the function number at FirstByte 2 (1-based), so a request is `31 fn 00`
  ("Arret fonctions" `32 fn 00`, "Status fonctions" `31 fn 01`); injector inhibition is
  `31 A0 00` (Tier 1) and `32 A0 00` ends it (final critique F02). The other functions (A1
  power latch stop, A2 OBDFC tests, A4, A6 static test, A7 fuel pump priming) stay Expert.
  Also writes `2E`/`2F`/`3B` to the frame
  identifiers `A0`/`A2`/`AE`/`BD` (the `21` frames that carry the immobilizer / "verrou
  logiciel" fields). Output control `30` and routines `31`-`33`/`38`-`3A` whose byte 2 is one
  of those values are other number spaces (`30 A0` is the fast radiator fan, `31 A2 00 00 00`
  "SRBLI start test sav", `31 A2 01 00 00` "Status tests OBDFC") and keep their normal tiers
  (security review M1). On other ECUs those numbers mean other things, so the write rule is
  Sirius only.
- **Name guard**: a Tier 1-3 request whose definition names look immobilizer/transponder/key
  related, applied by `Session` in kline-core ([cli.md](cli.md#name-guard)). The texts of an
  enumerated sent parameter count too: the value whose text has such a word is refused (fixed
  bytes plus that byte), or the whole request when the parameter is not one whole byte. A flag
  needs at least two distinguishing bytes (the fixed prefix, else the whole SentBytes
  template); a service byte alone never flags. Non-read variants of a flagged read are refused
  on purpose (intended, security review L2): a `21 xx` with extra bytes can write (BMT `21 A1`
  with a third byte writes CALTERMI), so on a read flagged by its received items only the exact
  two-byte read stays allowed (Tier 0).
- **Allowed**: reading transponder *status* (BMT `21A4`, exactly 2 bytes) is Tier 0.

Writing, learning, resetting or disabling the immobilizer is never implemented.

## Rule 4: no network, no telemetry

No network calls at runtime. No telemetry.

## Rule 5: safety systems: ABS fill / bleed / routines and airbag firing lines

In any tier, including expert; no flag, feature or approval unlocks it (owner decision
2026-11-23). Enforced in `kline-core` (`safety::forbidden`), on the exact bytes and the init
target, also without a loaded definition. Refusal reasons: `DenyReason::BoschAbsRoutine` and
`DenyReason::SafetySystemForbidden`; the message names the request and says "permanently
forbidden".

### ABS / brake ECU (Bosch 5.3, ISO8 at `0x01`, key bytes `DC 83`)

**Why.** `41 04 ..` downloads Intel 80C196 machine code into the ABS RAM and `71 ..` runs it:
the `71` frames open inlet / outlet valves and run the return pump. A valve or the pump stays
energized until the matching OFF / FIN frame. kline has no sequence executor that would send
that frame by itself on an error, a lost reply or Ctrl-C, and **whether the ECU releases its
outputs when it receives `3F` or when the keep-alive `36` stops is NOT VERIFIED**. A reviewer
showed that a download followed by a `71` was reachable (`exec 41.. --stop 71..`). So every
`41` and every `71` is forbidden, and the factory fill and bleed are forbidden by name: they
need the filling rig or a garage bleeding procedure.

| Name (X24_ABS_Bosch5.3) | Bytes | Purpose | Why forbidden |
|---|---|---|---|
| any download | `41 ..` | load 80C196 code into the ABS RAM (DOWNLOAD_*, DL_*) | the code comes from other vehicles' definitions and calls ROM entry points not verified on this ABS software (`51`: `02 49 12`); it prepares a `71` |
| any routine run | `71 ..` | run the routine loaded last (valves, pump, EEPROM coding, live values) | outputs can stay energized; release on `3F` / keep-alive loss NOT VERIFIED |
| DOWNLOAD_remplissage_usine | `41 04 00 00 00 00 12 06 00 ..` (131 bytes) | factory fill routine | factory filling rig only: permanently forbidden, use a garage |
| DOWNLOAD_PURGE | `41 04 00 00 00 00 12 06 00 ..` (134 bytes) | brake bleed routine | brake bleed: permanently forbidden, use a garage |
| DOWNLOAD_Y265K14311 | `41 04 00 00 BB 07 07 64 07 ..` (113 bytes) | valve / pump driver the bleed frames run | brake bleed: permanently forbidden, use a garage |
| MES_REMPLISSAGE | `71 B8 0B E8 03 16 00 52 54 22 64` | factory fill, start parameters | factory filling rig only |
| MES_REMPLISSAGE_Message2..8 | `71 FF 52 FF 54 00 00 FF 40 FF 42`, `71 FF 30 FF 32 00 00 FF 34 FF 36`, `71 FF 38 FF 3A 00 00 FF 3C FF 3E`, `71 00 52 00 54 00 00 00 40 00 42`, `71 00 30 00 32 00 00 00 34 00 36`, `71 00 38 00 3A 00 00 00 3C 00 3E`, `71 00 22 00 62 00 00 FF 64 00 00` | factory fill steps: valve pairs and pump ON / OFF | factory filling rig only |
| MES_PURGE_GARAGE_{AVG,AVD,ARG,ARD,ASV1,ASV2}_{ON,OFF}, MES_PURGE_GARAGE_FIN | 11 bytes each, e.g. `71 FF 30 FF 32 00 00 00 00 00 00` (ARD_ON), `71 00 30 00 32 00 00 00 00 00 00` (ARD_OFF), `71 00 22 00 62 00 00 FF 64 00 00` (FIN) | garage bleed: opens the valves of one wheel / the ASR valves, then ends | brake bleed: permanently forbidden, use a garage |
| MES_PURGE_SEC_21/22/23/25, MES_PURGE_ECH_ARG/ARD, MES_PURGE_DESACTIVE_24/25/26/CONDIT, MES_PURGE_CONDITIONS | 11 bytes each, e.g. `71 00 52 FF 54 00 00 00 00 00 00` (SEC_21), `71 FF 38 FF 3A E8 03 00 38 00 3A` (ECH_ARD) | bleed sequence steps (secondary circuit, exchange, deactivate, conditions) | brake bleed: permanently forbidden, use a garage |

The complete 32 frames are `safety::BOSCH_ABS_FILL_BLEED_FRAMES`; the test
`crates/kline-core/tests/bosch_abs_forbidden.rs` checks them and the download headers against
the XML both ways (skipped without `ddt-xml/`). Only the 9-byte headers of the downloads are in
the code, not the Renault routine code (rule 1).

Also on the ISO8 unit at `0x01` (any key bytes): the generic engine forms `11 FF`, `31 A0 00`,
`32 A0 00` and `30 lid` + 1-4 bytes (no ABS requests), and a second approved request in one
session.

**Future path, NOT IMPLEMENTED.** A sequence executor that only sends frames pinned by exact
bytes in the definition's order and sends the OFF / FIN frame automatically on any error, lost
reply or Ctrl-C, after the output release on `3F` / keep-alive loss has been verified on the
car. Until then nothing above is reachable, and the factory fill and bleed stay forbidden even
then.

### Airbag ECU (address `0x2C`, any protocol, with or without a definition)

**Why.** Owner policy: airbag faults are not cleared yet, and nothing touches the firing
lines or the crash data. Before this rule `3B D0`/`3B D6`/`3B E0` were refused only by
name-guard false positives ("verrou", "cle"), `30 80` and `14 FF 00` were Tier 1, `10 A0` was
Tier 0 and `3B D2`/`D3`/`D7`, `10 80` were Expert (command inventory 2026-11-23, AB70_X65 and
AB80_X65_X90_V1).

| Name (AB70 / AB80) | Bytes | Purpose | Why forbidden |
|---|---|---|---|
| Verrouillage / Deverrouillage MAF; Inhibition-Reactivation AB_PSG | `3B D0 ..` (`00` unlock, `FF` lock, `55` / `AA` passenger airbag active / inhibited) | firing line lock state, passenger airbag inhibit | a locked line or an inhibited airbag does not fire, silently |
| Correction impedances lignes (frontales) | `3B D2 ..` | squib line impedance windows | a broken firing line can look healthy (lamp off, airbag cannot fire) |
| Correction impedances lignes laterales | `3B D3 ..` | side squib line impedance windows | same |
| Configuration 2 du systeme | `3B D6 ..` | which airbags, pretensioners, crash output exist | SANS for a present component stops monitoring and firing it |
| ResetCptRobust | `3B D7 ..` | robustness counter reset | crash-record related, effect unknown |
| Reset compteur crash / ResetBE | `3B E0 ..` | crash counter reset, supplier mode | erases crash evidence |
| ClearDiagInfo / Reinitialisation EEPROM | `14 ..` (any) | fault memory clear; on AB80 `14 FF 00` resets the EEPROM | no airbag fault clear yet (owner); EEPROM reset can unconfigure the airbags |
| Activation sortie crash | `30 ..` (any; AB80 `30 80 00`/`01`/`12`) | drives the crash output line | simulates a crash signal to the other ECUs |
| StartDiagSession other than `10 81` | `10 80` supplier, `10 A0` BE (engineering), `10 B0` production, `10 C0` after-sales, and any `10` other than exactly `10 81` | privileged sessions | gate of the forbidden writes; `10 A0` is the engineering session on this ECU, not a read |
| generic engine forms | `11 FF`, `31 A0 00`, `32 A0 00` | adaptation reset / injector inhibition elsewhere | no airbag requests |

Tier 0 on the airbag stays the reads: `21 xx`, `1A xx`, `17`/`18`, `3E`, `10 81`. The dates
(`3B B0`, `3B C0`) and configuration 1 (`3B D1`) stay Tier 2 with the airbag warnings. The
name guard is unchanged (it still refuses immobilizer-named requests on every ECU).
