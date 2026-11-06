# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
"""Engineering report of a comparison run (used by tools/qa/qa.py).

select() picks the rows to render: every warn/fail file, worst first (up to max_bad; the worst
`four` of them from four directions) and a passing sample, one per group. build() adds an
SSIM difference image per view (cached by the two render files), copies thumbnails into
<run>/report/img/ and writes <run>/report/index.html: summary by group, failure categories with
examples, thresholds, extra sections, then one row per file with reference | ours | difference and
the metrics.

Extra sections: build(..., extra_sections=[(title, html), ...]) writes them into index.html, and
add_section(index_html, title, html) adds or replaces one in a report written before (for a review
that runs after the report). group_names ({group: display name}) labels the groups.
"""
import hashlib
import html
import json
import os
import random
import re
import shutil
import sys
import time

import numpy as np
from PIL import Image

HERE = os.path.dirname(os.path.abspath(__file__))
FOUR = ["front34", "back34", "side", "top"]
CATEGORY_TEXT = {
    "convert_failed": "dwg2gltf exited with an error or timed out",
    "series_dwg": "series DWG (block library): plain conversion is empty, needs --seriesdwg",
    "empty": "no triangles in our output",
    "both_empty": "nothing to convert, and the reference is empty too",
    "axis_units": "constant axis or unit transform between ours and the reference",
    "missing_parts": "reference surface with nothing of ours nearby (> 1 % of the diagonal)",
    "extra_parts": "our surface with nothing of the reference nearby",
    "shape": "surfaces apart (Chamfer / Hausdorff / bbox) without whole parts missing",
    "inside_out": "our normals point against the reference's on coincident surface",
    "tessellation": "same shape, surface area differs (tessellation, duplicate or hidden faces)",
    "texture_missing": "reference textured, ours plain colour",
    "texture_extra": "ours textured, reference plain colour",
    "texture_mismatch": "textured on different parts",
    "normal_map_missing": "the reference has a normal (bump) map, ours has none",
    "texture_scale": "texture repeats per metre differ (UV scale) where both are textured",
    "color_aci7": "ACI 7 drawn white by us, black in the reference",
    "color": "base colours differ on coincident surface",
    "offset": "same shape, placed elsewhere (bbox centre offset)",
    "compare_error": "the comparison itself failed",
}


def fnum(v, d=None):
    try:
        return float(v)
    except (TypeError, ValueError):
        return d


def select(rows, max_bad=400, four=40, showcase=30, seed=7):
    """Rows to render in the report, each with r["_views"] (view names)."""
    bad = [r for r in rows if r["status"] in ("warn", "fail", "error")]
    bad.sort(key=lambda r: (fnum(r.get("score"), -1), r["group"], r["file"]))
    bad = bad[:max_bad]
    good = sorted((r for r in rows if r["status"] == "pass"), key=lambda r: (r["group"], r["file"]))
    random.Random(seed).shuffle(good)
    seen, show = set(), []
    for r in good:
        if r["group"] not in seen and len(show) < showcase:
            seen.add(r["group"])
            show.append(r)
    for k, r in enumerate(bad):
        r["_views"] = FOUR if k < four else ["front34"]
        if r.get("category") in ("convert_failed", "series_dwg", "empty", "compare_error", "ref_empty", "both_empty") or not r.get("bbox_ref"):
            r["_views"] = []
    for r in show:
        r["_views"] = ["front34"]
    return bad, show


def diff_image(ref, ours, out_jpg):
    """SSIM between two renders; writes a difference image (red where they differ). Returns
    (ssim, mean pixel difference on the object)."""
    from skimage.metrics import structural_similarity
    A = np.asarray(Image.open(ref).convert("RGB"), np.float64) / 255
    B = np.asarray(Image.open(ours).convert("RGB"), np.float64) / 255
    if A.shape != B.shape:
        B = np.asarray(Image.open(ours).convert("RGB").resize(A.shape[1::-1]), np.float64) / 255
    s, full = structural_similarity(A, B, channel_axis=2, data_range=1.0, full=True)
    d = 1 - full.mean(2)
    bg = np.median(np.stack([A[0, 0], A[0, -1], A[-1, 0], A[-1, -1]]), 0)  # background from the corners
    fg = (np.abs(A - bg).max(2) > 0.03) | (np.abs(B - bg).max(2) > 0.03)
    pix = float(np.abs(A - B).max(2)[fg].mean()) if fg.any() else 0.0
    heat = np.clip((d - 0.1) * 2.5, 0, 1)
    rgb = np.stack([np.ones_like(heat), 1 - heat, 1 - heat], 2)  # white -> red
    gray = (A.mean(2) * 0.35 + 0.65)[..., None]
    img = np.where(heat[..., None] > 0.02, rgb, gray * np.ones(3))
    Image.fromarray((img * 255).astype(np.uint8)).save(out_jpg + ".tmp.jpg", quality=88)
    os.replace(out_jpg + ".tmp.jpg", out_jpg)
    return float(s), pix


