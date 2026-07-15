#!/usr/bin/env python3
"""Regenerate the README screenshots from the running tui-pdf binary.

Each shot drives the real binary headless through the ghostty-term-use harness
(`gtu`), snapshots the *exact* screen to SVG, and renders it to PNG with
rsvg-convert. No mock-ups, no per-feature hacks — one export primitive.

Requirements:
  - gtu:   build ../ghostty-term-use (zig build -Doptimize=ReleaseFast) or set GTU_BIN
  - tui-pdf: cargo build
  - rsvg-convert (librsvg)

Env:
  GTU_BIN   path to the gtu binary (default: ../../../ghostty-term-use/zig-out/bin/gtu)
  TUI_BIN   path to tui-pdf     (default: ../../target/debug/tui-pdf)
  SESSION   tui-pdf session to open (default: isogeny_based); or set DOC to a PDF path
"""
import json, os, subprocess, sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
GTU = os.environ.get("GTU_BIN", os.path.join(ROOT, "..", "ghostty-term-use", "zig-out", "bin", "gtu"))
TUI = os.environ.get("TUI_BIN", os.path.join(ROOT, "target", "debug", "tui-pdf"))
SESSION = os.environ.get("SESSION", "isogeny_based")
DOC = os.environ.get("DOC")  # if set, open this file instead of a session
ENV = ["TERM=xterm-kitty", "COLORTERM=truecolor",
       f"HOME={os.environ.get('HOME','')}", f"PATH={os.environ.get('PATH','')}"]
CMD = [TUI, DOC] if DOC else [TUI, "--session", SESSION]
WAIT = [{"op": "wait", "idle_ms": 1500, "budget_ms": 30000}]

def shot(name, extra):
    svg = os.path.join(HERE, name + ".svg")
    steps = WAIT + extra + [{"op": "snap", "name": name, "svg_path": svg}]
    sc = {"cmd": CMD, "cols": 120, "rows": 40, "cell_w": 10, "cell_h": 20, "env": ENV, "steps": steps}
    out = subprocess.run([GTU], input=json.dumps(sc).encode(), capture_output=True, timeout=120)
    d = json.loads(out.stdout)
    assert d.get("ok"), d
    subprocess.run(["rsvg-convert", svg, "-o", os.path.join(HERE, name + ".png")], check=True)
    os.remove(svg)
    print("wrote", name + ".png")

shot("pdf_view", [])
shot("file_browser", [{"op": "send_keys", "keys": ["e"]}, {"op": "wait", "idle_ms": 600, "budget_ms": 8000}])
shot("doc_picker", [{"op": "send_keys", "keys": ["d"]}, {"op": "wait", "idle_ms": 500, "budget_ms": 6000}])
shot("toc", [{"op": "send_keys", "keys": ["t"]}, {"op": "wait", "idle_ms": 600, "budget_ms": 8000}])
shot("search", [{"op": "send_keys", "keys": ["/"]}, {"op": "send_text", "text": "field"},
                {"op": "send_keys", "keys": ["Enter"]}, {"op": "wait", "idle_ms": 1500, "budget_ms": 15000}])
print("done")
