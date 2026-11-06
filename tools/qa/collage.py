# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
"""Presentation images: the reference next to dwg2gltf, same camera and light (used by tools/qa/qa.py).

qa.py renders the drawings of the selection file (qa.py select) and calls write_all(), which writes
into the run folder:
  collage/collage_01.png ...   grids of "reference | dwg2gltf" tiles with group and drawing name
  collage/slide_01.png ...     1920x1080 slides of the best-looking drawings
  collage/failures_01.png      the most instructive failures, one or two per category (this run's)
  shots/<group>__<stem>.png    single 1920x1080 comparisons (<stem>.png for top-level DWGs)
pick() chooses a selection for qa.py select: passing drawings, one per group first, textured and
detailed ones preferred.
"""
import csv
import math
import os
import sys

import numpy as np
from PIL import Image, ImageDraw, ImageFont

FONT_DIR = "/usr/share/fonts/noto"
BG = (243, 243, 241)
INK = (29, 29, 31)
MUTED = (110, 110, 116)
OURS = (36, 99, 235)
LINE = (220, 220, 216)
FAIL_TEXT = {
    "convert_failed": "conversion failed",
    "series_dwg": "series DWG, needs --seriesdwg",
    "empty": "nothing converted",
    "missing_parts": "parts missing",
    "extra_parts": "extra / misplaced parts",
    "shape": "geometry differs",
    "inside_out": "faces inside out",
    "tessellation": "tessellation / hidden faces",
    "texture_missing": "texture missing",
    "texture_mismatch": "textures on other parts",
    "texture_extra": "texture where the reference has none",
    "texture_scale": "texture scale differs",
    "normal_map_missing": "normal map missing",
    "color_aci7": "ACI 7: white here, black in the reference",
    "color": "colours differ",
    "axis_units": "axes or units differ",
    "offset": "placed elsewhere",
}


def log(*a):
    print(*a, file=sys.stderr, flush=True)


def font(weight, size):
    for name in (f"NotoSans-{weight}.ttf", "NotoSans-Regular.ttf"):
        p = os.path.join(FONT_DIR, name)
        if os.path.exists(p):
            return ImageFont.truetype(p, size)
    return ImageFont.load_default(size)


def fnum(v, d=0.0):
    try:
        return float(v)
    except (TypeError, ValueError):
        return d


def nice_slug(s):
    return " ".join(w.capitalize() if len(w) > 3 else w.upper() for w in s.split("-"))


def shot_name(r):
    stem = os.path.splitext(r["file"])[0]
    return f"{r['group']}__{stem}.png" if r.get("group") else f"{stem}.png"


# --- selection -------------------------------------------------------------------

def appeal(r):
    ext = sorted(fnum(x) for x in (r.get("ext_ref") or "").split())
    if len(ext) != 3 or ext[2] <= 0:
        return -1
    tris = fnum(r["tris_ours"])
    s = math.log10(max(tris, 10))
    s += 1.5 * (str(r["textured_ours"]) == "True")
    s += 0.5 * min(fnum(r["mats_ours"]), 6) / 6
    if any(k in (r.get("log_converted") or "") for k in ("3DSOLID", "BODY", "REGION", "SURFACE")):
        s += 1.0  # ACIS solids
    if ext[0] < 0.03 * ext[2]:  # flat panels
        s -= 4
    if ext[2] / max(ext[1], 1e-6) > 5:  # very long and thin
        s -= 2
    d = math.sqrt(sum(x * x for x in ext))
    if d < 0.25 or d > 7:
        s -= 3
    if tris < 1500:
        s -= 3
    return s


