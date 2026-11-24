#!/bin/bash
# Every makinax item the makinax-mcp server uses must be exercised by a makinax-level test:
# the item's name must appear in crates/makinax/tests/*.rs or inside a #[cfg(test)] module in crates/makinax/src.
#
# Items are read from every `makinax::…` path in crates/makinax-mcp/src and crates/makinax-mcp/tests,
# with `use` groups expanded
# (nested too: `makinax::ext::{a, sim::{b, c}}` names a, b and c). Comments and string literals
# (plain, byte and raw) are stripped first: a `makinax::…` path in text (e.g. a JSON-schema id)
# names no item.
set -euo pipefail
cd "$(dirname "$0")/.."
used=$(python3 - <<'PY'
import os, re

SKIP = set()

def strip(src):
    out, i, n = [], 0, len(src)
    while i < n:
        c = src[i]
        if src.startswith("//", i):
            while i < n and src[i] != "\n":
                i += 1
            continue
        if src.startswith("/*", i):
            depth = 0
            while i < n:
                if src.startswith("/*", i):
                    depth += 1; i += 2
                elif src.startswith("*/", i):
                    depth -= 1; i += 2
                    if depth == 0:
                        break
                else:
                    i += 1
            continue
        m = re.match(r'b?r(#*)"', src[i:])
        if m and (i == 0 or not (src[i - 1].isalnum() or src[i - 1] == "_")):
            end = src.find('"' + m.group(1), i + m.end())
            i = n if end < 0 else end + 1 + len(m.group(1))
            out.append(" ")
            continue
        if c == '"' or (c == "b" and src.startswith('b"', i) and not (i and (src[i-1].isalnum() or src[i-1] == "_"))):
            i += 2 if c == "b" else 1
            while i < n and src[i] != '"':
                i += 2 if src[i] == "\\" else 1
            i += 1
            out.append(" ")
            continue
        if c == "'":
            m = re.match(r"'(\\.[^']*|[^\\'])'", src[i:])
            if m:
                i += m.end(); out.append(" "); continue
        out.append(c); i += 1
    return "".join(out)

TOK = re.compile(r"\s*(::|[A-Za-z_][A-Za-z0-9_]*|\{|\}|,|\*|.)", re.S)

def tokens(s):
    return [m.group(1) for m in TOK.finditer(s)]

def tree(t, i, leaves):
    """Parse a path tail at t[i] (after `::`); add every leaf's last segment."""
    last = None
    while i < len(t):
        w = t[i]
        if w == "{":
            i += 1
            while i < len(t) and t[i] != "}":
                if t[i] == ",":
                    i += 1; continue
                before = i
                i = tree(t, i, leaves)
                while i < len(t) and t[i] not in (",", "}"):
                    i += 1  # `as alias`
                if i == before:
                    i += 1
            return i + 1
        if w == "*":
            return i + 1
        if re.match(r"[A-Za-z_]", w):
            last = w; i += 1
            if i < len(t) and t[i] == "::":
                i += 1; continue
            break
        break
    if last and last != "self":
        leaves.add(last)
    return i

leaves = set()
for root in ("crates/makinax-mcp/src", "crates/makinax-mcp/tests"):
    if not os.path.isdir(root):
        raise SystemExit(f"item source {root} does not exist")
    for d, _, fs in os.walk(root):
        if "node_modules" in d:
            continue
        for f in fs:
            p = os.path.join(d, f)
            if not f.endswith(".rs") or p in SKIP:
                continue
            t = tokens(strip(open(p, encoding="utf-8").read()))
            for k, w in enumerate(t):
                if w == "makinax" and k + 1 < len(t) and t[k + 1] == "::" and not (k and t[k - 1] == "::" and k > 1 and re.match(r"[A-Za-z_]", t[k - 2])):
                    tree(t, k + 2, leaves)
print("\n".join(sorted(leaves)))
PY
)
testtext=$(cat crates/makinax/tests/*.rs; for f in $(grep -rl '#\[cfg(test)\]' crates/makinax/src); do sed -n '/#\[cfg(test)\]/,$p' "$f"; done)
missing=0
count=0
for item in $used; do
  count=$((count + 1))
  grep -qw -- "$item" <<<"$testtext" || { echo "UNTESTED: $item"; missing=1; }
done
echo "checked $count items"
# A check that found nothing proves nothing: an empty or moved item source
# would otherwise print COVERAGE OK. The server uses 54 items today.
MIN_ITEMS=40
if [ "$count" -lt "$MIN_ITEMS" ]; then
  echo "COVERAGE FAILED: only $count makinax items found (expected at least $MIN_ITEMS); is the item source right?"
  exit 1
fi
[ $missing = 0 ] && echo "COVERAGE OK"
exit $missing
