# clio-spec: DDT2000 XML semantics

How `crates/clio-spec` reads a DDT2000 ECU definition and turns response bytes into values.
The reference is the ddt4all Python decoder (GPL-3, `src/ddt4all/core/ecu/`, commit `e109cd2`).
We match its **decoding** exactly. Where we deviate (mostly encoding and error handling), or where
ddt4all and the XML disagree or are ambiguous, this document says so.

Verification: `crates/clio-spec/tests/crosscheck_ddt4all.py` runs ddt4all and our `dump` example on
the same bytes (seeded random frames, full and truncated, for every request of all six ECU files
plus our fixture). Per item it compares the display string, the raw bytes (ddt4all `getHexValue`
against our `raw`) and, for scaled items, the unrounded number (ours against ddt4all's formula
recomputed from its own `getHexValue` and parsed fields, compared with `==`). Run on 2026-11-21
against ddt4all `e109cd2`: 458,652 random item decodes, 0 display, 0 raw and 0 numeric mismatches
(208,680 numeric comparisons); golden + fixture frames: 84 items, 0 mismatches. It also matched
request/data counts, KW1/KW2, address, endianness and FastInit flag for all six files. Against a
deliberately perturbed build (value x (1 + 1e-15), raw | 0x100) the display strings still all
matched while the raw and numeric comparisons failed, so those two checks add coverage.

## Reading the file

- **Encoding.** A BOM wins, else `encoding="..."` from the XML declaration, else UTF-8. Decoding
  is strict: bytes that are invalid in that encoding are an error, never replaced.
  Of our inputs, only `BMT_X65_ph1_p.xml` and `x65_addressing.xml` declare windows-1252;
  `Safir_1/2`, `BMT_SAGEM`, `SIRIUS32...` and `S2000...` declare UTF-8 and are valid UTF-8.
- **Namespace.** `<Target>` (root, or a child of the `<xml>` wrapper) must be in
  `http://www-diag.renault.com/2002/ECU`. Below it, elements are matched by local name.
  `<Projects xmlns="">` is in no namespace and still works.
- **Which elements are searched where** (copied from ddt4all):
  - Direct children of `<Target>`: `Function`, `Projects`, `PowerSupply`, `K`, `CAN`,
    `AutoIdents`, `Description`.
  - Anywhere in the document: every `<Requests>` (its `Endian`) and every `<Request>` below it,
    every `<Data>`.
  - Inside a `<Request>`, anywhere: `Sent`, `Received`, `SentBytes` (inside `Sent`),
    `DataItem` (inside `Sent`/`Received`), `ManuelSend`, `DenyAccess`, `DossierMaintenabilite`,
    `ReplyBytes`, `ShiftBytesCount`, `VariableLength`.
  - Inside a `<Data>`, anywhere: the first `Bytes`, the first `Bits`, every `List/Item`,
    the first `Description`, `Comment`, `Mnemonic`; `Scaled` and `Binary` inside `Bits`.
- **Duplicates.** ddt4all keeps requests, datas and per-request data items in dicts keyed by
  name. We keep document order and let a repeated name replace the earlier entry in place
  (same result as a Python dict). The six files have no duplicates.
