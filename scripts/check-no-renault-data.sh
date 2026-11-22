#!/usr/bin/env bash
# Fails if a tracked file looks like a Renault DDT2000 ECU definition. Those files are Renault's
# and are never redistributed: users supply their own (see README.md).
#
# Flags a tracked file when
#   - it lives under ddt-xml/, or
#   - it is an .xml/.xml.gz/.zip whose name matches a known Renault definition name, or
#   - it is an .xml larger than KB_LIMIT (default 8) whose root (first tags) is <ECU>/<Ecu>/<Target>, also
#     as <ddt:Target>, and that contains both Request(s) and Data(s) elements.
# Hand-written fixtures in crates/*/tests/fixtures are far below the size limit.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"
KB_LIMIT="${KB_LIMIT:-8}"

git ls-files -z | KB_LIMIT="$KB_LIMIT" "${PYTHON:-python3}" -I -c '
import os, re, sys
limit = int(os.environ["KB_LIMIT"]) * 1024
names = re.compile(r"^(S2000|BMT_X65|SBMT).*\.(xml|xml\.gz|zip)$", re.I)
bad = []
for p in sys.stdin.buffer.read().decode().split("\0"):
    if not p or not os.path.isfile(p):
        continue
    base = os.path.basename(p)
    if p.startswith("ddt-xml/"):
        bad.append((p, "under ddt-xml/")); continue
    if names.match(base):
        bad.append((p, "known Renault definition file name")); continue
    if p.lower().endswith(".xml") and os.path.getsize(p) > limit:
        data = open(p, "rb").read().decode("latin-1")
        data = re.sub(r"<\?xml[^>]*\?>|<!--.*?-->", "", data, flags=re.S)
        # Real files are <xml><ddt:Target ...> (DDT2000) or <ECU>/<Target> roots; look at the first tags.
        head = re.findall(r"<(?:[\w-]+:)?([A-Za-z_][\w.-]*)", data[:4096])[:3]
        if any(t.lower() in ("ecu", "target") for t in head) and re.search(r"<(\w+:)?Requests?\b", data) and re.search(r"<(\w+:)?Datas?\b", data):
            bad.append((p, "ECU/Target root with Requests and Datas, %d KB" % (os.path.getsize(p) // 1024)))
for p, why in bad:
    print("FAIL: %s: %s" % (p, why), file=sys.stderr)
if bad:
    print("Renault ECU data must never be committed (git rm --cached it).", file=sys.stderr)
    sys.exit(1)
print("ok: no Renault ECU data tracked")
'