def cached_diff(ref, ours, cache_dir):
    key = hashlib.sha256((os.path.basename(os.path.dirname(ref)) + "/" + os.path.basename(ref) + "|" +
                          os.path.basename(os.path.dirname(ours)) + "/" + os.path.basename(ours)).encode()).hexdigest()[:24]
    os.makedirs(cache_dir, exist_ok=True)
    jpg, js = os.path.join(cache_dir, key + ".jpg"), os.path.join(cache_dir, key + ".json")
    if os.path.exists(jpg) and os.path.exists(js):
        s, pix = json.load(open(js))
    else:
        s, pix = diff_image(ref, ours, jpg)
        json.dump([s, pix], open(js, "w"))
    return jpg, s, pix


def thumb(src, dst, size=360):
    if os.path.exists(dst):
        return
    im = Image.open(src).convert("RGB")
    im.thumbnail((size, size), Image.LANCZOS)
    im.save(dst, quality=86)


def build(run_dir, rows, bad, show, diff_cache, info, group_names=None, extra_sections=()):
    """Report of a run. bad/show rows carry r["_imgs"] = {view: (ref_jpg, ours_jpg)}.
    extra_sections: (title, html) pairs written after the thresholds."""
    rep = os.path.join(run_dir, "report")
    img = os.path.join(rep, "img")
    if os.path.isdir(img):
        shutil.rmtree(img)
    os.makedirs(img, exist_ok=True)
    for r in bad + show:
        r["_img"], r["ssim"], r["pixdiff"] = [], "", ""
        ss = []
        for v, (ref, ours) in (r.get("_imgs") or {}).items():
            if not (os.path.exists(ref) and os.path.exists(ours)):
                continue
            diff, s, pix = cached_diff(ref, ours, diff_cache)
            ss.append((s, pix))
            base = f"{r['group']}__{os.path.splitext(r['file'])[0]}_{v}" if r["group"] else f"{os.path.splitext(r['file'])[0]}_{v}"
            names = []
            for tag, src in (("ref", ref), ("ours", ours), ("diff", diff)):
                dst = os.path.join(img, f"{base}_{tag}.jpg")
                thumb(src, dst)
                names.append("img/" + os.path.basename(dst))
            r["_img"].append((v, names))
        if ss:
            r["ssim"] = min(x[0] for x in ss)
            r["pixdiff"] = max(x[1] for x in ss)
    write_html(os.path.join(rep, "index.html"), rows, bad, show, info, group_names, extra_sections)
    return os.path.join(rep, "index.html")


SECTIONS_END = "<!-- end of extra sections -->"


def section_id(title):
    return "x-" + (re.sub(r"[^a-z0-9]+", "-", title.lower()).strip("-") or "section")


def section_html(title, body, sid=None):
    sid = sid or section_id(title)
    return (f'<section id="{html.escape(sid)}"><h2>{html.escape(title)}</h2>\n{body}\n'
            f'</section><!-- /{html.escape(sid)} -->')


def add_section(index_html, title, body, sid=None):
    """Add a section (title, html body) to a written report, or replace the one with the same id
    (default: derived from the title), so running a later step again does not repeat it."""
    sid = sid or section_id(title)
    page = open(index_html, encoding="utf-8").read()
    new = section_html(title, body, sid)
    pat = re.compile(r'<section id="' + re.escape(html.escape(sid)) + r'">.*?</section><!-- /'
                     + re.escape(html.escape(sid)) + r' -->', re.S)
    if pat.search(page):
        page = pat.sub(lambda m: new, page, count=1)
    elif SECTIONS_END in page:
        page = page.replace(SECTIONS_END, new + "\n" + SECTIONS_END, 1)
    else:
        page = page.replace("</main>", new + "\n</main>", 1)
    tmp = index_html + ".tmp"
    open(tmp, "w", encoding="utf-8").write(page)
    os.replace(tmp, index_html)


