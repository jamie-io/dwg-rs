# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
"""Compare our GLB of a DWG with a reference GLB of the same drawing (library; run it through tools/qa/qa.py).

compare_glbs(ours, ref, plugins) prepares both surfaces once (constant transform, alignment,
deduplication, surface samples and nearest-point queries, see Context) and runs every metric
function of METRICS and of the plugins over them. A metric is `fn(ctx, res)`: it reads ctx and
writes columns into the dict res. A plugin is a Python file with a list METRICS of such
functions (qa.py run --plugins file.py ...); a metric that raises is recorded in
res["metric_errors"] and does not fail the row. classify() then sets status, score and categories
from THRESHOLDS.
"""
import hashlib
import importlib.util
import os
import subprocess
import sys
import time
import traceback
from types import SimpleNamespace

import numpy as np
from scipy.spatial import cKDTree

sys.path.insert(0, os.path.dirname(os.path.abspath(__file__)))
from glbmesh import Glb, linear_to_srgb  # noqa: E402

METRIC_VERSION = 11  # bump to recompute all cached comparisons
N_QUERY = 30000       # surface samples per side for the distances
N_TREE = 250000       # extra samples per side in the search structure
NEAR = 0.002          # a point within this fraction of the diagonal lies on the other surface
FAR = 0.01            # a point farther than this lies on a part the other side lacks
# check: (warn, fail). A value at or below warn passes; above fail fails.
THRESHOLDS = {
    "chamfer": (0.002, 0.01),        # symmetric mean distance / union bbox diagonal
    "haus95": (0.01, 0.05),          # 95th percentile of the larger one-sided distance / diagonal
    "extent_err": (0.01, 0.05),      # largest relative bbox size difference over the three axes
    "area_err": (0.10, 0.30),        # |log(area ours / area ref)|, after removing duplicate faces
    "missing": (0.01, 0.05),         # share of the reference surface with nothing of ours nearby
    "extra": (0.01, 0.05),           # share of our surface with nothing of the reference nearby
    "inside_out": (0.01, 0.05),      # share of coincident surface whose normals oppose the reference
    "color": (0.10, 0.25),           # mean sRGB distance (0..1.7) of base colour at coincident points
    "texture": (0.10, 0.40),         # share of coincident surface textured on one side only
    "normal_map": (0.10, 1.01),      # share of coincident surface with a normal map only in the reference (warns only)
    "tex_scale": (0.18, 0.41),       # |log| of the median ratio of texture repeats per meter (1.2x, 1.5x)
    "center_off": (0.01, 0.05),      # bbox centre offset / diagonal (placement, reported separately)
}


# --- geometry -----------------------------------------------------------------

def tri_area_normal(t):
    c = np.cross(t[:, 1] - t[:, 0], t[:, 2] - t[:, 0])
    a = np.linalg.norm(c, axis=1)
    n = c / np.maximum(a, 1e-30)[:, None]
    return a / 2, n


def dedupe(t, q):
    """Index of the first copy of each triangle (same three corners, either winding)."""
    if not len(t):
        return np.zeros(0, np.int64)
    k = np.round(t / q).astype(np.int64).reshape(len(t), 9)
    v = k.reshape(-1, 3)
    _, vid = np.unique(v, axis=0, return_inverse=True)
    key = np.sort(vid.reshape(-1, 3), axis=1)
    _, first = np.unique(key, axis=0, return_index=True)
    return np.sort(first)


def sample(t, area, n, rng):
    p = area / area.sum()
    i = rng.choice(len(t), n, p=p)
    r1, r2 = rng.random(n), rng.random(n)
    s = np.sqrt(r1)
    a, b, c = t[i, 0], t[i, 1], t[i, 2]
    pts = a * (1 - s)[:, None] + b * (s * (1 - r2))[:, None] + c * (s * r2)[:, None]
    return pts, i


