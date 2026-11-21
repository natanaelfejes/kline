#!/usr/bin/env python3
"""Differential check of clio-spec against the ddt4all reference decoder (GPL-3).

Not run by `cargo test` (needs Python, a ddt4all checkout and the Renault XML files).
Run from the repository root:

    python3 -I crates/clio-spec/tests/crosscheck_ddt4all.py \
        --ddt4all-src /path/to/ddt4all/src [--xml-dir ddt-xml] [--frames 200]

What it does:
  1. Copies ddt4all's package to a temporary directory (ddt4all writes a config.json next to
     its source tree on import) and stubs its ELM327 module (pulls in pyserial, never used here).
  2. Loads every ECU XML with ddt4all's EcuFile and with our `dump` example, compares request
     and data counts, protocol, KW1/KW2, function address and endianness.
  3. GOLDEN: decodes the fixed frames used by tests/golden_decode.rs and prints ddt4all's
     display strings (recorded as comments in that test).
  4. FIXTURE: decodes the frame of tests/fixture.rs on our hand-written fixture.
  5. RANDOM: for every request with received items, decodes N seeded random frames (full length
     and truncated) with both decoders and compares, for every item:
       - the display string, exactly;
       - the raw bytes: ddt4all's getHexValue against our `raw` (int or hex bytes);
       - the numeric value of scaled items: our `value` against ddt4all's unrounded result. ddt4all
         only returns the display string, so the script recomputes its formula
         `(float(v) * float(step) + float(offset)) / float(divideby)` from its own getHexValue and
         parsed fields (signed via its hex8/hex16_tosigned); if that fails it falls back to
         float(display). Compared with ==, no tolerance.
  6. Prints ddt4all's little-endian setValue result for a 16-bit item (documented divergence).

Exit status 0 only if every comparison matched.
"""

import argparse
import json
import os
import random
import shutil
import subprocess
import sys
import tempfile
import types

FILES = [
    "Safir_1.xml",
    "Safir_2.xml",
    "BMT_X65_ph1_p.xml",
    "BMT_SAGEM.xml",
    "SIRIUS32___EMS3132___Soft_RE1_maj.xml",
    "S2000_Atmo___SoftA3.xml",
]


def golden_frames():
    """Same bytes as tests/golden_decode.rs. Keep in sync."""
    p2 = bytearray(25)
    p2[0], p2[1] = 0x73, 0x51
    p2[3] = 0x90  # byte 4: position papillon=2, estompage de couple=1
    p2[5] = 0x08  # byte 6: etat antivol (BitOffset 4) = 1
    p2[13], p2[14] = 0x0B, 0xB8  # bytes 14-15: regime moteur moyen = 3000
    p2[15] = 200  # byte 16: pression collecteur
    p2[17] = 100  # byte 18: tension sonde O2
    p2[18] = 200  # byte 19: temperature eau
    p2[19] = 104  # byte 20: temperature air
    p2[20] = 160  # byte 21: tension batterie
    p2[21] = 1  # byte 22: vitesse du vehicule (list)
    p2[24] = 10  # byte 25: depression altimetrique
    p3 = bytearray(29)
    p3[0], p3[1] = 0x73, 0x52
    p3[20] = 0xF0  # byte 21: RCO ralenti, signed -16
    p3[21], p3[22] = 0x03, 0x20  # bytes 22-23: regime de consigne = 800
    p3[23], p3[24] = 0x03, 0xE8  # bytes 24-25: temps injection = 1000
    p3[25] = 120  # byte 26: avance
    p3[26] = 128  # byte 27: facteur richesse
    tir = bytes([0x61, 0xA3, 0x85, 0x02, 0x03, 0x00])
    return [
        ("Safir_2.xml", "P2 - Ctrl entrees", bytes(p2)),
        ("Safir_2.xml", "P3 - Ctrl sorties", bytes(p3)),
        ("BMT_X65_ph1_p.xml", "ETATS TIR", tir),
    ]


def load_ddt4all(src):
    tmp = tempfile.mkdtemp(prefix="ddt4all-xcheck-")
    shutil.copytree(os.path.join(src, "ddt4all"), os.path.join(tmp, "src", "ddt4all"),
                    ignore=shutil.ignore_patterns("__pycache__"))
    sys.path.insert(0, os.path.join(tmp, "src"))
    try:
        import ddt4all.core.elm as elm_pkg  # light package __init__
        stub = types.ModuleType("ddt4all.core.elm.elm")
        sys.modules["ddt4all.core.elm.elm"] = stub
        elm_pkg.elm = stub
        from ddt4all.core.ecu.ecu_file import EcuFile  # noqa: E402
    except Exception:
        shutil.rmtree(tmp, ignore_errors=True)
        raise
    return EcuFile, tmp


