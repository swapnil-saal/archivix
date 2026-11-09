#!/usr/bin/env python3
"""Render bench.py JSON results as a Markdown comparison table."""
import json, sys
from collections import OrderedDict

rows = []
for f in sys.argv[1:]:
    rows += json.load(open(f))
by = OrderedDict()
for r in rows:
    by.setdefault(r["scenario"], []).append(r)

def fmt_t(t):
    return f"{t*1000:.0f} ms" if t < 1 else f"{t:.2f} s"

print("| Scenario | Fastest other tool | Time | archivix | Time | Speedup | Size other → archivix |")
print("|---|---|---:|---|---:|---:|---|")
for sc, rs in by.items():
    others = [r for r in rs if not r["tool"].startswith("archivix")]
    ours = [r for r in rs if r["tool"].startswith("archivix")]
    if not others or not ours:
        continue
    o = min(others, key=lambda r: r["seconds"])
    a = min(ours, key=lambda r: r["seconds"])
    sp = o["seconds"] / a["seconds"] if a["seconds"] else 0
    size = ""
    if o.get("size") and a.get("size"):
        size = f'{o["size"]/1e6:.1f} → {a["size"]/1e6:.1f} MB ({(a["size"]/o["size"]-1)*100:+.1f}%)'
    mark = "**" if sp >= 1.05 else ""
    esc = lambda t: t.replace("|", "\\|")
    print(f"| {sc} | {esc(o['tool'])} | {fmt_t(o['seconds'])} | {esc(a['tool'])} | {mark}{fmt_t(a['seconds'])}{mark} | {mark}{sp:.1f}×{mark} | {size} |")
