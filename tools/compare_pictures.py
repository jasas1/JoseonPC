#!/usr/bin/env python3
"""Compares the review pictures of the Rust panels with the macOS ones.

Usage: python3 tools/compare_pictures.py [directory]   (default: target/joseon-render-review)
A pair is `NAME.png` and `mac-NAME.png`. Prints the mean difference per channel
and the share of pixels that differ by more than 24 of 255. No dependency.
"""
import os, struct, sys, zlib


def load(path):
    d = open(path, "rb").read()
    i, idat = 8, b""
    while i < len(d):
        (n,) = struct.unpack(">I", d[i:i + 4])
        t, c = d[i + 4:i + 8], d[i + 8:i + 8 + n]
        i += 12 + n
        if t == b"IHDR":
            w, h, _, ct = struct.unpack(">IIBB", c[:10])
        if t == b"IDAT":
            idat += c
    raw = zlib.decompress(idat)
    bpp = {2: 3, 6: 4}[ct]
    stride, rows, prev, pos = w * bpp, [], bytearray(w * bpp), 0
    for _ in range(h):
        f, line = raw[pos], bytearray(raw[pos + 1:pos + 1 + stride])
        pos += 1 + stride
        for x in range(stride):
            a = line[x - bpp] if x >= bpp else 0
            b = prev[x]
            c = prev[x - bpp] if x >= bpp else 0
            if f == 1:
                line[x] = (line[x] + a) & 255
            elif f == 2:
                line[x] = (line[x] + b) & 255
            elif f == 3:
                line[x] = (line[x] + (a + b) // 2) & 255
            elif f == 4:
                pa, pb, pc = abs(b - c), abs(a - c), abs(a + b - 2 * c)
                line[x] = (line[x] + (a if pa <= pb and pa <= pc else (b if pb <= pc else c))) & 255
        rows.append(line)
        prev = line
    return w, h, bpp, rows


directory = sys.argv[1] if len(sys.argv) > 1 else "target/joseon-render-review"
for name in sorted(os.listdir(directory)):
    if not name.startswith("mac-"):
        continue
    ours = os.path.join(directory, name[4:])
    if not os.path.exists(ours):
        continue
    w, h, b1, a = load(ours)
    w2, h2, b2, m = load(os.path.join(directory, name))
    if (w, h) != (w2, h2):
        print(f"{name[4:-4]}: sizes differ, {w}x{h} and {w2}x{h2}")
        continue
    total = big = count = 0
    for y in range(0, h, 2):
        for x in range(0, w, 2):
            d = max(abs(a[y][x * b1 + c] - m[y][x * b2 + c]) for c in range(3))
            total += d
            count += 1
            big += d > 24
    print(f"{name[4:-4]}: mean difference {total / count:.2f} of 255, pixels over 24: {100 * big / count:.2f} %")
