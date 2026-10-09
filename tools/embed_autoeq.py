#!/usr/bin/env python3
"""Makes curve blocks for the macOS app's EmbeddedCurves.swift from the AutoEq files in data/autoeq/.

The steps are the ones of docs/third-party-autoeq.md: read `frequency` and `raw`, take the mean of
the source points in each bin of the shared 200-point grid, fill an empty bin by a straight line in
dB over log frequency, write two decimals. The Rust file embedded_curves.rs is then made from the
Swift file with tools/embed_curves.py, so both apps hold the same numbers.

    python3 tools/embed_autoeq.py --swift                            # the Swift blocks
    python3 tools/embed_autoeq.py --check FILE.csv "Embedded name"   # compare with embedded_curves.rs
"""
import csv
import math
import os
import re
import sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
EMBEDDED = os.path.join(ROOT, "crates/joseon-headphones/src/embedded_curves.rs")

# (constant, file in data/autoeq, name in the app, source text, doc line)
CURVES = [
    ("BEYERDYNAMIC_DT900_PRO_X", "Beyerdynamic DT 900 Pro X (oratory1990).csv", "Beyerdynamic DT 900 Pro X",
     "oratory1990 (AutoEq results, over-ear)", "Beyerdynamic DT 900 Pro X, open dynamic studio headphone."),
    ("AUDIO64_U12T_M15", "64 Audio U12t m15 (HypetheSonics).csv", "64 Audio U12t (m15 module)",
     "HypetheSonics (GRAS RA0045 rig, in-ear)", "64 Audio U12t in-ear monitor with the m15 Apex module."),
    ("AUDIO64_U12T_M20", "64 Audio U12t m20 (HypetheSonics).csv", "64 Audio U12t (m20 module)",
     "HypetheSonics (GRAS RA0045 rig, in-ear)", "64 Audio U12t in-ear monitor with the m20 Apex module."),
    ("AUDIO64_U12T_MX", "64 Audio U12t mX (HypetheSonics).csv", "64 Audio U12t (mX module)",
     "HypetheSonics (GRAS RA0045 rig, in-ear)", "64 Audio U12t in-ear monitor with the mX Apex module."),
    ("XENNS_MANGIRD_TEA_PRO", "Xenns Mangird Tea Pro 2024 (Jaytiss).csv", "Xenns Mangird Tea Pro",
     "Jaytiss (AutoEq results, in-ear)", "Xenns Mangird Tea Pro (2024) in-ear monitor."),
]


def grid():
    text = open(EMBEDDED, encoding="utf-8").read()
    block = re.search(r"STANDARD_FREQUENCIES_HZ: \[f32; POINT_COUNT\] = \[(.*?)\];", text, re.S).group(1)
    values = [float(n) for n in re.split(r"[\s,]+", block.strip()) if n]
    assert len(values) == 200, len(values)
    return values


def embedded(name):
    text = open(EMBEDDED, encoding="utf-8").read()
    block = re.search(r'name: "' + re.escape(name) + r'",\s*source: "[^"]*",\s*levels_db: &\[(.*?)\]', text, re.S)
    return [float(n) for n in re.split(r"[\s,]+", block.group(1).strip()) if n]


def read(path):
    points = []
    with open(path, encoding="utf-8", newline="") as file:
        for row in csv.DictReader(file):
            try:
                f, level = float(row["frequency"]), float(row["raw"])
            except (KeyError, TypeError, ValueError):
                continue
            if math.isfinite(f) and math.isfinite(level) and f > 0:
                points.append((f, level))
    points.sort()
    return points


def decimate(points, frequencies):
    """The mean of the source points in each bin. The edges of a bin are the geometric middles to its neighbors."""
    n = len(frequencies)
    edges = [math.sqrt(frequencies[i] * frequencies[i + 1]) for i in range(n - 1)]
    sums, counts = [0.0] * n, [0] * n
    for f, level in points:
        i = 0
        while i < n - 1 and f >= edges[i]:
            i += 1
        sums[i] += level
        counts[i] += 1
    out = [sums[i] / counts[i] if counts[i] else None for i in range(n)]
    known = [i for i in range(n) if out[i] is not None]
    for i in range(n):
        if out[i] is not None:
            continue
        below = max((k for k in known if k < i), default=None)
        above = min((k for k in known if k > i), default=None)
        if below is None or above is None:
            out[i] = out[above if below is None else below]
        else:
            t = math.log(frequencies[i] / frequencies[below]) / math.log(frequencies[above] / frequencies[below])
            out[i] = out[below] + t * (out[above] - out[below])
    return out


SWIFT_NAMES = ["dt900ProX", "u12tM15", "u12tM20", "u12tMX", "teaPro"]
PATHS = [
    "results/oratory1990/over-ear/Beyerdynamic DT 900 Pro X/Beyerdynamic DT 900 Pro X.csv",
    "results/HypetheSonics/GRAS RA0045 in-ear/64 Audio U12t (m15 Apex module)/64 Audio U12t (m15 Apex module).csv",
    "results/HypetheSonics/GRAS RA0045 in-ear/64 Audio U12t (m20 Apex module)/64 Audio U12t (m20 Apex module).csv",
    "results/HypetheSonics/GRAS RA0045 in-ear/64 Audio U12t (mX Apex module)/64 Audio U12t (mX Apex module).csv",
    "results/Jaytiss/in-ear/Xenns Mangird Tea Pro 2024/Xenns Mangird Tea Pro 2024.csv",
]
SWIFT = [(n, c[1], c[2], c[3], c[4], p) for n, c, p in zip(SWIFT_NAMES, CURVES, PATHS)]


def swift_number(v):
    """Two decimals, as the first set writes them: "0" for zero."""
    text = f"{v:.2f}"
    return "0" if text in ("0.00", "-0.00") else text


def swift_rows(values, indent="            "):
    lines, line = [], indent
    for i, v in enumerate(values):
        piece = swift_number(v) + ("" if i == len(values) - 1 else ", ")
        if len((line + piece).rstrip()) > 110:
            lines.append(line.rstrip())
            line = indent
        line += piece
    lines.append(line.rstrip())
    return "\n".join(lines)


def main():
    frequencies = grid()
    if len(sys.argv) == 4 and sys.argv[1] == "--check":
        ours = decimate(read(sys.argv[2]), frequencies)
        theirs = embedded(sys.argv[3])
        worst = max(abs(a - b) for a, b in zip(ours, theirs))
        print(f"{sys.argv[3]}: largest difference {worst:.3f} dB over {len(theirs)} points")
        return 0 if worst <= 0.011 else 1
    if len(sys.argv) == 2 and sys.argv[1] == "--swift":
        # The same curves as blocks for EmbeddedCurves.swift of the macOS app.
        for swift, file, name, source, doc, path in SWIFT:
            levels = decimate(read(os.path.join(ROOT, "data/autoeq", file)), frequencies)
            print(f"    /// {name} \u2014 {source}")
            print(f"    /// AutoEq: `{path}` (column `raw`)")
            print(f"    public static let {swift} = HeadphoneCurve(")
            print(f'        name: "{name}",')
            print(f'        source: "{source}",')
            print("        frequenciesHz: standardFrequenciesHz,")
            print("        levelsDB: [")
            print(swift_rows(levels))
            print("        ]")
            print("    )")
            print()
        return 0
    print(__doc__)
    return 2

if __name__ == "__main__":
    sys.exit(main())