def write_html(path, rows, bad, show, info, group_names=None, extra_sections=()):
    gnames = group_names or {}
    sys.path.insert(0, HERE)
    from compare import THRESHOLDS
    by_status, by_group, by_cat = {}, {}, {}
    for r in rows:
        by_status[r["status"]] = by_status.get(r["status"], 0) + 1
        d = by_group.setdefault(r["group"] or "(top level)", {})
        d[r["status"]] = d.get(r["status"], 0) + 1
        for c in filter(None, (r.get("categories") or "").split(",")):
            if r["status"] == "pass":
                continue
            by_cat.setdefault(c, []).append(r)
    e = lambda v: html.escape(str(v))  # noqa: E731
    h = [f"""<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>dwg2gltf vs reference</title><style>
:root{{--bg:#f6f6f4;--fg:#1d1d1f;--mut:#6b6b70;--line:#dcdcd8;--pass:#2f7d4a;--warn:#b7791f;--fail:#b42318;--card:#fff}}
@media (prefers-color-scheme: dark){{:root{{--bg:#17171a;--fg:#ececee;--mut:#9a9aa2;--line:#33333a;--card:#202024}}}}
body{{background:var(--bg);color:var(--fg);font:14px/1.45 "Noto Sans","Liberation Sans",system-ui,sans-serif;margin:0;padding:24px 16px}}
main{{max-width:1500px;margin:0 auto}} h1{{font-size:22px;margin:0 0 4px}} h2{{font-size:17px;margin:28px 0 8px}}
.mut{{color:var(--mut)}} table{{border-collapse:collapse}} td,th{{padding:4px 10px 4px 0;text-align:left;vertical-align:top}}
.sum td,.sum th{{border-bottom:1px solid var(--line)}} .pass{{color:var(--pass)}} .warn{{color:var(--warn)}} .fail,.error{{color:var(--fail)}}
.row{{display:grid;grid-template-columns:repeat(3,minmax(0,240px)) 1fr;gap:8px;background:var(--card);border:1px solid var(--line);
border-radius:6px;padding:10px;margin:10px 0}} .row img{{width:100%;border-radius:3px;display:block}}
.cap{{font-size:11px;color:var(--mut);text-align:center}} .m{{font:12px/1.5 ui-monospace,"Liberation Mono",monospace;overflow-wrap:anywhere}}
.views{{grid-column:1/4;display:grid;grid-template-columns:repeat(3,minmax(0,240px));gap:8px}}
@media (max-width:820px){{.row{{grid-template-columns:1fr 1fr 1fr}} .row .m{{grid-column:1/4}} .views{{grid-template-columns:1fr 1fr 1fr}}}}
a{{color:inherit}}</style></head><body><main>
<h1>dwg2gltf vs reference</h1>
<p class="mut">Run {e(info.get('run_id', ''))}: {len(rows)} DWGs with a reference, converter {e(info.get('converter', ''))},
generated {time.strftime('%Y-%m-%d %H:%M')}. Reference GLB generator: {e(info.get('ref_generator', '') or 'unknown')}. Renders: Blender Eevee, backface culling on,
identical camera framed on the union of both boxes (each its own when they hardly overlap). Columns per row: reference | ours | SSIM difference (red = differs).</p>
<h2>Summary</h2><table class="sum"><tr><th>group</th><th class="pass">pass</th><th class="warn">warn</th><th class="fail">fail</th><th>other</th></tr>"""]
    for src, d in sorted(by_group.items(), key=lambda x: ((gnames.get(x[0]) or x[0]).casefold(), x[0]) if gnames else x[0]):
        other = sum(v for k, v in d.items() if k not in ("pass", "warn", "fail"))
        label = e(src) if not gnames.get(src) else f'{e(gnames[src])} <span class="mut">{e(src)}</span>'
        h.append(f"<tr><td>{label}</td><td>{d.get('pass', 0)}</td><td>{d.get('warn', 0)}</td><td>{d.get('fail', 0)}</td><td>{other}</td></tr>")
    h.append(f"<tr><th>all</th><th>{by_status.get('pass', 0)}</th><th>{by_status.get('warn', 0)}</th><th>{by_status.get('fail', 0)}</th>"
             f"<th>{sum(v for k, v in by_status.items() if k not in ('pass', 'warn', 'fail'))}</th></tr></table>")
    h.append('<h2>Failure categories</h2><table class="sum"><tr><th>category</th><th>files</th><th>fail</th><th>meaning</th><th>examples</th></tr>')
    for c, rs in sorted(by_cat.items(), key=lambda x: -len(x[1])):
        rs = sorted(rs, key=lambda r: fnum(r.get("score"), 0))
        ex = ", ".join(f'<a href="#{e(r["key"])}">{e(r["key"][:60])}</a>' for r in rs[:3])
        nf = sum(r["status"] == "fail" for r in rs)
        h.append(f"<tr><td>{e(c)}</td><td>{len(rs)}</td><td>{nf}</td><td class='mut'>{e(CATEGORY_TEXT.get(c, ''))}</td><td>{ex}</td></tr>")
    h.append('</table><h2>Thresholds</h2><table class="sum"><tr><th>check</th><th>warn above</th><th>fail above</th></tr>')
    for k, (w, f) in THRESHOLDS.items():
        h.append(f"<tr><td>{e(k)}</td><td>{w:g}</td><td>{f:g}</td></tr>")
    h.append("</table><p class='mut'>A constant axis/unit transform always fails. offset (placement only) warns at most. "
             "inside_out is not judged where the reference is two-sided (more than 30 % of its faces duplicated with both windings).</p>")
    for title, body in extra_sections or ():
        h.append(section_html(title, body))
    h.append(SECTIONS_END)

    def block(title, rs):
        h.append(f"<h2>{e(title)} ({len(rs)})</h2>")
        for r in rs:
            mid = r["key"]
            views = r.get("_img", [])
            imgs, more = "", ""
            if views:
                v0, names = views[0]
                imgs = "".join(f'<div><img loading="lazy" src="{e(n)}" alt=""><div class="cap">{t} {e(v0)}</div></div>'
                               for n, t in zip(names, ("reference", "ours", "difference")))
                if len(views) > 1:
                    more = '<div class="views">' + "".join(
                        f'<div><img loading="lazy" src="{e(n)}" alt=""><div class="cap">{t} {e(v)}</div></div>'
                        for v, names in views[1:] for n, t in zip(names, ("reference", "ours", "difference"))) + "</div>"
            else:
                imgs = '<div class="cap" style="grid-column:1/4">no render</div>'
            gname = gnames.get(r.get("group"))
            met = [f'<b>{e(mid)}</b>' + (f' &middot; {e(gname)}' if gname else ""),
                   f'<b class="{e(r.get("status", ""))}">{e(r.get("status", ""))}</b> score {e(r.get("score", ""))} &middot; {e(r.get("categories") or "")}',
                   f'chamfer {e(r.get("chamfer", ""))} &middot; haus95 {e(r.get("haus95", ""))} &middot; missing {e(r.get("missing", ""))} &middot; extra {e(r.get("extra", ""))}',
                   f'extent_err {e(r.get("extent_err", ""))} &middot; area_err {e(r.get("area_err", ""))} &middot; inside_out {e(r.get("inside_out", ""))} &middot; transform {e(r.get("transform", ""))} &middot; offset {e(r.get("center_off", ""))}',
                   f'colour {e(r.get("color", ""))} &middot; texture {e(r.get("texture", ""))} &middot; tex scale x{e(r.get("tex_scale_ratio", ""))} &middot; normal maps {e(r.get("normal_maps_ours", ""))}/{e(r.get("normal_maps_ref", ""))} &middot; textured ours/ref {e(r.get("textured_ours", ""))}/{e(r.get("textured_ref", ""))} &middot; materials {e(r.get("mats_ours", ""))}/{e(r.get("mats_ref", ""))}',
                   f'ours {e(r.get("mat_colors_ours", ""))}<br>ref&nbsp; {e(r.get("mat_colors_ref", ""))}',
                   f'triangles {e(r.get("tris_ours", ""))}/{e(r.get("tris_ref", ""))} (ref duplicates {e(r.get("dup_ref", ""))}) &middot; extents ours {e(r.get("ext_ours", ""))} ref {e(r.get("ext_ref", ""))}',
                   f'SSIM {r["ssim"]:.3f} &middot; pixel diff {r["pixdiff"]:.3f}' if r.get("ssim") != "" else "",
                   f'log: {e(r.get("log_repaired") or "")} {e(r.get("log_skipped") or "")} {e(r.get("error") or "")}',
                   f'<span class="mut">{e(r.get("dwg", ""))}</span>']
            h.append(f'<div class="row" id="{e(mid)}">{imgs}<div class="m">' + "<br>".join(x for x in met if x) + f"</div>{more}</div>")

    block("Warn and fail, worst first", bad)
    block("Passing sample", show)
    h.append("</main></body></html>")
    open(path, "w", encoding="utf-8").write("\n".join(h))

