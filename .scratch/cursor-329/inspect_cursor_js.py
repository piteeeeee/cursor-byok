#!/usr/bin/env python3
"""Read-only inspection of Cursor.app JS bundles for BidiAppend / dataBinary / BidiPoll / RunSSE."""
import os
import re

BUNDLES = [
    "/Applications/Cursor.app/Contents/Resources/app/out/vs/workbench/workbench.desktop.main.js",
    "/Applications/Cursor.app/Contents/Resources/app/out/vs/workbench/api/node/extensionHostProcess.js",
    "/Applications/Cursor.app/Contents/Resources/app/extensions/cursor-always-local/dist/main.js",
]

PATTERNS = [
    r"dataBinary",
    r"data_binary",
    r"BidiAppendRequest",
    r"bidiAppend",
    r"BidiPoll",
    r"bidiPoll",
    r"RunSSE",
    r"runSSE",
]

for path in BUNDLES:
    if not os.path.exists(path):
        print(f"[MISSING] {path}")
        continue
    with open(path, "r", encoding="utf-8", errors="replace") as f:
        text = f.read()
    print(f"\n==================== {os.path.basename(path)} ({len(text)} chars) ====================")
    for pat in PATTERNS:
        matches = list(re.finditer(pat, text))
        print(f"  Pattern '{pat}': {len(matches)} matches")
        for idx, m in enumerate(matches[:8]):
            start = max(0, m.start() - 220)
            end = min(len(text), m.end() + 260)
            snippet = text[start:end].replace("\n", "\\n")
            print(f"    [{idx+1} @ {m.start()}] ...{snippet}...")