def closest_on_tri(p, a, b, c):
    """Closest points of p on triangles (a, b, c), all (M,3) (Ericson 5.1.5)."""
    ab, ac, ap = b - a, c - a, p - a
    d1, d2 = (ab * ap).sum(1), (ac * ap).sum(1)
    bp = p - b
    d3, d4 = (ab * bp).sum(1), (ac * bp).sum(1)
    cp = p - c
    d5, d6 = (ab * cp).sum(1), (ac * cp).sum(1)
    va = d3 * d6 - d5 * d4
    vb = d5 * d2 - d1 * d6
    vc = d1 * d4 - d3 * d2
    denom = va + vb + vc
    denom = np.where(np.abs(denom) < 1e-300, 1e-300, denom)
    v = vb / denom
    w = vc / denom
    out = a + ab * v[:, None] + ac * w[:, None]  # interior

    def put(mask, val):
        out[mask] = val[mask]

    eps = 1e-300
    # edge BC
    m = (va <= 0) & ((d4 - d3) >= 0) & ((d5 - d6) >= 0)
    ww = (d4 - d3) / np.maximum((d4 - d3) + (d5 - d6), eps)
    put(m, b + (c - b) * ww[:, None])
    # edge AC
    m = (vb <= 0) & (d2 >= 0) & (d6 <= 0)
    ww = d2 / np.maximum(d2 - d6, eps)
    put(m, a + ac * ww[:, None])
    # edge AB
    m = (vc <= 0) & (d1 >= 0) & (d3 <= 0)
    vv = d1 / np.maximum(d1 - d3, eps)
    put(m, a + ab * vv[:, None])
    # vertices
    put((d6 >= 0) & (d5 <= d6), c)
    put((d3 >= 0) & (d4 <= d3), b)
    put((d1 <= 0) & (d2 <= 0), a)
    return out


class Surface:
    """Triangles with two search structures over surface samples: by position, and by position plus
    normal (scaled by `w`), so a point on one face of a thin plate also finds the triangles of that
    face and not only those of the face behind it."""

    def __init__(self, tris, rng, w):
        self.t = tris
        self.area, self.n = tri_area_normal(tris)
        keep = self.area > 0
        self.t, self.area, self.n = self.t[keep], self.area[keep], self.n[keep]
        self.idx = np.flatnonzero(keep)  # into the original triangle list
        pts, ti = sample(self.t, self.area, N_TREE, rng)
        cen = self.t.mean(1)
        self.w = w
        self.tree_pts = np.concatenate([pts, cen])
        self.tree_tri = np.concatenate([ti, np.arange(len(self.t))])
        self.tree = cKDTree(self.tree_pts)
        self.tree6 = cKDTree(np.hstack([self.tree_pts, self.n[self.tree_tri] * w]))

    def query(self, p, n, k=12):
        """Exact distance from points p (normals n) to the nearest of the candidate triangles, the candidate
        triangles (M,2k) with their distances, and the nearest triangle."""
        _, i3 = self.tree.query(p, k=k)
        _, i6 = self.tree6.query(np.hstack([p, n * self.w]), k=k)
        cand = self.tree_tri[np.hstack([i3, i6])]  # (M,2k)
        M, K = cand.shape
        pp = np.repeat(p, K, axis=0)
        c = cand.reshape(-1)
        cl = closest_on_tri(pp, self.t[c, 0], self.t[c, 1], self.t[c, 2])
        d = np.linalg.norm(pp - cl, axis=1).reshape(M, K)
        j = d.argmin(1)
        return d[np.arange(M), j], cand, d, cand[np.arange(M), j]


def signed_perms():
    import itertools
    for perm in itertools.permutations(range(3)):
        for signs in itertools.product((1, -1), repeat=3):
            m = np.zeros((3, 3))
            for r, (c, s) in enumerate(zip(perm, signs)):
                m[r, c] = s
            yield perm, m


SCALES = [1, 1000, 100, 10, 0.1, 0.01, 0.001, 25.4, 1 / 25.4, 0.0254, 1 / 0.0254]