def pick(rows, count):
    """Diverse, appealing, passing drawings: one per group first."""
    cand = [r for r in rows if r["status"] == "pass" or (r["status"] == "warn" and fnum(r.get("score")) >= 85
                                                         and set(r["reasons"].split(",")) <= {"offset", "normal_map"})]
    for r in cand:
        r["_appeal"] = appeal(r)
    cand = [r for r in cand if r["_appeal"] > 2]
    cand.sort(key=lambda r: (-r["_appeal"], r["group"], r["file"]))
    chosen, per_group = [], {}
    for rnd in range(4):
        for r in cand:
            if len(chosen) >= count:
                break
            if r in chosen or per_group.get(r["group"], 0) > rnd:
                continue
            chosen.append(r)
            per_group[r["group"]] = per_group.get(r["group"], 0) + 1
    return chosen


def interleave(items):
    """Alternate groups so a sheet is not all from one folder."""
    buckets = {}
    for r in items:
        buckets.setdefault(r.get("group", ""), []).append(r)
    out = []
    while any(buckets.values()):
        for k in sorted(buckets, key=lambda k: (-len(buckets[k]), k)):
            if buckets[k]:
                out.append(buckets[k].pop(0))
    return out


def pair_ssim(a, b):
    """Similarity of two flattened renders (grey, 256 px)."""
    from skimage.metrics import structural_similarity

    def load(p):
        return np.asarray(Image.open(p).convert("L").resize((256, 256)), np.float64) / 255

    return float(structural_similarity(load(a), load(b), data_range=1.0))


def coverage(path):
    """Share of the image that is not background."""
    a = np.asarray(Image.open(path).convert("RGB").resize((256, 256)), np.int16)
    return float((np.abs(a - np.array(BG)).max(2) > 12).mean())


# --- images ----------------------------------------------------------------------

def flatten(png, jpg, quality=90):
    """Cycles render with transparent film -> JPEG on the sheet background. The shadow catcher's
    semi-transparent shadows fade out towards the edges instead of ending at the image border."""
    im = Image.open(png).convert("RGBA")
    a = np.asarray(im, np.float64).copy()
    h, w = a.shape[:2]
    y, x = np.mgrid[0:h, 0:w]
    edge = np.minimum(np.minimum(x, w - 1 - x), np.minimum(y, h - 1 - y)) / (0.12 * min(w, h))
    fade = np.clip(edge, 0, 1) ** 1.5
    a[..., 3] = np.where(a[..., 3] < 250, a[..., 3] * fade, a[..., 3])
    im = Image.fromarray(a.astype(np.uint8), "RGBA")
    bg = Image.new("RGBA", im.size, BG + (255,))
    bg.alpha_composite(im)
    tmp = jpg + ".tmp"
    bg.convert("RGB").save(tmp, "JPEG", quality=quality)
    os.replace(tmp, jpg)


