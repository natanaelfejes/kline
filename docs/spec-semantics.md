# clio-spec: DDT2000 XML semantics

How `crates/clio-spec` reads a DDT2000 ECU definition and turns response bytes into values.
The reference is the ddt4all Python decoder (GPL-3, `src/ddt4all/core/ecu/`, commit `e109cd2`).
We match its **decoding** exactly. Where we deviate (mostly encoding and error handling), or where
ddt4all and the XML disagree or are ambiguous, this document says so.

Verification: `crates/clio-spec/tests/crosscheck_ddt4all.py` runs ddt4all and our `dump` example on
the same bytes. On 2026-11-21 it compared 458,652 item decodes (seeded random frames, full and
truncated, for every request of all six ECU files plus our fixture) with 0 mismatches in the
display string, and matched request/data counts, KW1/KW2, address, endianness and FastInit flag
for all six files.

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

## `<K>` and `<CAN>`

All of `<K>` is parsed: `BaudRate`; `ISO8` (`L`, `KW1`, `KW2` + `Timeout`, `FirstRequest@Delay`)
either directly under `K` (Safir) or under `KWP` (BMT, SIRIUS); `KWP@Multipoint`, `KWP/FastInit`
(S2000), `KWP/KW2_@Delay`; `InterByte`, `ReplyToRequest`, `RequestToReply@Timeout`,
`TesterPresent`, `OnError`. `KLine::protocol()` follows ddt4all's precedence: KWP before ISO8,
FastInit before ISO8.

- ddt4all reads only the key words, FastInit and the address. It ignores every timing value.
  We store the raw numbers. **The unit is assumed to be ms** (TesterPresent 3000 = 3 s matches
  ddt4all's hard-coded 3 s tester present). This is not verified against DDT2000.
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
  `FirstByte` is 1-based. 0 or missing means the item can never be decoded.
- span = `ceil((bits + BitOffset) / 8)` bytes from `FirstByte`. If the frame is shorter, the item
  decodes to `None` (ddt4all also returns `None`). **`Received@MinBytes` is not enforced** by the
  decoder (ddt4all ignores it as well). Callers that need it must check it themselves.
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
2. `signed`: applied **only when bytes_count is 1 or 2**. The result is
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

## Encoding (`DataDef::encode`, `DataDef::insert`, `Request::build`)

Encoding is not used for anything this project sends yet. It is not covered by the crosscheck.

- `Value::Number` on scaled data: `raw = round((x * DivideBy - Offset) / Step)`.
  **Deviation:** ddt4all truncates with `int()`. That loses steps to float error. With truncation,
  the round-trip property test fails at once (Safir_1 `pression collecteur absolue mesuree`:
  raw 3 decodes to 114.1 and truncates back to 2). For an off-grid input, the two can pick
  different raw values (1503 rpm / Step 2 gives 752 for us and 751 for ddt4all).
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
    `12 34`, and ddt4all reads that back as `3412`. We write `34 12`, which reads back as `1234`.
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