def detect_transform(a, b, rng):
    """Constant global transform taking ours (a) to ref (b): axis permutation/sign and unit scale,
    judged by bbox extents and confirmed by a quick chamfer distance. Returns (3x3 matrix, scale, label)."""
    ea, eb = np.ptp(a.reshape(-1, 3), 0), np.ptp(b.reshape(-1, 3), 0)
    if not (ea > 0).any() or not (eb > 0).any():
        return np.eye(3), 1.0, "identity"
    tol = 1e-9 + 1e-3 * max(eb.max(), 1e-9)

    def fit(m, s):
        e = np.abs(m) @ ea * s
        return np.max(np.abs(np.log((e + tol) / (eb + tol))))

    ident = fit(np.eye(3), 1)
    if ident < 0.02:
        return np.eye(3), 1.0, "identity"
    best = []
    import itertools
    for perm in itertools.permutations(range(3)):
        m = np.eye(3)[list(perm)]  # extents do not depend on the signs
        for s in SCALES:
            best.append((fit(m, s), perm, s))
    best.sort(key=lambda x: x[0])
    err, perm, s = best[0]
    if err > 0.05 or err > ident / 3:
        return np.eye(3), 1.0, "identity"
    # among sign variants of the chosen permutation, the one with the smallest quick chamfer
    pa = a.reshape(-1, 3)[rng.choice(a.size // 3, min(4000, a.size // 3))]
    pb = b.reshape(-1, 3)[rng.choice(b.size // 3, min(4000, b.size // 3))]
    tb = cKDTree(pb - pb.mean(0))
    cands = []
    for p2, m in signed_perms():
        if p2 != perm:
            continue
        x = (pa - pa.mean(0)) @ m.T * s
        cands.append((tb.query(x)[0].mean(), m))
    cands.sort(key=lambda x: x[0])
    m = cands[0][1]
    parts = []
    if not np.allclose(m, np.eye(3)):
        names = "XYZ"
        parts.append("axes " + ",".join(("-" if m[r].sum() < 0 else "") + names[int(np.abs(m[r]).argmax())] for r in range(3)))
        if np.linalg.det(m) < 0:
            parts.append("mirrored")
    if s != 1:
        parts.append(f"scale x{s:g}")
    return m, s, " ".join(parts) or "identity"


# --- one file -------------------------------------------------------------------

def convert(dwg, glb, binary, timeout):
    os.makedirs(os.path.dirname(glb), exist_ok=True)
    t0 = time.time()
    try:
        r = subprocess.run([binary, dwg, "-o", glb], capture_output=True, text=True, timeout=timeout)
        out, code = r.stdout + r.stderr, r.returncode
    except subprocess.TimeoutExpired:
        out, code = f"timeout after {timeout}s", -9
    open(os.path.splitext(glb)[0] + ".log", "w").write(out)
    return code, time.time() - t0, out


def parse_log(out, res):
    """Columns from dwg2gltf's summary output."""
    for line in out.splitlines():
        t = line.strip()
        if t.startswith("converted:"):
            res["log_converted"] = t.split(":", 1)[1].strip()[:300]
        elif t.startswith("triangles:"):
            res["log_triangles"] = t.split(":", 1)[1].strip()
        elif t.startswith("repaired:"):
            res["log_repaired"] = t.split(":", 1)[1].strip()[:200]
        elif t.startswith("skipped:"):
            res["log_skipped"] = t.split(":", 1)[1].strip()[:200]
        elif t.startswith("warning:"):
            res["log_warning"] = t[:200]
    res["series_hint"] = "--seriesdwg" in out


def code_hash(plugin_paths=()):
    """Identifies the metric code: comparisons cached under another hash are recomputed."""
    h = hashlib.sha256(str(METRIC_VERSION).encode())
    here = os.path.dirname(os.path.abspath(__file__))
    for p in [os.path.join(here, "compare.py"), os.path.join(here, "glbmesh.py")] + list(plugin_paths):
        h.update(open(p, "rb").read())
    return h.hexdigest()[:12]


def load_plugins(paths):
    """Metric functions of plugin files (each defines METRICS = [fn(ctx, res), ...])."""
    fns = []
    for p in paths:
        spec = importlib.util.spec_from_file_location(f"qa_plugin_{abs(hash(p))}", p)
        mod = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(mod)
        fns += list(getattr(mod, "METRICS", []))
    return fns


def bbox6(t):
    v = t.reshape(-1, 3)
    return [round(float(x), 6) for x in list(v.min(0)) + list(v.max(0))]


def compare_glbs(glb, ref, plugins=()):
    """All metrics of our GLB against the reference GLB, as a dict of columns (with status)."""
    res = {}
    A, B = Glb(glb), Glb(ref)
    res["ref_generator"] = B.generator
    res["tris_ours"], res["tris_ref"] = len(A.tris), len(B.tris)
    if not len(B.tris):
        res.update(status="skip", reasons="ref_empty", category="ref_empty", categories="ref_empty")
        return res
    res["bbox_ref"] = bbox6(B.tris)  # raw glTF bounds (Y-up), used to frame the renders
    if not len(A.tris):
        return res  # the caller classifies an empty conversion
    res["bbox_ours"] = bbox6(A.tris)
    ctx = prepare(A, B, res)
    errors = []
    for fn in list(METRICS) + list(plugins):
        try:
            fn(ctx, res)
        except Exception as e:
            errors.append(f"{getattr(fn, '__name__', fn)}: {type(e).__name__}: {e}")
            res.setdefault("trace", traceback.format_exc()[-600:])
    res["metric_errors"] = "; ".join(errors)
    classify(res)
    return res


def prepare(A, B, res):
    """Context shared by the metrics: both meshes in the reference's frame, deduplicated, sampled,
    and the nearest-point queries in both directions.

    ctx.A, ctx.B          Glb objects (materials, uv_density, ...)
    ctx.ta, ctx.tb        triangles (T,3,3) in the reference frame; ctx.diag (m), ctx.q (weld size)
    ctx.ka, ctx.kb        indices of the first copy of each triangle (duplicates removed)
    ctx.SA, ctx.SB        Surface objects (kept triangles, normals, .idx into the Glb triangles)
    ctx.pa, ctx.ia        our surface samples and their triangle in SA (likewise pb, ib for the ref)
    ctx.dab, ctx.cand_ab, ctx.dmat_ab, ctx.near_b   distances of pa to the ref, candidate triangles
    ctx.dba, ...          the same from the reference to ours
    ctx.on                our samples lying on the reference surface (dab < NEAR * diag)
    """
    rng = np.random.default_rng(1)
    lo_b, hi_b = B.tris.reshape(-1, 3).min(0), B.tris.reshape(-1, 3).max(0)
    va = A.tris.reshape(-1, 3)
    res["ext_ours"] = " ".join(f"{x:.4f}" for x in va.max(0) - va.min(0))
    res["ext_ref"] = " ".join(f"{x:.4f}" for x in hi_b - lo_b)
    # constant global transform (axes, units): detect and report, then compare in the ref's frame
    m, s, label = detect_transform(A.tris, B.tris, rng)
    res["transform"] = label
    ta = A.tris @ m.T * s if label != "identity" else A.tris
    tb = B.tris
    va = ta.reshape(-1, 3)
    lo_a, hi_a = va.min(0), va.max(0)
    ea, eb = hi_a - lo_a, hi_b - lo_b
    diag = float(np.linalg.norm(np.maximum(hi_a, hi_b) - np.minimum(lo_a, lo_b)))
    res["diag_ref"] = round(float(np.linalg.norm(eb)), 4)
    big = max(eb.max(), 1e-9)
    res["extent_err"] = float(np.max(np.abs(ea - eb) / np.maximum(eb, 0.02 * big)))
    off = ((lo_a + hi_a) - (lo_b + hi_b)) / 2
    res["center_off"] = float(np.linalg.norm(off) / max(np.linalg.norm(eb), 1e-9))
    res["center_off_m"] = " ".join(f"{x:.4f}" for x in off)
    if res["center_off"] > THRESHOLDS["center_off"][0] and res["extent_err"] < 0.02:
        ta = ta - off  # pure translation: compare shapes, report the offset
        res["aligned"] = "translated"
        va = ta.reshape(-1, 3)
        lo_a, hi_a = va.min(0), va.max(0)
        diag = float(np.linalg.norm(np.maximum(hi_a, hi_b) - np.minimum(lo_a, lo_b)))
    res["diag"] = round(diag, 4)
    q = diag * 1e-6
    ka, kb = dedupe(ta, q), dedupe(tb, q)
    SA, SB = Surface(ta, rng, 0.02 * diag), Surface(tb, rng, 0.02 * diag)
    pa, ia = sample(SA.t, SA.area, N_QUERY, rng)
    pb, ib = sample(SB.t, SB.area, N_QUERY, rng)
    dab, cand_ab, dmat_ab, near_b = SB.query(pa, SA.n[ia])
    dba, cand_ba, dmat_ba, near_a = SA.query(pb, SB.n[ib])
    return SimpleNamespace(A=A, B=B, ta=ta, tb=tb, diag=diag, q=q, ka=ka, kb=kb, SA=SA, SB=SB,
                           pa=pa, ia=ia, pb=pb, ib=ib, dab=dab, cand_ab=cand_ab, dmat_ab=dmat_ab,
                           near_b=near_b, dba=dba, cand_ba=cand_ba, dmat_ba=dmat_ba, near_a=near_a,
                           on=dab < NEAR * diag)


def m_area_volume(ctx, res):
    """Surface area without duplicate faces; closed volume and inside-out shells."""
    ta, tb, ka, kb = ctx.ta, ctx.tb, ctx.ka, ctx.kb
    res["dup_ours"] = round(1 - len(ka) / len(ta), 4)
    res["dup_ref"] = round(1 - len(kb) / len(tb), 4)
    # some exporters write the faces of double-sided materials twice, once per winding
    res["ref_two_sided"] = res["dup_ref"] > 0.3
    area_a = tri_area_normal(ta[ka])[0].sum()
    area_b = tri_area_normal(tb[kb])[0].sum()
    res["area_ours"], res["area_ref"] = round(float(area_a), 5), round(float(area_b), 5)
    res["area_err"] = float(abs(np.log(max(area_a, 1e-12) / max(area_b, 1e-12))))
    for tag, t, k in (("ours", ta, ka), ("ref", tb, kb)):
        vol, neg, closed_area = closed_volume(t[k], ctx.q)
        res[f"vol_{tag}"] = round(vol, 6) if vol is not None else ""
        res[f"neg_shells_{tag}"] = neg if vol is not None else ""
        res[f"closed_frac_{tag}"] = round(closed_area, 3) if vol is not None else ""


def m_distances(ctx, res):
    """Chamfer, Hausdorff 95 %, surface missing on either side."""
    dab, dba, diag = ctx.dab, ctx.dba, ctx.diag
    res["chamfer"] = float((dab.mean() + dba.mean()) / 2 / diag)
    res["haus95"] = float(max(np.percentile(dab, 95), np.percentile(dba, 95)) / diag)
    res["haus_max"] = float(max(dab.max(), dba.max()) / diag)
    res["missing"] = float((dba > FAR * diag).mean())  # ref surface not covered by ours
    res["extra"] = float((dab > FAR * diag).mean())
    res["coincident"] = float(ctx.on.mean())


def m_normals(ctx, res):
    """Inside-out where the nearest reference triangle facing our way is farther than the nearest
    one facing the other way (a thin plate's back face does not count) and more than 0.1 % of the
    diagonal away (the candidate search is not exact at that scale)."""
    dots = np.einsum("mj,mkj->mk", ctx.SA.n[ctx.ia], ctx.SB.n[ctx.cand_ab])
    d_agree = np.where(dots > 0.3, ctx.dmat_ab, np.inf).min(1)
    d_opp = np.where(dots < -0.3, ctx.dmat_ab, np.inf).min(1)
    opposite = (d_opp + 1e-4 * ctx.diag < d_agree) & (d_agree > 1e-3 * ctx.diag)
    res["inside_out"] = float(opposite[ctx.on].mean()) if ctx.on.any() else 0.0


def m_materials(ctx, res):
    """Materials by base colour where the surfaces coincide (names differ between exporters):
    colour, texture presence, texture scale, normal maps, ACI 7 white/black."""
    A, B, SA, SB, ia, near_b, on = ctx.A, ctx.B, ctx.SA, ctx.SB, ctx.ia, ctx.near_b, ctx.on
    used_a = sorted(set(A.mat[SA.idx].tolist()))
    used_b = sorted(set(B.mat[SB.idx].tolist()))
    res["mats_ours"], res["mats_ref"] = len(used_a), len(used_b)
    res["textured_ours"] = any(A.materials[i]["textured"] for i in used_a if i >= 0)
    res["textured_ref"] = any(B.materials[i]["textured"] for i in used_b if i >= 0)

    def mcol(G, idx):
        cols = np.array([linear_to_srgb(mm["color"]) for mm in G.materials] + [[0.8, 0.8, 0.8]])
        tex = np.array([mm["textured"] for mm in G.materials] + [False])
        alpha = np.array([mm["alpha"] for mm in G.materials] + [1.0])
        idx = np.where(idx < 0, len(G.materials), idx)
        return cols[idx], tex[idx], alpha[idx]

    ca, xa, aa = mcol(A, A.mat[SA.idx[ia]])
    cb, xb, ab = mcol(B, B.mat[SB.idx[near_b]])
    res["color"] = res["color_bad"] = res["texture"] = res["alpha_diff"] = ""
    res["tex_scale_ratio"] = res["tex_scale"] = res["tex_scale_ratio_raw"] = ""
    if on.any():
        dc = np.linalg.norm(ca - cb, axis=1)
        res["color"] = float(dc[on].mean())
        res["color_bad"] = float((dc[on] > 0.2).mean())
        res["texture"] = float((xa != xb)[on].mean())
        res["alpha_diff"] = float(np.abs(aa - ab)[on].mean())
        # texture scale: repeats per meter where both sides show a texture with visible contrast
        # (tex_scale_ratio_raw: wherever both are textured, also nearly uniform textures)
        std_b = np.array([mm["tex_std"] for mm in B.materials] + [0.0])
        mb_ = B.mat[SB.idx[near_b]]
        mb_i = np.where(mb_ < 0, len(B.materials), mb_)
        ua, ub = A.uv_density[SA.idx[ia]], B.uv_density[SB.idx[near_b]]
        ok0 = on & xa & xb & np.isfinite(ua) & np.isfinite(ub) & (ua > 0) & (ub > 0)
        if ok0.sum() > 50:
            res["tex_scale_ratio_raw"] = float(np.median(ua[ok0] / ub[ok0]))
        ok = ok0 & (std_b[mb_i] > 0.03)
        if ok.sum() > 50:
            ratio = float(np.median(ua[ok] / ub[ok]))
            res["tex_scale_ratio"] = ratio
            res["tex_scale"] = float(abs(np.log(ratio)))
        elif res["tex_scale_ratio_raw"] != "" and abs(np.log(res["tex_scale_ratio_raw"])) > np.log(3):
            # a low-contrast texture hides a small scale error, not a 3x or 100x one: at 100x too
            # many repeats a wood or fabric texture averages to a flat colour
            ratio = res["tex_scale_ratio_raw"]
            res["tex_scale_ratio"] = ratio
            res["tex_scale"] = float(abs(np.log(ratio)))
        # normal (bump) maps: some exporters write them for many fabrics and metals
        nm_a = np.array([mm["normal_map"] for mm in A.materials] + [False])
        nm_b = np.array([mm["normal_map"] for mm in B.materials] + [False])
        ma_ = A.mat[SA.idx[ia]]
        ha = nm_a[np.where(ma_ < 0, len(A.materials), ma_)]
        hb = nm_b[mb_i]
        res["normal_map"] = float((on & hb & ~ha).mean() / max(on.mean(), 1e-9))
        res["normal_maps_ours"], res["normal_maps_ref"] = int(nm_a[:-1].sum()), int(nm_b[:-1].sum())
        # ACI 7 ("white/black") drawn white by us and black by the reference
        res["white_black"] = float(((ca.min(1) > 0.95) & (cb.max(1) < 0.05))[on].mean())
    res["mean_color_ours"] = hexcol(ca.mean(0))
    res["mean_color_ref"] = hexcol(cb.mean(0))
    res["mat_colors_ours"] = " ".join(hexcol(linear_to_srgb(A.materials[i]["color"])) + ("T" if A.materials[i]["textured"] else "") for i in used_a if i >= 0)
    res["mat_colors_ref"] = " ".join(hexcol(linear_to_srgb(B.materials[i]["color"])) + ("T" if B.materials[i]["textured"] else "") for i in used_b if i >= 0)


METRICS = [m_area_volume, m_distances, m_normals, m_materials]


def hexcol(c):
    c = np.clip(np.asarray(c), 0, 1)
    return "#" + "".join(f"{int(round(x * 255)):02x}" for x in c)


def closed_volume(t, q, max_tris=600000):
    """Volume of the closed shells (watertight components), their share of the area and how many
    shells have negative volume (inside out). None for meshes that are too big."""
    if len(t) > max_tris or not len(t):
        return None, None, None
    k = np.round(t / q).astype(np.int64).reshape(-1, 3)
    _, vid = np.unique(k, axis=0, return_inverse=True)
    f = vid.reshape(-1, 3)
    e = np.concatenate([f[:, [0, 1]], f[:, [1, 2]], f[:, [2, 0]]])
    fe = np.tile(np.arange(len(f)), 3)
    key = np.sort(e, 1)
    uk, inv, cnt = np.unique(key, axis=0, return_inverse=True, return_counts=True)
    # components through edges shared by exactly two faces
    from scipy.sparse import coo_matrix
    from scipy.sparse.csgraph import connected_components
    order = np.argsort(inv, kind="stable")
    inv_s, fe_s = inv[order], fe[order]
    starts = np.r_[0, np.flatnonzero(np.diff(inv_s)) + 1]
    two = cnt[inv_s[starts]] == 2
    s2 = starts[two]
    g = coo_matrix((np.ones(len(s2)), (fe_s[s2], fe_s[s2 + 1])), shape=(len(f), len(f)))
    ncomp, lab = connected_components(g, directed=False)
    # a component is closed when none of its edges is used by 1 or >2 faces
    bad_edge = cnt[inv] != 2
    bad_comp = np.zeros(ncomp, bool)
    bad_comp[lab[fe[bad_edge]]] = True
    a0, a1, a2 = t[:, 0], t[:, 1], t[:, 2]
    sv = np.einsum("ij,ij->i", a0, np.cross(a1, a2)) / 6
    area = np.linalg.norm(np.cross(a1 - a0, a2 - a0), axis=1) / 2
    cv = np.bincount(lab, sv, ncomp)
    ca = np.bincount(lab, area, ncomp)
    closed = ~bad_comp & (ca > 0)
    vol = float(np.abs(cv[closed]).sum())
    neg = int(((cv < -1e-12) & closed & (np.abs(cv) > 1e-9)).sum())
    return vol, neg, float(ca[closed].sum() / max(area.sum(), 1e-30))


def level(v, check):
    w, f = THRESHOLDS[check]
    if v == "" or v is None:
        return 0
    return 2 if v > f else 1 if v > w else 0


def classify(res):
    """pass/warn/fail plus reasons; score 100 - penalties (10 per warn-level check scaled up to 40 at fail)."""
    reasons, worst, score = [], 0, 100.0
    if res.get("transform", "identity") != "identity":
        reasons.append("transform")
        worst, score = 2, score - 40
    checks = ["chamfer", "haus95", "extent_err", "area_err", "missing", "extra", "color", "texture", "tex_scale", "normal_map"]
    if not res.get("ref_two_sided"):
        checks.append("inside_out")
    for c in checks:
        v = res.get(c, "")
        if v == "" or v is None:
            continue
        w, f = THRESHOLDS[c]
        lv = level(v, c)
        if lv:
            reasons.append(c)
            worst = max(worst, lv)
        score -= 0 if v <= w else min(40.0, 10 + 30 * (v - w) / (f - w))
    if level(res.get("center_off", 0), "center_off"):
        reasons.append("offset")  # placement only: reported, not failed
        worst = max(worst, 1)
        score -= 5
    res["score"] = round(max(score, 0.0), 1)
    res["status"] = ["pass", "warn", "fail"][worst]
    res["reasons"] = ",".join(reasons)
    res["categories"] = ",".join(categories(res, reasons))
    res["category"] = res["categories"].split(",")[0] if res["status"] != "pass" else ""


def categories(res, reasons):
    """Failure categories for the report, most important first."""
    r, out = set(reasons), []
    if "transform" in r:
        out.append("axis_units")
    if "missing" in r:
        out.append("missing_parts")
    if "extra" in r:
        out.append("extra_parts")
    if r & {"chamfer", "haus95", "extent_err"} and not r & {"missing", "extra"}:
        out.append("shape")
    if "inside_out" in r:
        out.append("inside_out")
    if "area_err" in r and not r & {"missing", "extra", "chamfer", "haus95", "extent_err"}:
        out.append("tessellation")
    if "texture" in r:
        out.append("texture_missing" if res.get("textured_ref") and not res.get("textured_ours")
                   else "texture_extra" if res.get("textured_ours") and not res.get("textured_ref") else "texture_mismatch")
    if "tex_scale" in r:
        out.append("texture_scale")
    if "normal_map" in r:
        out.append("normal_map_missing")
    if "color" in r:
        out.append("color_aci7" if (res.get("white_black") or 0) > 0.05 else "color")
    if "offset" in r:
        out.append("offset")
    return out


# --- driver ---------------------------------------------------------------------

COLUMNS = ["status", "score", "category", "categories", "reasons", "group", "file", "name", "transform", "chamfer", "haus95", "haus_max", "extent_err", "area_err",
           "missing", "extra", "coincident", "inside_out", "ref_two_sided", "color", "color_bad", "texture",
           "alpha_diff", "white_black", "tex_scale", "tex_scale_ratio", "tex_scale_ratio_raw", "normal_map", "normal_maps_ours", "normal_maps_ref", "center_off", "center_off_m", "aligned", "ext_ours", "ext_ref", "diag", "area_ours",
           "area_ref", "dup_ours", "dup_ref", "vol_ours", "vol_ref", "neg_shells_ours", "neg_shells_ref",
           "closed_frac_ours", "closed_frac_ref", "tris_ours", "tris_ref", "mats_ours", "mats_ref", "textured_ours",
           "textured_ref", "mean_color_ours", "mean_color_ref", "mat_colors_ours", "mat_colors_ref", "convert_code",
           "convert_s", "log_triangles", "log_repaired", "log_skipped", "series_hint", "ref_generator", "commit",
           "dwg", "ref", "glb", "dwg_sha", "ref_sha", "bbox_ours", "bbox_ref",
           "log_converted", "log_warning", "metric_errors", "error"]