def load_img(path, size):
    if path and os.path.exists(path):
        return Image.open(path).convert("RGB").resize((size, size), Image.LANCZOS)
    im = Image.new("RGB", (size, size), BG)
    ImageDraw.Draw(im).text((20, size // 2), "no render", font=font("Regular", max(14, size // 20)), fill=MUTED)
    return im


def ellipsize(draw, text, f, width):
    if draw.textlength(text, font=f) <= width:
        return text
    while text and draw.textlength(text + "…", font=f) > width:
        text = text[:-1]
    return text.rstrip(" -,/") + "…"


def tile(r, S, label_h, caption=None, note=None):
    gap = 6
    W = 2 * S + gap
    im = Image.new("RGB", (W, S + label_h), (255, 255, 255))
    d = ImageDraw.Draw(im)
    small = font("Medium", max(13, S // 34))
    for k, side in enumerate(("ref", "ours")):
        x = k * (S + gap)
        im.paste(load_img(r.get(f"_{side}_img"), S), (x, 0))
        d.text((x + 14, 12), "reference" if side == "ref" else "dwg2gltf", font=small,
               fill=MUTED if side == "ref" else OURS)
    f1, f2 = font("SemiBold", max(16, S // 24)), font("Regular", max(14, S // 29))
    y = S + 10
    d.text((14, y), ellipsize(d, caption or r["_group"] or r["name"], f1, W - 28), font=f1, fill=INK)
    d.text((14, y + f1.size + 6), ellipsize(d, note or r["name"] or r["file"], f2, W - 28), font=f2, fill=MUTED)
    return im


def sheet(items, path, title, subtitle, footer, cols, S, captions=None, notes=None):
    label_h = 18 + int(S / 24 * 1.25) + int(S / 29 * 1.25) + 14
    tw, th = 2 * S + 6, S + label_h
    gx, gy, M, head, foot = 28, 28, 56, 150, 70
    rows = math.ceil(len(items) / cols)
    W = M * 2 + cols * tw + (cols - 1) * gx
    H = head + rows * th + (rows - 1) * gy + foot + M // 2
    im = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(im)
    d.text((M, 44), title, font=font("Bold", 46), fill=INK)
    d.text((M, 104), subtitle, font=font("Regular", 22), fill=MUTED)
    for k, r in enumerate(items):
        x = M + (k % cols) * (tw + gx)
        y = head + (k // cols) * (th + gy)
        im.paste(tile(r, S, label_h, captions[k] if captions else None, notes[k] if notes else None), (x, y))
        d.rectangle((x - 1, y - 1, x + tw, y + th), outline=LINE)
    d.text((M, H - foot), footer, font=font("Regular", 18), fill=MUTED)
    im.save(path, optimize=True)


def slide(r, path, footer):
    W, H, S, top = 1920, 1080, 860, 150
    im = Image.new("RGB", (W, H), BG)
    d = ImageDraw.Draw(im)
    d.text((80, 46), r["_group"], font=font("SemiBold", 30), fill=MUTED)
    f = font("Bold", 44)
    d.text((80, 88), ellipsize(d, r["name"] or r["file"], f, W - 160), font=f, fill=INK)
    for k, side in enumerate(("ref", "ours")):
        x = 80 + k * (S + 40)
        im.paste(load_img(r.get(f"_{side}_img"), S), (x, top))
        d.text((x + 22, top + 20), "reference" if side == "ref" else "dwg2gltf", font=font("Medium", 24),
               fill=MUTED if side == "ref" else OURS)
    d.text((80, H - 62), footer, font=font("Regular", 20), fill=MUTED)
    im.save(path, optimize=True)


def metrics_line(r):
    if r.get("status") not in ("pass", "warn", "fail") or r.get("chamfer") in ("", None):
        return f"status: {r.get('status', '')} {r.get('category', '')}"
    ch = fnum(r["chamfer"]) * 100
    t1, t2 = int(fnum(r["tris_ours"])), int(fnum(r["tris_ref"]))
    return (f"{r['status']}  ·  mean surface distance {ch:.3f} % of the size  ·  triangles {t1:,} / {t2:,}  ·  "
            f"materials {r['mats_ours']} / {r['mats_ref']}  ·  "
            f"{'textured' if str(r['textured_ours']) == 'True' else 'plain colours'}")


def why(r):
    bits = [FAIL_TEXT.get(r.get("category", ""), r.get("category", ""))]
    if fnum(r.get("missing")) > 0.01:
        bits.append(f"{fnum(r['missing']) * 100:.0f} % of the reference surface missing")
    if fnum(r.get("extra")) > 0.01:
        bits.append(f"{fnum(r['extra']) * 100:.0f} % extra surface")
    if fnum(r.get("inside_out")) > 0.01:
        bits.append(f"{fnum(r['inside_out']) * 100:.0f} % inside out")
    if r.get("category", "").startswith("texture") and r.get("category") != "texture_scale":
        bits.append(f"textured: reference {'yes' if str(r['textured_ref']) == 'True' else 'no'}, "
                    f"ours {'yes' if str(r['textured_ours']) == 'True' else 'no'}")
    if r.get("category") == "texture_scale" and fnum(r.get("tex_scale_ratio")):
        q = fnum(r["tex_scale_ratio"])
        k = q if q >= 1 else 1 / q
        ks = f"{k:.0f}" if k >= 10 else f"{k:.2g}"
        bits.append(f"ours repeats {ks}x as often" if q >= 1 else f"ours repeats {ks}x less often")
    if r.get("category") == "axis_units":
        bits.append(f"ours to reference: {r['transform']}")
    if r.get("category") == "tessellation":
        bits.append(f"area {fnum(r['area_ours']):.2f} vs {fnum(r['area_ref']):.2f} m²")
    return "  ·  ".join(bits)


def failures(rows, n):
    """One or two of each category, worst first, from different groups where possible."""
    bad = [r for r in rows if r["status"] in ("warn", "fail") and r.get("category") in FAIL_TEXT]
    bad.sort(key=lambda r: (fnum(r.get("score")), r["group"], r["file"]))
    per_cat, out, seen = {}, [], set()
    for lim in (1, 2, 3):
        for r in bad:
            c = r["category"]
            if len(out) >= n or r in out or per_cat.get(c, 0) >= lim or (lim == 1 and r["group"] in seen):
                continue
            out.append(r)
            per_cat[c] = per_cat.get(c, 0) + 1
            seen.add(r["group"])
    return out


def write_all(run_dir, collage_items, slides, shots, fails, info, per_sheet=28, cols=4):
    """Compose every presentation image of a run. Items carry _ref_img/_ours_img (flattened JPEGs),
    _group and the results columns. Returns the written paths."""
    cdir, sdir = os.path.join(run_dir, "collage"), os.path.join(run_dir, "shots")
    os.makedirs(cdir, exist_ok=True)
    os.makedirs(sdir, exist_ok=True)
    written = []
    stamp = f"dwg2gltf {info['converter']}  ·  {info['date']}"
    n = math.ceil(len(collage_items) / per_sheet)
    ngroups = len({r["group"] for r in collage_items})
    for s in range(n):
        p = os.path.join(cdir, f"collage_{s + 1:02d}.png")
        sheet(collage_items[s * per_sheet:(s + 1) * per_sheet], p, "Reference vs dwg2gltf",
              "Left: the reference GLB.  Right: dwg2gltf, converted from the same DWG.  "
              "Same camera, light and framing.",
              f"Sheet {s + 1} of {n}  ·  {len(collage_items)} drawings from {ngroups} groups  ·  "
              f"Blender Cycles, front 3/4 view  ·  {stamp}", cols, 512)
        written.append(p)
    for k, r in enumerate(slides):
        p = os.path.join(cdir, f"slide_{k + 1:02d}.png")
        slide(r, p, metrics_line(r))
        written.append(p)
    for r in shots:
        p = os.path.join(sdir, shot_name(r))
        slide(r, p, metrics_line(r))
        written.append(p)
    if fails:
        fails = sorted(fails, key=lambda r: (r["category"], fnum(r.get("score"))))
        for start in range(0, len(fails), per_sheet):
            part = fails[start:start + per_sheet]
            p = os.path.join(cdir, f"failures_{start // per_sheet + 1:02d}.png")
            sheet(part, p, "Where dwg2gltf differs from the reference",
                  "The most instructive warn/fail cases of this run, one or two per failure category.  "
                  "Left: reference.  Right: dwg2gltf.",
                  f"{info['compared']} drawings compared  ·  {stamp}", cols, 512,
                  captions=[f"{r['_group']}  ·  {r['name'] or r['file']}" if r["_group"] else (r["name"] or r["file"])
                            for r in part], notes=[why(r) for r in part])
            written.append(p)
    with open(os.path.join(cdir, "selection.csv"), "w", newline="", encoding="utf-8") as f:
        w = csv.writer(f)
        w.writerow(["use", "status", "score", "category", "group", "file"])
        for use, items in (("collage", collage_items), ("slide", slides), ("shot", shots), ("failures", fails)):
            for r in items:
                w.writerow([use, r["status"], r.get("score", ""), r.get("category", ""), r["group"], r["file"]])
    return written