- **Numbers.** Surrounding whitespace is allowed (`Step=" .375"`), like Python `int()`/`float()`.
  Empty attributes count as absent (ddt4all's `if attr:`). A decimal comma is accepted only in
  `Bytes@count` (ddt4all does the same).
- **Hex.** `SentBytes`/`ReplyBytes` must be an even number of hex digits. ddt4all does not check.
- **Required vs defaulted.** A `<DataItem>` without `FirstByte` is an error
  (`SpecError::MissingFirstByte`; ddt4all would default to 0 and never decode it). All 2165
  DataItems of our six files have one. The remaining defaults are kept because the attributes are
  legitimately absent in real files (counts over the six files):

  | Attribute | Default | Absent in |
  |---|---|---|
  | `DataItem@BitOffset` | 0 | 1065 of 2165 |
  | `Bits@count` | 8 | 680 of 2040 `<Bits>` |
  | `Scaled@Step` | 1 | 829 of 945 `<Scaled>` |
  | `Scaled@Offset` | 0 | 906 of 945 |
  | `Scaled@DivideBy` | 1 | 919 of 945 |
  | `Received@MinBytes` | 0 | 0 of 149 (all present) |

- **Unread names.** `EcuDef::from_xml_bytes_with_report` also returns a `ParseReport` listing every
  element and attribute the parser does not read, with its path
  (`Target[@Name="t"]/Datas/Data[@Name="speed"]/Bits/Scaled`). This catches misspellings such as
  `Stepp="2"`, which would otherwise fall back to the default silently. An unknown element is
  reported once and its subtree skipped. Children of `Projects` and `DenyAccess` are free-form.
  In our six files the unread names are exactly the elements `Categories` (screen layout),
  `Devices`, `Failures`, `Language`, `DateOfficialisation` and the attributes `Target@upload`,
  `Target@href`, `Target@email` (snapshot test `parse_ddt::unknown_names_snapshot`).
  `<Binary>` and `<ShiftBytesCount>` are read but occur in none of the files.

## `<K>` and `<CAN>`

All of `<K>` is parsed: `BaudRate`; `ISO8` (`L`, `KW1`, `KW2` + `Timeout`, `FirstRequest@Delay`)
either directly under `K` (Safir) or under `KWP` (BMT, SIRIUS); `KWP@Multipoint`, `KWP/FastInit`
(S2000), `KWP/KW2_@Delay`; `InterByte`, `ReplyToRequest`, `RequestToReply@Timeout`,
`TesterPresent`, `OnError`. `KLine::protocol()` follows ddt4all's precedence: KWP before ISO8,
FastInit before ISO8.

- ddt4all reads only the key words, FastInit and the address. It ignores every timing value.
- **Timings are raw `Option<u32>`, assumed to be milliseconds.** clio-spec stores the XML numbers
  unchanged (`InterByte@Delay`, `ReplyToRequest@Delay`, `RequestToReply@Timeout`,
  `TesterPresent@Delay`, `OnError@Delay`, `KW2@Timeout`, `FirstRequest@Delay`, `KW2_@Delay`)
  and does **not** resolve them into a timing set: clio-core owns its timing struct and decides
  defaults and units. The ms assumption fits TesterPresent 3000 = ddt4all's hard-coded 3 s tester
  present, but is not verified against DDT2000.
- **Where 5-baud (ISO8) data can live.** `K.iso8` (`<K><ISO8>`, Safir: plain ISO 9141) and
  `K.kwp.iso8` (`<K><KWP><ISO8>`, BMT, SIRIUS: KWP2000 slow init); each may carry `L`, KW1/KW2,
  `KW2@Timeout` and `FirstRequest@Delay`. A fast-init file has `K.kwp.fast_init` (`<KWP><FastInit>`,
  S2000) with only KW1/KW2 and no timing. `KLine::protocol()` and `KLine::key_words()` pick
  between them; a consumer that needs the ISO8 timings must read the `Iso8` of the selected path.
- A `KWP` with neither `FastInit` nor `ISO8` crashes ddt4all. We report `KProtocol::Unknown`.
- `CAN`: `BaudRate`, `SendId/CANId@Value`, `ReceiveId/CANId@Value`. None of our files has one.

## Data definitions

| Field | Rule (ddt4all) |
|---|---|
| bits_count | default 8; `Bytes@count` n gives 8n; `Bits@count` overrides both counts |
| bytes_count | default 1; `Bytes@count` n; `Bits@count` b gives ceil(b/8) |
| fractional `Bytes@count` (`0,5`) | bytes = ceil(f), bits = int(f*8) |
| signed | `Bits@signed` with **any** non-empty value, `"0"` included |
| ascii | `Bytes@ascii="1"` |
| scaled | `Bits/Scaled`: Step (1), Offset (0), DivideBy (1), Format, Unit |
| list | every `List/Item@Value,@Text`. A repeated Value: the last Text wins |

## Locating an item in a frame (`DataDef::extract`)

- The frame is the **response including its positive response SID** (`73 51 ...` for `33 51`).
  `FirstByte` is 1-based (missing is a parse error; `FirstByte="0"` can never be decoded).
- span = `ceil((bits + BitOffset) / 8)` bytes from `FirstByte`. If the frame is shorter, the item
  decodes to `None` (ddt4all also returns `None`).

## Validating a response (`Request::decode_checked`)

`Request::decode_unchecked` decodes any bytes (ddt4all `get_values_from_stream`); it is for the
ddt4all crosscheck, replaying logs and canned frames. `decode_checked` (request sent with its
default `SentBytes`) and `decode_checked_against(ecu, sent, response)` (any built frame) first
check, in order, and return a typed error:

1. empty: `TooShort { min: max(MinBytes, 1) }`;
2. `7F sid nrc`: `NegativeResponse { sid, nrc }` (`TooShort { min: 3 }` if cut off);
3. byte 1 must be `sent[0] + 0x40`: `UnexpectedSid { expected, got }` (`NoPositiveSid` when the
   request has no SentBytes or its SID is 0xC0 or above, e.g. Safir `FF00`);
4. length >= `Received@MinBytes`: `TooShort { min, len }`;
5. echo: response bytes 2 .. (first received `FirstByte` - 1), limited to the length of `sent`,
   must equal `sent`: `ReplyMismatch { position, expected, got }`. In our files this is always
   exactly byte 2 (the local identifier of `21 xx`, the Safir block number of `33 5x`, the
   routine / IO identifier of `31 xx` / `30 xx`) or nothing (first item at byte 2). Requests
   without received items skip it.

What ddt4all does (`core/ecu/ecu_request.py` `send_request`, `e109cd2`), and how we differ:

- **Negative response:** only `elmstream.startswith('7F')`, NRC from characters 6..8 of the
  spaced hex string, then the request returns `None`. Same detection, but ours is typed and does
  not depend on string formatting.
- **Positive SID and echo:** ddt4all checks neither in `send_request` (its ELM layer can filter
  by a `positive` prefix, which ecu_request never passes). **Deviation:** we reject both.
- **MinBytes:** read and stored, never used for decoding (only the request editor uses it as a
  default length; the macro docs call it the response length including the positive SID).
  **Deviation:** we enforce it. In our files MinBytes is at least the last item's end except in
  SIRIUS `Trame 14` (MinBytes 22, items reach 24), SIRIUS `Trame 30` (14 vs 17) and Safir 1/2
  `ET2` (5 vs 7); there a response of exactly MinBytes passes and the items beyond it decode
  to `None`.
- **ReplyBytes:** used only as the canned answer in simulation mode. **We do not compare against
  it.** It occurs 4 times (SIRIUS `Output Control`, `.Status`, `.Stop`; S2000 `Output Control`),
  always `700000` while the request is `30 D0 ...` with a sent data item at byte 2, so it holds
  zeros where the ECU echoes the parameter. Checking against it would reject the expected echo.
- Endianness: `DataItem@Endian` if present, else the `Requests@Endian` of the file. ddt4all keeps
  a single global value (the last `<Requests>` with the attribute wins). We do the same. Unknown
  values are an **error** for us. ddt4all would silently treat them as big endian.
- **Big endian:** BitOffset counts **from the MSB** of the span. Bits
  `[BitOffset, BitOffset+bits)` are taken MSB-first. So BitOffset 7 with 1 bit is the LSB of
  `FirstByte` (BMT `TIR RF RECU`).
- **Little endian:** the span is read as one big-endian integer, shifted right by BitOffset
  (**counted from the LSB**), masked to `bits`, written as `ceil(bits/8)` big-endian bytes, and
  those bytes are reversed.
  - Ambiguity: for 1-bit items this flips the bit numbering. `Endian="Little" BitOffset="7"` is
    the **MSB** (Safir_1 `regulation richesse active`), where the same item without the attribute
    would be the LSB. The XML authors seem to use `Endian="Little"` for exactly this effect
    (Safir_1 2 items, SIRIUS 1, S2000 several), but this is not verified against DDT2000.
  - Ambiguity: for bit counts that are not a multiple of 8, the byte reversal gives odd values
    (12 bits of `34 12` give `0x1204`). We copy ddt4all. Our files use little endian only for
    1-bit, 16-bit and whole-`Bytes` items.

## From raw to value (`DataDef::interpret`)

1. `ascii`: the bytes are decoded as UTF-8 and **invalid bytes are dropped** (Python
   `errors='ignore'`). Ambiguity: DDT2000 is a Windows program, so a 0xE9 byte was probably
   meant as windows-1252 `é`. ddt4all (and we) drop it.
2. `signed` (`Bits@signed` with any non-empty value: `signed="0"` also means signed; all 15
   occurrences in our files are `signed="1"`; see the traps below): applied **only when
   bytes_count is 1 or 2**. The result is
   `-(v & 0x80) | (v & 0x7f)` (or the 0x8000 form). The sign bit is always bit 7/15, whatever
   bits_count is, and higher bits are dropped. Other widths stay unsigned; ddt4all only prints a
   warning for them.
3. Not scaled: list lookup with the (signed) value. Without a match, the value is the
   **lowercase hex** of the raw bytes (`Value::Hex`), exactly what ddt4all shows.
4. Scaled: `(v * Step + Offset) / DivideBy` in f64, in that order. **The List is ignored** when
   `Scaled` is present. If DivideBy is 0, there is no value (ddt4all prints an error and returns
   `None`).
5. Display string (`DecodedItem::display`):
   - If Format contains `.`: Python `'%.Nf'`, where N is the number of characters between the
     first and the second `.`. Rounding is half-to-even on the exact binary value
     (`-39.375` gives `-39.38`, `-38.125` gives `-38.12`). Rust's `{:.N}` behaves the same way;
     this was checked with tests and in the crosscheck.
   - Otherwise, if the value is integral, the integer (`-0.0` gives `0`).
   - Otherwise Python `str(float)`: shortest round-trip digits, with an exponent below 1e-4 or
     from 1e16 up (`1e-05`).
   - Ambiguity: Formats without a `.` are ignored. `CODAPV` with `Format="00"` (BMT) shows `3`,
     where DDT2000 probably shows `03`. The 345 `Format="bin"` (17 each in Safir_1/2, 130 in SIRIUS,
     181 in S2000) show decimal, where DDT2000 probably shows binary. Not verified. On the Safir
     these are the fault flag bytes, so the raw integer in `DecodedItem::raw` is what callers should
     use.

## Traps for callers

- **`Bits@signed` is a presence flag.** Any non-empty value makes the data signed, `"0"`
  included (ddt4all `if signed:` on the string). Empty counts as absent.
- **The sign bit is bit 7 or bit 15 of the extracted value, never the top bit of the field.**
  A signed 4-bit field is shown unsigned (0..15), a signed 12-bit field uses bit 15, which is
  always 0, so it is never negative. Widths over 2 bytes are never sign-extended.
- **`Format="bin"` and `Format="00"` display decimal.** Only formats containing `.` are applied.
  A CLI that wants binary or zero padding formats `DecodedItem::raw_int()` (the raw integer,
  `None` for ASCII / items over 8 bytes / missing) itself, using `DataDef::bits_count` as width.

## Encoding (`DataDef::encode`, `encode_exact`, `DataDef::insert`, `Request::build`)

Encoding is not used for anything this project sends yet. It is not covered by the crosscheck.

- `Value::Number` on scaled data: `raw = round((x * DivideBy - Offset) / Step)`.
  **Deviation:** ddt4all truncates with `int()`. That loses steps to float error. With truncation,
  the round-trip property test fails at once (Safir_1 `pression collecteur absolue mesuree`:
  raw 3 decodes to 114.1 and truncates back to 2). For an off-grid input, the two can pick
  different raw values (1503 rpm / Step 2 gives 752 for us and 751 for ddt4all).
- `encode_exact` refuses off-grid numbers instead of rounding: if
  `(x * DivideBy - Offset) / Step` is more than `DataDef::GRID_TOLERANCE` (1e-6) steps away from
  an integer it returns `SpecError::OffGrid { value, nearest }` (1503 rpm with Step 2: error,
  nearest 1504). The tolerance absorbs f64 noise only: with 0 tolerance the round-trip property
  test fails on real data, with 1e-6 every decoded value of Safir 1/2 and BMT re-encodes.
  Other value kinds behave like `encode`.
- Negative values give two's complement. They are allowed only for signed data whose sign width
  (8/16) equals bits_count. Anything that does not fit, or would decode to a different value,
  is an `Encode` error. ddt4all writes garbage in these cases (Python `bin()` of a negative or
  oversized value).
- `Value::Text`: a list label (the last item with that Text wins), or ASCII text padded with
  spaces or truncated to bytes_count (as in ddt4all). Non-ASCII text is an error.
- `Value::Hex`: the raw value in hex. It must fit in bits_count.
- `insert` is the exact inverse of `extract`, for both endiannesses. Bits of the frame outside the
  item are kept.
  - **Disagreement:** ddt4all's `setValue` for little endian does **not** invert its own
    `getHexValue` for multi-byte items. It writes the value in big-endian byte order. Verified by
    running ddt4all: value `0x1234`, 16 bits, `Endian="Little"`, FirstByte 1 is written as
    `12 34`, and ddt4all reads that back as `3412`. We write `34 12`, which reads back as `1234`
    (pinned by `fixture::little_endian_insert_is_true_inverse_not_ddt4all_set_value`).
    This affects only S2000 sent items (`RFFD Lecture contexte panne`, `SRBLI start test sav`),
    which this project does not use.

## Request attributes

- `ManuelSend`, `DenyAccess` children (`Plant`, `AfterSales`, `Engineering`, `Supplier`),
  `DossierMaintenabilite`, `VariableLength`, `ReplyBytes` and `ShiftBytesCount` are parsed as
  data. They have no effect on decoding.
- `Ref="1"` is parsed into `DataItem::reference`. ddt4all reads it but never uses it, and its
  meaning is unknown. All data items have it, except 11 in SIRIUS and 62 in S2000.
- Lookup by name: exact match first, then case-insensitive (ddt4all `get_request`).
  `request_by_bytes` returns all matches, because different requests can share the same bytes
  (BMT `IDENTIFICATION` and `TYPE DE CARTE` are both `2180`).

## Other differences from ddt4all

- ddt4all parses `<Data>` only inside its loop over `<Requests>` elements. A file without
  `<Requests>` would have no data. We always parse every `<Data>`.
- Empty `<Description>` or `<Comment>` crashes ddt4all. We get an empty string.
- Values wider than 16 significant bytes are converted to f64 byte by byte, which can double-round.
  ddt4all uses Python's exact int-to-float conversion. Only theoretical: no scaled item in our
  files is wider than 24 bits.
- `Bits count="0"` decodes to `None` (ddt4all would raise an exception).
- Numeric encode is limited to 1..=64 bits.

## Vehicle addressing file (`Addressing`)

`<Functions>` with `<Tool Address>`, a top-level `<ProtocolList>` (`Code`, `InitMode`,
`MultiPoint`, `Name`, `<Search/>`), and per `<Function Address Name>`: localized `<Name xml:lang>`,
`XId`, `RId`, optional `ISO8` (an alternative 5-baud address, e.g. UCH 38 has `ISO8` 174), and its
own `<ProtocolList>`. ddt4all has no equivalent in `core/ecu`, so this parser is not
cross-checked.
