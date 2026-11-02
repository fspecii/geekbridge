#!/usr/bin/env python3 -I
# SPDX-License-Identifier: GPL-3.0-or-later
"""Golden-image helper (Pillow). Run as `python3 -I scripts/golden/golden_img.py ...`.

  reduce SRC.png DST.png [--crop X,Y,W,H]   scale to 50 %, reduce to a 256-colour palette, write a small PNG
  compare ACTUAL.png GOLDEN.png DIFF.png    (GOLDEN_CROP=X,Y,W,H crops ACTUAL first) exit 0 if equal within tolerance, 1 otherwise (writes DIFF.png)

Tolerance: a pixel differs when any RGB channel differs by more than PIXEL_THRESHOLD (default 24, out of 255);
the images match while differing pixels are at most MAX_RATIO (default 0.0005 = 0.05 %) of all pixels.
A size mismatch always fails. ACTUAL is reduced in memory the same way as `reduce`, so pass a raw capture.
"""
import os
import sys

from PIL import Image, ImageChops

PIXEL_THRESHOLD = int(os.environ.get("GOLDEN_PIXEL_THRESHOLD", "24"))
MAX_RATIO = float(os.environ.get("GOLDEN_MAX_RATIO", "0.0005"))


def env_crop():
    v = os.environ.get("GOLDEN_CROP")
    return tuple(int(x) for x in v.split(",")) if v else None


def load_reduced(path, crop=None):
    crop = crop or env_crop()
    im = Image.open(path).convert("RGB")
    if crop:
        x, y, w, h = crop
        im = im.crop((x, y, x + w, y + h))
    return im.resize((max(1, im.width // 2), max(1, im.height // 2)), Image.BOX)


def cmd_reduce(src, dst, crop=None):
    im = load_reduced(src, crop)
    pal = im.quantize(colors=256, method=Image.Quantize.MEDIANCUT, dither=Image.Dither.NONE)
    pal.save(dst, optimize=True)


def cmd_compare(actual, golden, diff_out):
    a = load_reduced(actual) if os.environ.get("GOLDEN_RAW", "1") == "1" else Image.open(actual).convert("RGB")
    gp = Image.open(golden)
    g = gp.convert("RGB")
    if gp.mode == "P" and a.size == g.size:
        # Map the capture onto the golden's palette so palette-reduction error cancels out; real changes still differ.
        a = a.quantize(palette=gp, dither=Image.Dither.NONE).convert("RGB")
    if a.size != g.size:
        print(f"size mismatch: actual {a.size} golden {g.size}")
        write_diff(a, g, None, diff_out)
        return 1
    d = ImageChops.difference(a, g)
    mask = d.convert("RGB").point(lambda v: 255 if v > PIXEL_THRESHOLD else 0)
    mask = ImageChops.lighter(ImageChops.lighter(*mask.split()[:2]), mask.split()[2])
    bad = mask.histogram()[255]
    total = a.width * a.height
    ratio = bad / total
    print(f"{bad} of {total} pixels differ ({ratio:.4%}; allowed {MAX_RATIO:.4%}, per-pixel threshold {PIXEL_THRESHOLD})")
    if ratio <= MAX_RATIO:
        return 0
    write_diff(a, g, mask, diff_out)
    return 1


def write_diff(a, g, mask, out):
    """expected | actual | highlighted, side by side."""
    w, h = g.width, g.height
    canvas = Image.new("RGB", (w * 3 + 8, max(h, a.height)), (128, 128, 128))
    canvas.paste(g, (0, 0))
    canvas.paste(a, (w + 4, 0))
    if mask is not None:
        hi = g.copy().point(lambda v: v // 3 + 140)
        hi.paste(Image.new("RGB", g.size, (255, 0, 60)), (0, 0), mask)
        canvas.paste(hi, (w * 2 + 8, 0))
    os.makedirs(os.path.dirname(os.path.abspath(out)), exist_ok=True)
    canvas.save(out)


if __name__ == "__main__":
    args = sys.argv[1:]
    if len(args) >= 3 and args[0] == "reduce":
        crop = None
        if "--crop" in args:
            crop = tuple(int(v) for v in args[args.index("--crop") + 1].split(","))
        cmd_reduce(args[1], args[2], crop)
    elif len(args) == 4 and args[0] == "compare":
        sys.exit(cmd_compare(*args[1:]))
    else:
        print(__doc__)
        sys.exit(2)