def max_reach(ecu, req):
    end = 0
    for name, di in req.dataitems.items():
        d = ecu.data[name]
        end = max(end, di.firstbyte - 1 + (d.bitscount + di.bitoffset + 7) // 8)
    return end


def ours(dump, path, request, frames):
    args = [dump, path, "--request", request]
    for f in frames:
        args += ["--response", f.hex().upper()]
    out = subprocess.run(args, check=True, capture_output=True, text=True).stdout
    return [json.loads(line) for line in out.splitlines()]


def theirs(ecu, request, frame):
    import io
    import contextlib
    with contextlib.redirect_stdout(io.StringIO()):  # ddt4all prints warnings
        return ecu.requests[request].get_values_from_stream(frame.hex().upper())


def ref_raw_and_number(ecu, request, name, frame):
    """(raw hex or None, unrounded number or None) as ddt4all computes them."""
    from ddt4all.core.ecu.utils import hex8_tosigned, hex16_tosigned
    d = ecu.data[name]
    di = ecu.requests[request].dataitems[name]
    hexval = d.getHexValue(frame.hex().upper(), di, ecu.endianness)
    if hexval is None or d.bytesascii or not d.scaled:
        return hexval, None
    try:
        v = int("0x" + hexval, 16)
        if d.signed and d.bytescount == 1:
            v = hex8_tosigned(v)
        elif d.signed and d.bytescount == 2:
            v = hex16_tosigned(v)
        if float(d.divideby) == 0:
            return hexval, None
        return hexval, (float(v) * float(d.step) + float(d.offset)) / float(d.divideby)
    except Exception:  # fall back to the rounded display string
        disp = d.getDisplayValue(frame.hex().upper(), di, ecu.endianness)
        return hexval, None if disp is None else float(disp)


class Stats:
    def __init__(self):
        self.n = {"display": 0, "raw": 0, "number": 0}
        self.bad = {"display": 0, "raw": 0, "number": 0}

    def check(self, kind, ok, msg):
        self.n[kind] += 1
        if not ok:
            self.bad[kind] += 1
            if sum(self.bad.values()) < 30:
                print(f"  BAD {kind} {msg}")
        return ok

    def failures(self):
        return sum(self.bad.values())


def compare_items(stats, ecu, request, frame, ref_display, our_items, label):
    """Compare display, raw and numeric value of every item ddt4all decoded."""
    mine = {i["name"]: i for i in our_items}
    for name, disp in ref_display.items():
        item = mine.get(name, {})
        where = f"{label} {request!r} {name!r} frame={frame.hex()}"
        stats.check("display", item.get("display") == disp,
                    f"{where} ddt4all={disp!r} ours={item.get('display')!r}")
        ref_hex, ref_num = ref_raw_and_number(ecu, request, name, frame)
        raw = item.get("raw")
        if ref_hex is None or raw is None:
            ok = ref_hex is None and raw is None
        elif isinstance(raw, int):
            ok = raw == int(ref_hex, 16)
        else:
            ok = raw.upper() == ref_hex.upper()
        stats.check("raw", ok, f"{where} ddt4all={ref_hex!r} ours={raw!r}")
        if ecu.data[name].scaled and not ecu.data[name].bytesascii:
            v = item.get("value")
            got = v["value"] if v and v.get("kind") == "number" else None
            stats.check("number", got == ref_num, f"{where} ddt4all={ref_num!r} ours={got!r}")
    if set(mine) != set(ref_display):
        stats.check("display", False, f"item set {label} {request!r}")


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--ddt4all-src", default=os.environ.get("DDT4ALL_SRC"), required=False)
    ap.add_argument("--xml-dir", default="ddt-xml")
    ap.add_argument("--frames", type=int, default=200)
    ap.add_argument("--seed", type=int, default=20261121)
    a = ap.parse_args()
    if not a.ddt4all_src:
        sys.exit("need --ddt4all-src or DDT4ALL_SRC")

    subprocess.run(["cargo", "build", "-q", "-p", "clio-spec", "--example", "dump"], check=True)
    dump = os.path.join(os.environ.get("CARGO_TARGET_DIR", "target"), "debug", "examples", "dump")
    EcuFile, tmp = load_ddt4all(a.ddt4all_src)
    failures = 0
    compared = 0
    stats = Stats()  # GOLDEN + FIXTURE
    random_stats = Stats()
    try:
        ecus = {}
        for f in FILES:
            path = os.path.join(a.xml_dir, f)
            if not os.path.exists(path):
                print(f"SKIP {f}: not found")
                continue
            ecu = EcuFile(path, True)
            ecus[f] = ecu
            model = json.loads(subprocess.run([dump, path], check=True, capture_output=True,
                                              text=True).stdout)
            k = model["k"] or {}
            kwp = k.get("kwp") or {}
            init = (kwp.get("fast_init") or kwp.get("iso8") or k.get("iso8") or {})
            mine = {
                "requests": len(model["requests"]),
                "datas": len(model["datas"]),
                "kw1": "%X" % init["kw1"] if init.get("kw1") is not None else "",
                "kw2": "%X" % init["kw2"] if init.get("kw2") is not None else "",
                "funcaddr": "%02X" % model["function"]["address"],
                "endian": model["endian"],
                "fastinit": kwp.get("fast_init") is not None,
            }
            ref = {
                "requests": len(ecu.requests),
                "datas": len(ecu.data),
                "kw1": ecu.kw1,
                "kw2": ecu.kw2,
                "funcaddr": ecu.funcaddr,
                "endian": ecu.endianness or "Big",
                "fastinit": ecu.fastinit,
            }
            status = "OK" if mine == ref else "MISMATCH"
            failures += mine != ref
            print(f"MODEL {status} {f}: ours={mine} ddt4all={ref} protocol={ecu.ecu_protocol}")

        print("\nGOLDEN (ddt4all display strings)")
        for f, req, frame in golden_frames():
            if f not in ecus:
                continue
            ref = theirs(ecus[f], req, frame)
            items = ours(dump, os.path.join(a.xml_dir, f), req, [frame])[0]["items"]
            got = {i["name"]: i["display"] for i in items}
            print(f"  {f} {req!r} frame={frame.hex().upper()}")
            for name in ref:
                ok = ref[name] == got.get(name)
                print(f"    {'OK ' if ok else 'BAD'} {name!r}: ddt4all={ref[name]!r} ours={got.get(name)!r}")
            compare_items(stats, ecus[f], req, frame, ref, items, f)

        print("\nFIXTURE (tests/fixtures/minimal_ecu.xml, frame of tests/fixture.rs)")
        fx = os.path.join("crates", "clio-spec", "tests", "fixtures", "minimal_ecu.xml")
        fecu = EcuFile(fx, True)
        frame = bytes([0x61, 0x01, 0xB6, 0x34, 0x12, 0x12, 0x34, 0xF6, 0xFE, 0xFF, 0x02,
                       0xAB, 0xCD, ord("E"), ord("7"), ord("J"), 0xFF, 0x01])
        ref = theirs(fecu, "Read Block", frame)
        items = ours(dump, fx, "Read Block", [frame])[0]["items"]
        got = {i["name"]: i["display"] for i in items}
        for name in ref:
            ok = ref[name] == got.get(name)
            print(f"    {'OK ' if ok else 'BAD'} {name!r}: ddt4all={ref[name]!r} ours={got.get(name)!r}")
        compare_items(stats, fecu, "Read Block", frame, ref, items, "fixture")
        ecus["fixture"] = fecu
        paths = {"fixture": fx}

        print("\nRANDOM")
        rng = random.Random(a.seed)
        for f, ecu in ecus.items():
            path = paths.get(f) or os.path.join(a.xml_dir, f)
            file_items = 0
            for rname, req in ecu.requests.items():
                if not req.dataitems:
                    continue
                n = max(max_reach(ecu, req), req.minbytes)
                frames = [bytes(rng.getrandbits(8) for _ in range(n)) for _ in range(a.frames)]
                frames += [bytes([0xFF] * n), bytes(n)]
                frames += [fr[: rng.randrange(0, n)] for fr in frames[:20]]  # truncated
                got = ours(dump, path, rname, frames)
                for frame, g in zip(frames, got):
                    ref = theirs(ecu, rname, frame)
                    compared += len(ref)
                    file_items += len(ref)
                    compare_items(random_stats, ecu, rname, frame, ref, g["items"], f)
            print(f"  {f}: {file_items} item decodes compared")

        print("\nLITTLE-ENDIAN setValue (ddt4all), 16-bit value 0x1234 at FirstByte 1:")
        from ddt4all.core.ecu.ecu_data import EcuData
        from ddt4all.core.ecu.data_item import DataItem
        d = EcuData({"bitscount": 16, "bytescount": 2}, "x")
        di = DataItem({"firstbyte": 1, "endian": "Little"}, "Big", "x")
        written = d.setValue("1234", ["00", "00"], di, "Big")
        read_back = d.getHexValue("".join(written), di, "Big")
        print(f"  setValue wrote {written}; getHexValue reads it back as {read_back!r}")
    finally:
        shutil.rmtree(tmp, ignore_errors=True)

    failures += stats.failures() + random_stats.failures()
    for label, st in (("golden+fixture", stats), ("random", random_stats)):
        print(f"{label}: " + ", ".join(f"{k} {st.n[k]} compared / {st.bad[k]} mismatches"
                                       for k in st.n))
    print(f"\n{compared} random item decodes compared, {failures} mismatches in total")
    sys.exit(1 if failures else 0)


if __name__ == "__main__":
    main()
