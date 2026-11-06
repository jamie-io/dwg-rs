# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
"""Compare dwg2gltf's GLBs with reference GLBs of the same drawings.

    python tools/qa/qa.py run                      # build, convert, compare, render, report, collages
    python tools/qa/qa.py run --subset 40 --skip-collage   # quick iteration
    python tools/qa/qa.py diff                     # previous run vs latest; exit 1 on regressions
    python tools/qa/qa.py select --run latest      # choose the drawings for collages, slides and shots
    python tools/qa/qa.py prune                    # apply the GLB retention policy to the cache

Input (--samples, default samples/): DWG files directly in the folder and/or in its top-level
subfolders ("groups"). The reference for <dir>/<stem>.dwg is <dir>/ref/<stem>.glb; DWGs without a
reference are counted and skipped. A drawing is identified by <group>/<file> (just <file> at the
top level); --group limits a run to some subfolders, --list to the DWGs listed in a file, and
--settle N skips DWGs modified in the last N seconds (a folder that is still being filled).
--group-names TSV gives groups display names in the report and collages. results.csv of older
runs with a `folder` column instead of `group` is read the same way.

    samples/
      a.dwg   ref/a.glb
      solids/b.dwg   solids/ref/b.glb
      surfaces/c.dwg surfaces/ref/c.glb

Output (--out, default samples/compare/, skipped when scanning): every run gets an id
<date>-<commit>[-dirty]-<binary sha256> and its own folder runs/<id>/ (results.csv, run.json,
report/, collage/, shots/, renders_index.json); latest points to the last full run and
renders_index.json is a copy of its index. Converted GLBs, comparisons and renders live in cache/,
keyed by content: our GLB by (DWG sha256, binary sha256, options), a comparison additionally by the
reference sha256 and the metric code, renders by render settings and camera box plus the reference
sha256 (reference images, reused across converter versions) or the GLB key (ours, never reused by
another binary). Converted GLBs are deleted right after their comparison unless the drawing is
flagged (warn/fail) or in the selection; the GLB cache is capped (--keep-glbs, default 4 GB, least
recently used first) and a GLB that a render needs later is converted again with the same binary.
Results, comparisons and renders are kept. Reference GLBs are never touched.

The selection (--selection, default <out>/selection.json, written by `select`) lists the drawings
shown on collage sheets, slides and shots; without it those steps produce nothing.
"""
import argparse
import csv
import hashlib
import html
import hashlib
import json
import os
import shutil
import subprocess
import sys
import time
from concurrent.futures import ProcessPoolExecutor, as_completed

HERE = os.path.dirname(os.path.abspath(__file__))
REPO = os.path.dirname(os.path.dirname(HERE))
sys.path.insert(0, HERE)
import compare  # noqa: E402

SAMPLES = os.path.join(REPO, "samples")
OUT = os.path.join(REPO, "samples", "compare")
BLENDER = os.environ.get("BLENDER", "/usr/bin/blender")
RENDER = {"eng": {"res": 640, "samples": 16}, "present": {"res": 1024, "samples": 96}}
COLUMNS = compare.COLUMNS + ["compare_s", "run_id"]
STATUS_RANK = {"pass": 0, "skip": 0, "warn": 1, "fail": 2, "error": 2}
DIFF_METRICS = ["score", "chamfer", "haus95", "extent_err", "area_err", "missing", "extra", "inside_out", "color",
                "texture", "tex_scale", "normal_map", "tris_ours"]


def log(*a):
    print(*a, file=sys.stderr, flush=True)


def row_key(group, file):
    return f"{group}/{file}" if group else file


def write_json(path, obj):
    os.makedirs(os.path.dirname(path), exist_ok=True)
    with open(path + ".tmp", "w", encoding="utf-8") as f:
        json.dump(obj, f, indent=1, ensure_ascii=False, default=float)
    os.replace(path + ".tmp", path)


def sha16(text):
    return hashlib.sha256(text.encode()).hexdigest()[:16]


class Hashes:
    """sha256 of files, remembered by (size, mtime) in cache/sha256.json."""

    def __init__(self, path):
        self.path = path
        try:
            self.memo = json.load(open(path))
        except (OSError, ValueError):
            self.memo = {}
        self.dirty = False

    def __call__(self, p):
        st = os.stat(p)
        m = self.memo.get(p)
        if m and m[0] == st.st_size and m[1] == st.st_mtime:
            return m[2]
        h = hashlib.sha256()
        with open(p, "rb") as f:
            for chunk in iter(lambda: f.read(1 << 20), b""):
                h.update(chunk)
        self.memo[p] = [st.st_size, st.st_mtime, h.hexdigest()]
        self.dirty = True
        return h.hexdigest()

    def save(self):
        if self.dirty:
            write_json(self.path, self.memo)


# --- converter ----------------------------------------------------------------------

def git(*args):
    return subprocess.run(["git", "-C", REPO, *args], capture_output=True, text=True, check=True).stdout.strip()


def resolve_converter(a, hashes):
    if a.bin:
        binary = os.path.abspath(a.bin)
    else:
        log("building dwg2gltf (cargo build --release --offline)")
        subprocess.run(["cargo", "build", "--release", "--offline", "-p", "dwg2gltf"], cwd=REPO, check=True)
        binary = os.path.join(REPO, "target", "release", "dwg2gltf")
    sha = hashes(binary)
    info = {"path": binary, "sha256": sha}
    if a.commit:
        info.update(commit=a.commit, dirty=False, source="as given (--commit)")
    elif os.path.realpath(binary).startswith(os.path.realpath(REPO) + os.sep):
        try:
            src = ("crates", "Cargo.toml", "Cargo.lock", "vendor")
            info["commit"] = git("log", "-1", "--format=%h", "--", *src)  # last change of the converter source
            info["head"] = git("rev-parse", "--short", "HEAD")
            info["dirty"] = bool(git("status", "--porcelain", "--", *src))
            info["source"] = REPO
        except Exception:
            info.update(commit="unknown", dirty=False, source="git unavailable")
    else:
        info.update(commit="external", dirty=False, source="binary outside this repository")
    try:
        v = subprocess.run([binary, "--version"], capture_output=True, text=True, timeout=10)
        info["version"] = (v.stdout or v.stderr).strip().splitlines()[0][:100]
    except Exception:
        info["version"] = ""
    return info


def blender_version():
    try:
        return subprocess.run([BLENDER, "--version"], capture_output=True, text=True, timeout=60).stdout.splitlines()[0].strip()
    except Exception:
        return "unknown"


# --- samples ------------------------------------------------------------------------

def read_list(path):
    """Paths relative to --samples, one per line (blank lines and # comments ignored)."""
    with open(path, encoding="utf-8") as f:
        return {os.path.normpath(x.strip()).replace(os.sep, "/") for x in f
                if x.strip() and not x.lstrip().startswith("#")}


def read_group_names(path):
    """{group: display name} from a TSV whose first two columns are group and display name. A
    header line is allowed (it names a group that does not exist); # lines are ignored."""
    names = {}
    if path:
        with open(path, encoding="utf-8", newline="") as f:
            for row in csv.reader(f, delimiter="\t"):
                if len(row) >= 2 and row[0].strip() and not row[0].startswith("#") and row[1].strip():
                    names[row[0].strip()] = row[1].strip()
    return names


def scan(samples, a, sel_keys):
    """DWGs in `samples` and its top-level subfolders (groups) that have <dir>/ref/<stem>.glb.
    Returns (items, DWGs without a reference, DWGs skipped by --settle)."""
    items, no_ref, settling = [], 0, 0
    samples = os.path.abspath(samples)
    only = read_list(a.list) if getattr(a, "list", None) else None
    settle = getattr(a, "settle", 0) or 0
    now = time.time()
    skip = {os.path.realpath(a.out)}
    groups = [""] + sorted(d for d in os.listdir(samples)
                           if d != "ref" and os.path.isdir(os.path.join(samples, d))
                           and os.path.realpath(os.path.join(samples, d)) not in skip)
    for g in groups:
        if a.group and g not in a.group:
            continue
        folder = os.path.join(samples, g)
        for f in sorted(os.listdir(folder)):
            stem, ext = os.path.splitext(f)
            dwg = os.path.join(folder, f)
            if ext.lower() != ".dwg" or not os.path.isfile(dwg):
                continue
            if only is not None and (f"{g}/{f}" if g else f) not in only:
                continue
            if settle and now - os.path.getmtime(dwg) < settle:  # still being written
                settling += 1
                continue
            ref = os.path.join(folder, "ref", stem + ".glb")
            if not os.path.exists(ref):
                no_ref += 1
                continue
            key = row_key(g, f)
            if a.selection_only and key not in sel_keys:
                continue
            items.append(dict(key=key, group=g, file=f, name=stem, dwg=dwg, ref=ref))
    if a.subset:
        items.sort(key=lambda it: sha16(it["key"]))
        items = sorted(items[:a.subset], key=lambda it: it["key"])
    return items, no_ref, settling


# --- convert and compare (worker processes) ------------------------------------------

_PLUGINS = None


def both_empty(res, job):
    """dwg2gltf exits 3 when a drawing has nothing to convert: not a failure when the reference is empty too.
    Applied after the comparison cache, so cached results are classified the same way."""
    if res.get("category") == "convert_failed" and res.get("convert_code") == 3 \
            and not len(compare.Glb(job["ref"]).tris):
        res = dict(res, status="skip", score=None, reasons="both_empty", category="both_empty",
                   categories="both_empty", error="")
    return res


def file_sha(path):
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def compare_cached(glb, job):
    """compare_glbs, cached by the GLB's content: a new binary that writes the same bytes for a drawing
    reuses the metrics of an earlier binary. compare_glbs only sees the two GLBs and is deterministic,
    so the cached result is the one it would compute."""
    path = os.path.join(job["glb_cmp"], f"{file_sha(glb)[:32]}-{job['ref_sha'][:16]}-{job['chash']}.json")
    if os.path.exists(path):
        try:
            return dict(json.load(open(path)), _reused=True)
        except ValueError:
            pass  # a half-written file from an interrupted run
    t0 = time.time()
    res = compare.compare_glbs(glb, job["ref"], _PLUGINS)
    res["compare_s"] = round(time.time() - t0, 2)
    write_json(path, res)
    return res


def work(job):
    global _PLUGINS
    try:
        if os.path.exists(job["cmp"]):
            res = json.load(open(job["cmp"]))
            res["_cached"] = True
            return both_empty(res, job)
        if _PLUGINS is None:
            _PLUGINS = compare.load_plugins(job["plugins"])
        glb, logp = job["glb"], job["glb"][:-4] + ".log"
        res = {}
        if not os.path.exists(glb):
            os.makedirs(os.path.dirname(glb), exist_ok=True)
            tmp = glb[:-4] + f".{os.getpid()}.tmp.glb"
            t0 = time.time()
            try:
                r = subprocess.run([job["bin"], job["dwg"], "-o", tmp, *job["opts"]], capture_output=True, text=True,
                                   timeout=job["timeout"])
                out, code = r.stdout + r.stderr, r.returncode
            except subprocess.TimeoutExpired:
                out, code = f"timeout after {job['timeout']}s", -9
            res["convert_s"] = round(time.time() - t0, 2)
            open(logp, "w").write(out)
            if code == 0 and os.path.exists(tmp):
                os.replace(tmp, glb)
            elif os.path.exists(tmp):
                os.remove(tmp)
            res["convert_code"] = code
        else:
            res["convert_code"] = 0
        out = open(logp).read() if os.path.exists(logp) else ""
        compare.parse_log(out, res)
        if not os.path.exists(glb):
            last = out.strip().splitlines()[-1][:300] if out.strip() else ""
            res.update(status="fail", score=0.0, reasons="convert_failed", category="convert_failed",
                       categories="convert_failed", error=last)
        else:
            res.update(compare_cached(glb, job))
            if "status" not in res:  # nothing converted
                cat = "series_dwg" if res.get("series_hint") else "empty"
                res.update(status="fail", score=0.0, reasons=cat, category=cat, categories=cat)
        write_json(job["cmp"], {k: v for k, v in res.items() if not k.startswith("_")})
        res = both_empty(res, job)
        if os.path.exists(glb) and not job["keep_all"] and (job["keep_none"] or (not job["keep"] and res["status"] in ("pass", "skip"))):
            os.remove(glb)  # flagged and selected drawings keep theirs; others are converted again if needed
        return res
    except Exception as e:
        import traceback
        return dict(status="error", score=0.0, reasons="compare_error", category="compare_error",
                    categories="compare_error", error=f"{type(e).__name__}: {e}", trace=traceback.format_exc()[-800:])


# --- renders ------------------------------------------------------------------------

def gltf_to_blender(b):
    """glTF (Y up) bounds -> Blender (Z up) bounds: Blender (x, y, z) = glTF (x, -z, y)."""
    lo, hi = b[:3], b[3:]
    return [lo[0], -hi[2], lo[1], hi[0], -lo[2], hi[1]]


def parse_bbox(v):
    if isinstance(v, list):
        return v
    try:
        return [float(x) for x in json.loads(v)] if v else None
    except (ValueError, TypeError):
        return None


def overlap(a, b):
    inter, va, vb = 1.0, 1.0, 1.0
    for i in range(3):
        inter *= max(0.0, min(a[3 + i], b[3 + i]) - max(a[i], b[i]))
        va *= max(a[3 + i] - a[i], 1e-6)
        vb *= max(b[3 + i] - b[i], 1e-6)
    return inter / max(va, vb)


def boxes(r, extra=()):
    """Camera boxes (Blender axes, rounded to 1 mm) for the reference and ours: the union of both,
    or each its own when they hardly overlap (different units, far apart)."""
    br, bo = parse_bbox(r.get("bbox_ref")), parse_bbox(r.get("bbox_ours"))
    if not br:
        return None, None
    br = gltf_to_blender(br)
    bo = gltf_to_blender(bo) if bo else None
    others = [gltf_to_blender(parse_bbox(x)) for x in extra if parse_bbox(x)]
    if bo and overlap(br, bo) > 0.3:
        u = [min(x) for x in zip(*[b[:3] for b in [br, bo] + others])] + [max(x) for x in zip(*[b[3:] for b in [br, bo] + others])]
        br = bo = u
    rnd = lambda b: [round(x, 3) for x in b] if b else None  # noqa: E731
    return rnd(br), rnd(bo)


class Renders:
    """Render cache: cache/renders/<mode>-<settings>/{ref,ours}/... .jpg"""

    def __init__(self, out, blender_ver):
        self.root = os.path.join(out, "cache", "renders")
        src = open(os.path.join(HERE, "render_pair.py"), "rb").read()
        self.settings = {m: sha16(json.dumps(RENDER[m], sort_keys=True) + blender_ver + hashlib.sha256(src).hexdigest())[:10]
                         for m in RENDER}
        self.jobs = {m: [] for m in RENDER}
        self.flatten = []

    def path(self, mode, side, view, box, ref_sha, ours_key=None):
        bh = sha16(json.dumps(box))[:10]
        base = os.path.join(self.root, f"{mode}-{self.settings[mode]}")
        if side == "ref":
            return os.path.join(base, "ref", f"{ref_sha[:16]}-{bh}-{view}.jpg")
        return os.path.join(base, "ours", ours_key, f"{bh}-{view}.jpg")

    def want(self, mode, glbs, renders):
        """glbs: {name: path}; renders: [(name, view, box, jpg)]. Queues what is missing."""
        todo = []
        for name, view, box, jpg in renders:
            if os.path.exists(jpg) or name not in glbs:
                continue
            out = jpg[:-4] + ".png" if mode == "present" else jpg
            if mode == "present":
                self.flatten.append((out, jpg))
            todo.append({"glb": name, "view": view, "box": box, "out": out})
        if todo:
            self.jobs[mode].append({"glbs": glbs, "renders": todo})

    def run(self, tmp_dir):
        from collage import flatten
        for mode, jobs in self.jobs.items():
            if not jobs:
                continue
            n = sum(len(j["renders"]) for j in jobs)
            spec = os.path.join(tmp_dir, f"render_{mode}.json")
            write_json(spec, dict(mode=mode, jobs=jobs, **RENDER[mode]))
            log(f"rendering {n} {mode} images in Blender")
            t0 = time.time()
            subprocess.run([BLENDER, "-b", "--factory-startup", "-P", os.path.join(HERE, "render_pair.py"), "--", spec],
                           stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, check=False)
            log(f"  {time.time() - t0:.0f}s")
        for png, jpg in self.flatten:
            if os.path.exists(png):
                flatten(png, jpg)
                os.remove(png)
        self.jobs = {m: [] for m in RENDER}
        self.flatten = []


def ours_key(binsha, optkey, dwg_sha):
    return os.path.join(f"{binsha[:16]}-{optkey}", dwg_sha[:24])


def pair_renders(R, mode, r, views, binsha, optkey):
    """Queue reference and our renders of a result row; returns {view: (ref_jpg, ours_jpg)}."""
    br, bo = boxes(r)
    if not br:
        return {}
    ok = bool(r.get("bbox_ours"))
    glbs = {"ref": r["ref"]}
    if ok and os.path.exists(r["glb"]):
        glbs["ours"] = r["glb"]  # only needed for renders that are not cached
    out, todo = {}, []
    for v in views:
        rj = R.path(mode, "ref", v, br, r["ref_sha"])
        oj = R.path(mode, "ours", v, bo, r["ref_sha"], ours_key(binsha, optkey, r["dwg_sha"])) if ok else ""
        todo.append(("ref", v, br, rj))
        if ok:
            todo.append(("ours", v, bo, oj))
        out[v] = (rj, oj)
    R.want(mode, glbs, todo)
    return out


def needs_glb(R, mode, r, views, binsha, optkey):
    """Our GLB is needed: a render of it is missing and the GLB was deleted."""
    _, bo = boxes(r)
    if not bo or os.path.exists(r["glb"]):
        return False
    k = ours_key(binsha, optkey, r["dwg_sha"])
    return any(not os.path.exists(R.path(mode, "ours", v, bo, r["ref_sha"], k)) for v in views)


def convert_only(job):
    tmp = job["glb"][:-4] + f".{os.getpid()}.tmp.glb"
    try:
        r = subprocess.run([job["bin"], job["dwg"], "-o", tmp, *job["opts"]], capture_output=True, timeout=job["timeout"])
        if r.returncode == 0 and os.path.exists(tmp):
            os.replace(tmp, job["glb"])
            return True
    except subprocess.TimeoutExpired:
        pass
    if os.path.exists(tmp):
        os.remove(tmp)
    return False


def pool_size(workers, gb_each=2.5):
    """Worker processes: at most `workers`, and no more than the available memory allows. The largest
    drawings need about 1.2 GB to convert and 2.3 GB to compare (3 million triangles)."""
    try:
        avail = next(int(l.split()[1]) for l in open("/proc/meminfo") if l.startswith("MemAvailable:")) / 2**20
    except (OSError, StopIteration, ValueError):
        return max(1, workers)
    return max(1, min(workers, int(avail / gb_each)))


def previous_seconds(out):
    """Seconds per drawing (convert + compare) from the newest run that recorded them, by key."""
    for j in reversed(runs(out)):
        path = os.path.join(out, "runs", j["run_id"], "results.csv") if "run_id" in j else None
        if not path or not os.path.exists(path):
            continue
        secs = {}
        with open(path, encoding="utf-8") as f:
            for r in csv.DictReader(f):
                t = 0.0
                for c in ("convert_s", "compare_s"):
                    try:
                        t += float(r.get(c) or 0)
                    except ValueError:
                        pass
                if t:
                    secs[row_key(r.get("group", r.get("folder", "")), r["file"])] = t
        if secs:
            return secs
    return {}


def ensure_glbs(rows, conv, opts, timeout=600, workers=6):
    """Convert again the GLBs of these rows with the run's binary (only if it is still the same file)."""
    rows = [r for r in rows if not os.path.exists(r["glb"])]
    if not rows:
        return 0
    path = conv.get("path", "")
    if not os.path.exists(path) or Hashes(os.devnull)(path) != conv["sha256"]:
        log(f"  {len(rows)} GLBs needed again, but the binary {path} is gone or changed; their renders are skipped")
        return 0
    log(f"  converting {len(rows)} GLBs again for renders")
    jobs = [dict(bin=path, dwg=r["dwg"], glb=r["glb"], opts=opts, timeout=timeout) for r in rows]
    for j in jobs:
        os.makedirs(os.path.dirname(j["glb"]), exist_ok=True)
    with ProcessPoolExecutor(pool_size(workers)) as ex:
        return sum(ex.map(convert_only, jobs))


def keep_set(rows, sel_keys):
    return {r["glb"] for r in rows if r["status"] in ("warn", "fail", "error") or r["key"] in sel_keys}


def retain(out, binkey, keep, cap_gb):
    """Delete this binary's GLBs outside `keep`, then cap the GLB cache at cap_gb, least recently
    used first (GLBs in `keep` last). Returns the bytes freed."""
    base = os.path.join(out, "cache", "ours")
    freed = 0
    own = os.path.join(base, binkey)
    for f in os.listdir(own) if os.path.isdir(own) else []:
        p = os.path.join(own, f)
        if f.endswith(".glb") and p not in keep:
            freed += os.path.getsize(p)
            os.remove(p)
    if cap_gb is None:
        return freed
    files = []
    for d in os.listdir(base) if os.path.isdir(base) else []:
        for f in os.listdir(os.path.join(base, d)):
            if f.endswith(".glb"):
                p = os.path.join(base, d, f)
                st = os.stat(p)
                files.append((p in keep, st.st_mtime, st.st_size, p))
    total = sum(x[2] for x in files)
    for kept, _, size, p in sorted(files):
        if total <= cap_gb * 1e9:
            break
        if kept:
            log(f"  GLB cache over {cap_gb:g} GB with flagged/selected GLBs of this run alone; they stay")
            break
        os.remove(p)
        total -= size
        freed += size
    return freed


# --- run ------------------------------------------------------------------------------

def selection_path(a):
    return os.path.abspath(a.selection) if a.selection else os.path.join(os.path.abspath(a.out), "selection.json")


def load_selection(path):
    try:
        return json.load(open(path))
    except (OSError, ValueError):
        return {"collage": [], "slides": [], "shots": []}


def selection_keys(sel):
    return {x["key"] for k in ("collage", "slides", "shots") for x in sel.get(k, [])}


def cmd_run(a):
    t_start = time.time()
    out = os.path.abspath(a.out)
    os.makedirs(out, exist_ok=True)
    free = shutil.disk_usage(out).free / 1e9
    if free < a.min_free_gb:
        log(f"only {free:.1f} GB free on {out}; a run needs at least {a.min_free_gb:g} GB "
            f"(free space, or `tools/qa/qa.py prune`, or --min-free-gb)")
        return 2
    timings = {}
    hashes = Hashes(os.path.join(out, "cache", "sha256.json"))
    conv = resolve_converter(a, hashes)
    binsha = conv["sha256"]
    opts = list(a.opt or [])
    optkey = sha16(" ".join(opts))[:8] if opts else "default"
    run_id = f"{time.strftime('%Y%m%d-%H%M%S')}-{conv['commit']}{'-dirty' if conv.get('dirty') else ''}-{binsha[:8]}"
    run_dir = os.path.join(out, "runs", run_id)
    os.makedirs(run_dir)
    log(f"run {run_id}\n  converter {conv['path']} ({conv.get('version', '')})")
    sel = load_selection(selection_path(a))
    sel_keys = selection_keys(sel)
    items, no_ref, settling = scan(a.samples, a, sel_keys)
    limited = bool(a.subset or a.group or a.selection_only or a.skip_render or a.list or settling)
    log(f"{len(items)} DWGs with a reference ({no_ref} without one)"
        + (f", {settling} skipped: modified in the last {a.settle:g} s" if settling else ""))
    group_names = read_group_names(a.group_names)
    t0 = time.time()
    for it in items:
        it["dwg_sha"], it["ref_sha"] = hashes(it["dwg"]), hashes(it["ref"])
    hashes.save()
    timings["hash"] = round(time.time() - t0, 1)

    # convert + compare
    plugins = [os.path.abspath(p) for p in (a.plugins or [])]
    chash = compare.code_hash(plugins)
    t0 = time.time()
    jobs = []
    for it in items:
        it["glb"] = os.path.join(out, "cache", "ours", ours_key(binsha, optkey, it["dwg_sha"]) + ".glb")
        cmp = os.path.join(out, "cache", "compare", f"{binsha[:16]}-{optkey}",
                           f"{it['dwg_sha'][:24]}-{it['ref_sha'][:16]}-{chash}.json")
        jobs.append(dict(dwg=it["dwg"], ref=it["ref"], glb=it["glb"], cmp=cmp, bin=conv["path"], opts=opts,
                         timeout=a.timeout, plugins=plugins, keep=it["key"] in sel_keys, keep_all=a.keep_glbs == "all",
                         keep_none=a.keep_glbs == "none", ref_sha=it["ref_sha"], chash=chash,
                         glb_cmp=os.path.join(out, "cache", "compare-glb")))
    os.makedirs(os.path.join(out, "cache", "compare-glb"), exist_ok=True)
    # Slowest drawings first (times of the previous run, else file size), so a long conversion does not
    # start last and leave the other workers idle at the end.
    prev = previous_seconds(out)
    order = sorted(range(len(jobs)), key=lambda i: (-prev.get(items[i]["key"], 0.0), -os.path.getsize(items[i]["dwg"])))
    workers = pool_size(a.workers)
    log(f"  {workers} workers" + (f" (memory limits --workers {a.workers})" if workers < a.workers else ""))
    rows, cached, reused = [], 0, 0
    with ProcessPoolExecutor(workers) as ex:
        futs = {ex.submit(work, jobs[i]): items[i] for i in order}
        for k, fu in enumerate(as_completed(futs), 1):
            it = futs[fu]
            res = fu.result()
            cached += bool(res.pop("_cached", False))
            reused += bool(res.pop("_reused", False))
            row = dict(it)
            row.update(res)
            row.update(run_id=run_id, commit=conv["commit"])
            rows.append(row)
            if k % 50 == 0 or k == len(jobs):
                log(f"  compared {k}/{len(jobs)} ({cached} cached, {reused} same GLB as before) {time.time() - t0:.0f}s")
    rows.sort(key=lambda r: (r.get("score") if isinstance(r.get("score"), (int, float)) else -1, r["key"]))
    timings["compare"] = round(time.time() - t0, 1)
    with open(os.path.join(run_dir, "results.csv"), "w", newline="", encoding="utf-8") as f:
        extra = sorted({k for r in rows for k in r if k not in COLUMNS and not k.startswith("_")
                        and k not in ("key", "trace")})  # plugin columns
        w = csv.DictWriter(f, COLUMNS + extra, extrasaction="ignore")
        w.writeheader()
        for r in rows:
            w.writerow({k: (f"{v:.5g}" if isinstance(v, float) else json.dumps(v) if isinstance(v, list) else v)
                        for k, v in r.items()})
    counts = {}
    for r in rows:
        counts[r["status"]] = counts.get(r["status"], 0) + 1
    log("  " + ", ".join(f"{k} {v}" for k, v in sorted(counts.items())))

    blender_ver = blender_version()
    info = {"run_id": run_id, "converter": f"{conv['commit']}{' (dirty)' if conv.get('dirty') else ''} sha256 {binsha[:12]}",
            "date": time.strftime("%Y-%m-%d"), "compared": len(rows),
            "ref_generator": next((r.get("ref_generator") for r in rows if r.get("ref_generator")), "")}
    for r in rows:  # columns as text, the way results.csv has them
        for k in ("bbox_ours", "bbox_ref"):
            if isinstance(r.get(k), list):
                r[k] = json.dumps(r[k])
        for k, v in list(r.items()):
            if isinstance(v, bool):
                r[k] = str(v)
            elif isinstance(v, float):
                r[k] = f"{v:.5g}"
    by_key = {r["key"]: r for r in rows}
    sel_missing = sorted(k for k in sel_keys if k not in by_key) if not limited else []
    index = {}
    if not a.skip_render:
        import report
        import collage
        R = Renders(out, blender_ver)
        t0 = time.time()
        bad, show = report.select(rows, a.max_report, a.four_views, a.showcase)
        present = []
        if not a.skip_collage:
            for r in rows:
                r["_group"] = group_names.get(r["group"]) or collage.nice_slug(r["group"])
            lists = {k: [by_key[x["key"]] for x in sel.get(k, []) if x["key"] in by_key] for k in ("collage", "slides", "shots")}
            fails = collage.failures(rows, a.failures)
            present = list({r["key"]: r for r in lists["collage"] + lists["slides"] + lists["shots"] + fails}.values())
        need = [r for r in bad + show if needs_glb(R, "eng", r, r["_views"], binsha, optkey)]
        need += [r for r in present if needs_glb(R, "present", r, ["front34"], binsha, optkey)]
        ensure_glbs(list({r["key"]: r for r in need}.values()), conv, opts, a.timeout, a.workers)
        for r in bad + show:
            r["_imgs"] = pair_renders(R, "eng", r, r["_views"], binsha, optkey)
        for r in present:
            p = pair_renders(R, "present", r, ["front34"], binsha, optkey)
            r["_ref_img"], r["_ours_img"] = p.get("front34", ("", ""))
        R.run(run_dir)
        timings["render"] = round(time.time() - t0, 1)
        t0 = time.time()
        report.build(run_dir, rows, bad, show, os.path.join(out, "cache", "diff"), info, group_names)
        timings["report"] = round(time.time() - t0, 1)
        if not a.skip_collage:
            t0 = time.time()
            collage.write_all(run_dir, lists["collage"], lists["slides"], lists["shots"],
                              [r for r in fails if r.get("_ours_img") or r.get("_ref_img")], info)
            timings["collage"] = round(time.time() - t0, 1)
        rel = lambda p: os.path.relpath(p, out) if p and os.path.exists(p) else ""  # noqa: E731
        for r in rows:
            e = {}
            if r.get("_ref_img") or r.get("_ours_img"):
                e["present"] = {"ref": rel(r.get("_ref_img")), "ours": rel(r.get("_ours_img"))}
            if r.get("_imgs"):
                e["eng"] = {v: {"ref": rel(x[0]), "ours": rel(x[1])} for v, x in r["_imgs"].items()}
            shot = os.path.join(run_dir, "shots", collage.shot_name(r))
            if os.path.exists(shot):
                e["shot"] = rel(shot)
            if e:
                e.update(group=r["group"], name=r["name"], status=r["status"],
                         score=r["score"], category=r.get("category", ""))
                index[r["key"]] = e
    if a.keep_glbs != "all":
        none = a.keep_glbs == "none"
        freed = retain(out, f"{binsha[:16]}-{optkey}", set() if none else keep_set(rows, sel_keys),
                       None if none else float(a.keep_glbs))
        log(f"GLB cache: freed {freed / 1e9:.2f} GB (keeping flagged and selected, cap {a.keep_glbs} GB)")
    write_json(os.path.join(run_dir, "renders_index.json"),
               {"run_id": run_id, "generated": time.strftime("%Y-%m-%dT%H:%M:%S"), "samples": os.path.abspath(a.samples),
                "paths_relative_to": out, "count": len(index), "items": index})
    timings["total"] = round(time.time() - t_start, 1)
    import numpy
    import scipy
    try:
        import skimage
        skv = skimage.__version__
    except Exception:
        skv = ""
    run = {"run_id": run_id, "started": time.strftime("%Y-%m-%dT%H:%M:%S", time.localtime(t_start)),
           "finished": time.strftime("%Y-%m-%dT%H:%M:%S"), "converter": conv, "options": opts, "option_key": optkey,
           "blender": blender_ver, "python": sys.version.split()[0],
           "libs": {"numpy": numpy.__version__, "scipy": scipy.__version__, "scikit-image": skv},
           "settings": {"thresholds": compare.THRESHOLDS, "metric_code": chash, "plugins": plugins,
                        "render": RENDER, "samples_per_side": compare.N_QUERY, "seed": 1,
                        "note": "Cycles seed 0; OptiX denoising is not bit-identical across drivers"},
           "scope": {"subset": a.subset, "group": a.group, "selection_only": a.selection_only,
                     "list": os.path.abspath(a.list) if a.list else None, "settle_s": a.settle,
                     "settling": settling,
                     "limited": limited, "skip_render": a.skip_render, "skip_collage": a.skip_collage},
           "samples": {"path": os.path.abspath(a.samples), "compared": len(rows), "without_reference": no_ref,
                       "from_cache": cached},
           "counts": counts, "selection_missing": sel_missing, "timings_s": timings}
    write_json(os.path.join(run_dir, "run.json"), run)
    if not limited or a.set_latest:
        link = os.path.join(out, "latest")
        tmp = link + ".tmp"
        if os.path.lexists(tmp):
            os.remove(tmp)
        os.symlink(os.path.join("runs", run_id), tmp)
        os.replace(tmp, link)
        if index:
            shutil.copyfile(os.path.join(run_dir, "renders_index.json"), os.path.join(out, "renders_index.json"))
    log(f"done in {timings['total']:.0f}s: {run_dir}")
    return 0


# --- diff -----------------------------------------------------------------------------

def runs(out):
    rs = []
    for d in os.listdir(os.path.join(out, "runs")) if os.path.isdir(os.path.join(out, "runs")) else []:
        try:
            j = json.load(open(os.path.join(out, "runs", d, "run.json")))
            rs.append(j)
        except (OSError, ValueError):
            pass
    return sorted(rs, key=lambda j: j["started"])


def resolve_run(out, name):
    if name in (None, "latest"):
        p = os.path.realpath(os.path.join(out, "latest"))
    elif os.path.isdir(name):
        p = os.path.abspath(name)
    else:
        p = os.path.join(out, "runs", name)
    return json.load(open(os.path.join(p, "run.json"))), p


def load_rows(run_dir):
    with open(os.path.join(run_dir, "results.csv"), encoding="utf-8") as f:
        rows = {}
        for r in csv.DictReader(f):
            if "group" not in r:  # older results.csv files call the column "folder"
                r["group"] = r.get("folder") or ""
            r["key"] = row_key(r["group"], r["file"])
            rows[r["key"]] = r
        return rows


def cmd_diff(a):
    out = os.path.abspath(a.out)
    rb, db = resolve_run(out, a.b)
    if a.a:
        ra, da = resolve_run(out, a.a)
    else:
        prev = [j for j in runs(out) if j["started"] < rb["started"] and not j["scope"].get("limited")]
        other = [j for j in prev if j["converter"]["sha256"] != rb["converter"]["sha256"]]
        if not (other or prev):
            log("no earlier run to compare with")
            return 2
        ra = (other or prev)[-1]
        da = os.path.join(out, "runs", ra["run_id"])
    A, B = load_rows(da), load_rows(db)
    f = lambda v: compare_float(v)  # noqa: E731
    changes, regress, fixed, inputs = [], [], [], []
    for k in sorted(set(A) & set(B)):
        x, y = A[k], B[k]
        if x.get("dwg_sha") != y.get("dwg_sha") or x.get("ref_sha") != y.get("ref_sha"):
            inputs.append(k)
            continue
        ds = (f(y["score"]) or 0) - (f(x["score"]) or 0)
        dr = STATUS_RANK.get(y["status"], 2) - STATUS_RANK.get(x["status"], 2)
        deltas = {m: (f(x.get(m)), f(y.get(m))) for m in DIFF_METRICS}
        c = dict(key=k, a=x, b=y, dscore=ds, drank=dr, deltas=deltas)
        if dr > 0 or ds < -a.score_drop:
            regress.append(c)
        elif dr < 0 or ds > a.score_drop:
            fixed.append(c)
        if dr or abs(ds) > 0.05 or x.get("categories") != y.get("categories"):
            changes.append(c)
    only_a, only_b = sorted(set(A) - set(B)), sorted(set(B) - set(A))
    name = f"{ra['run_id']}__{rb['run_id']}"
    ddir = os.path.join(out, "diffs", name)
    os.makedirs(os.path.join(ddir, "img"), exist_ok=True)

    # old ours | new ours | reference for the biggest changes
    show = sorted(regress, key=lambda c: c["dscore"]) + sorted(fixed, key=lambda c: -c["dscore"])
    show = show[:a.render]
    if show and not a.skip_render:
        R = Renders(out, blender_version())
        ka = f"{ra['converter']['sha256'][:16]}-{ra.get('option_key', 'default')}"
        kb = f"{rb['converter']['sha256'][:16]}-{rb.get('option_key', 'default')}"
        for c in show:
            x, y = c["a"], c["b"]
            br, _ = boxes(dict(y, bbox_ours=y.get("bbox_ours") or x.get("bbox_ours")), extra=[x.get("bbox_ours")])
            if not br:
                continue
            glbs, todo = {"ref": y["ref"]}, []
            c["imgs"] = {"ref": R.path("eng", "ref", "front34", br, y["ref_sha"])}
            todo.append(("ref", "front34", br, c["imgs"]["ref"]))
            for tag, r, key, rn in (("old", x, ka, ra), ("new", y, kb, rb)):
                glb = os.path.join(out, "cache", "ours", key, y["dwg_sha"][:24] + ".glb")
                if not os.path.exists(glb) and r.get("bbox_ours"):
                    ensure_glbs([dict(r, glb=glb)], rn["converter"], rn.get("options", []))
                    c.setdefault("_temp", []).append(glb)
                if os.path.exists(glb) and r.get("bbox_ours"):
                    glbs[tag] = glb
                    c["imgs"][tag] = R.path("eng", "ours", "front34", br, y["ref_sha"], os.path.join(key, y["dwg_sha"][:24]))
                    todo.append((tag, "front34", br, c["imgs"][tag]))
            R.want("eng", glbs, todo)
        R.run(ddir)
        for c in show:
            for g in c.get("_temp", []):
                if os.path.exists(g):
                    os.remove(g)
    from report import thumb
    e = html.escape
    h = [f"""<!doctype html><html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>dwg2gltf run diff</title><style>
:root{{--bg:#f6f6f4;--fg:#1d1d1f;--mut:#6b6b70;--line:#dcdcd8;--bad:#b42318;--good:#2f7d4a;--card:#fff}}
@media (prefers-color-scheme: dark){{:root{{--bg:#17171a;--fg:#ececee;--mut:#9a9aa2;--line:#33333a;--card:#202024}}}}
body{{background:var(--bg);color:var(--fg);font:14px/1.45 "Noto Sans","Liberation Sans",system-ui,sans-serif;margin:0;padding:24px 16px}}
main{{max-width:1400px;margin:0 auto}} h1{{font-size:22px}} h2{{font-size:17px;margin-top:28px}} .mut{{color:var(--mut)}}
table{{border-collapse:collapse}} td,th{{padding:3px 10px 3px 0;text-align:left;border-bottom:1px solid var(--line);vertical-align:top}}
.bad{{color:var(--bad)}} .good{{color:var(--good)}} .row{{display:grid;grid-template-columns:repeat(3,minmax(0,220px)) 1fr;gap:8px;
background:var(--card);border:1px solid var(--line);border-radius:6px;padding:10px;margin:10px 0}} .row img{{width:100%;display:block}}
.cap{{font-size:11px;color:var(--mut);text-align:center}} .m{{font:12px/1.5 ui-monospace,monospace;overflow-wrap:anywhere}}
@media (max-width:820px){{.row{{grid-template-columns:1fr 1fr 1fr}} .row .m{{grid-column:1/4}}}}</style></head><body><main>
<h1>Run diff</h1><p class="mut">A: {e(ra['run_id'])} (converter {e(ra['converter']['commit'])}, sha256 {e(ra['converter']['sha256'][:12])})<br>
B: {e(rb['run_id'])} (converter {e(rb['converter']['commit'])}, sha256 {e(rb['converter']['sha256'][:12])})</p>
<table><tr><th>drawings in both</th><td>{len(set(A) & set(B))}</td></tr>
<tr><th class="bad">regressions</th><td>{len(regress)} (status worse or score down more than {a.score_drop:g})</td></tr>
<tr><th class="good">fixed / improved</th><td>{len(fixed)}</td></tr><tr><th>any change</th><td>{len(changes)}</td></tr>
<tr><th>input changed (DWG or reference), not judged</th><td>{len(inputs)}</td></tr>
<tr><th>only in A / only in B</th><td>{len(only_a)} / {len(only_b)}</td></tr>
<tr><th>status counts A / B</th><td>{e(json.dumps(ra.get('counts')))} / {e(json.dumps(rb.get('counts')))}</td></tr></table>"""]

    def block(title, cs, cls):
        h.append(f"<h2 class='{cls}'>{e(title)} ({len(cs)})</h2>")
        for c in cs:
            x, y = c["a"], c["b"]
            imgs = ""
            for tag, cap in (("old", "ours, A"), ("new", "ours, B"), ("ref", "reference")):
                p = c.get("imgs", {}).get(tag)
                if p and os.path.exists(p):
                    dst = os.path.join(ddir, "img", f"{sha16(c['key'])}_{tag}.jpg")
                    thumb(p, dst, 360)
                    imgs += f'<div><img loading="lazy" src="img/{os.path.basename(dst)}" alt=""><div class="cap">{cap}</div></div>'
                else:
                    imgs += f'<div class="cap">{cap}: no render</div>'
            met = " &middot; ".join(f"{m} {fmt(va)} &rarr; {fmt(vb)}" for m, (va, vb) in c["deltas"].items()
                                    if va != vb and not (va is None and vb is None))
            h.append(f'<div class="row">{imgs}<div class="m"><b>{e(c["key"])}</b><br>{e(x["status"])} {e(x["score"])} [{e(x.get("categories", ""))}] &rarr; '
                     f'<b>{e(y["status"])} {e(y["score"])}</b> [{e(y.get("categories", ""))}]<br>{met}</div></div>')

    block("Regressions", sorted(regress, key=lambda c: c["dscore"]), "bad")
    block("Fixed or improved", sorted(fixed, key=lambda c: -c["dscore"]), "good")
    rest = [c for c in changes if c not in regress and c not in fixed]
    h.append(f"<h2>Other changes ({len(rest)})</h2><table><tr><th>drawing</th><th>A</th><th>B</th><th>score</th><th>categories A &rarr; B</th></tr>")
    for c in sorted(rest, key=lambda c: c["dscore"]):
        h.append(f"<tr><td>{e(c['key'])}</td><td>{e(c['a']['status'])}</td><td>{e(c['b']['status'])}</td>"
                 f"<td>{c['dscore']:+.1f}</td><td>{e(c['a'].get('categories', ''))} &rarr; {e(c['b'].get('categories', ''))}</td></tr>")
    h.append("</table>")
    if inputs:
        h.append(f"<h2>Input changed ({len(inputs)})</h2><p class='mut'>" + ", ".join(e(k) for k in inputs) + "</p>")
    h.append("</main></body></html>")
    open(os.path.join(ddir, "index.html"), "w", encoding="utf-8").write("\n".join(h))
    write_json(os.path.join(ddir, "diff.json"), {
        "a": ra["run_id"], "b": rb["run_id"], "regressions": [c["key"] for c in regress],
        "fixed": [c["key"] for c in fixed], "changed": [c["key"] for c in changes], "input_changed": inputs,
        "only_a": only_a, "only_b": only_b})
    log(f"{len(regress)} regressions, {len(fixed)} fixed, {len(changes)} changed: {os.path.join(ddir, 'index.html')}")
    return 1 if len(regress) > a.max_regressions else 0


def compare_float(v):
    try:
        return float(v)
    except (TypeError, ValueError):
        return None


def fmt(v):
    return "-" if v is None else f"{v:.4g}"


# --- select ---------------------------------------------------------------------------

def cmd_select(a):
    """Choose the drawings of the collage sheets, slides and shots from a run with renders and write
    the selection file (keep it between runs, so the sheets stay comparable between versions)."""
    import collage
    out = os.path.abspath(a.out)
    run, rdir = resolve_run(out, a.run)
    rows = list(load_rows(rdir).values())
    binsha, optkey = run["converter"]["sha256"], run.get("option_key", "default")
    cand = collage.pick(rows, int(a.count * 1.4) + 4)
    R = Renders(out, blender_version())
    need = [r for r in cand if needs_glb(R, "present", r, ["front34"], binsha, optkey)]
    ensure_glbs(need, run["converter"], run.get("options", []))
    for r in cand:
        p = pair_renders(R, "present", r, ["front34"], binsha, optkey)
        r["_ref_img"], r["_ours_img"] = p.get("front34", ("", ""))
    R.run(rdir)
    for r in need:  # converted again only for these renders
        if os.path.exists(r["glb"]):
            os.remove(r["glb"])
    ok = []
    for r in cand:
        if all(r[k] and os.path.exists(r[k]) for k in ("_ref_img", "_ours_img")) and min(
                collage.coverage(r["_ref_img"]), collage.coverage(r["_ours_img"])) > 0.04:
            r["_ssim"] = collage.pair_ssim(r["_ref_img"], r["_ours_img"])
            if r["_ssim"] >= a.min_ssim:
                ok.append(r)
    chosen = collage.interleave(ok[:a.count])
    heroes, used = [], set()
    pool = sorted((r for r in chosen if r["_ssim"] >= 0.96), key=lambda r: -r["_appeal"])
    for r in pool:
        if len(heroes) < a.slides and r["group"] not in used:
            heroes.append(r)
            used.add(r["group"])
    shots, groups = [], set()
    for rnd in range(3):
        for r in sorted(chosen, key=lambda r: -r["_appeal"]):
            if len(shots) < a.shots and r not in shots and not (rnd == 0 and r["group"] in groups):
                shots.append(r)
                groups.add(r["group"])
    write_selection(selection_path(a), [("collage", chosen), ("slides", heroes), ("shots", shots)],
                    f"run {run['run_id']}: passing drawings, one per group first, presentation "
                    f"renders at least {a.min_ssim} SSIM alike (slides 0.96)")
    return 0


def write_selection(path, lists, source):
    doc = {"generated": time.strftime("%Y-%m-%d"), "source": source}
    for name, rs in lists:
        doc[name] = [{"key": r["key"], "dwg_sha": r.get("dwg_sha", ""), "name": r.get("name", "")} for r in rs]
    write_json(path, doc)
    log(f"{path}: " + ", ".join(f"{n} {len(rs)}" for n, rs in lists))


def run_rows(rdir):
    return list(load_rows(rdir).values())


# --- prune ----------------------------------------------------------------------------

def cmd_prune(a):
    """Apply the GLB policy to the whole cache: GLBs of binaries beyond the last --keep go; of the
    others only flagged and selected drawings stay (from each binary's last run), then the cap.
    Reference GLBs, comparisons and renders are kept."""
    out = os.path.abspath(a.out)
    sel_keys = selection_keys(load_selection(selection_path(a)))
    latest = {}
    for j in runs(out):
        latest[f"{j['converter']['sha256'][:16]}-{j.get('option_key', 'default')}"] = j
    order = sorted(latest, key=lambda k: latest[k]["started"], reverse=True)
    base = os.path.join(out, "cache", "ours")
    freed = 0
    for d in os.listdir(base) if os.path.isdir(base) else []:
        p = os.path.join(base, d)
        if d not in order[:a.keep]:
            size = sum(os.path.getsize(os.path.join(p, f)) for f in os.listdir(p))
            if not a.dry_run:
                shutil.rmtree(p)
            freed += size
            log(f"{'would delete' if a.dry_run else 'deleted'} {p} ({size / 1e9:.2f} GB)")
            continue
        rows = run_rows(os.path.join(out, "runs", latest[d]["run_id"]))
        keep = {os.path.join(p, r["dwg_sha"][:24] + ".glb") for r in rows if r["status"] in ("warn", "fail", "error")
                or r["key"] in sel_keys}
        for f in os.listdir(p):
            q = os.path.join(p, f)
            if f.endswith(".glb") and q not in keep:
                freed += os.path.getsize(q)
                if not a.dry_run:
                    os.remove(q)
    if not a.dry_run and a.cap_gb != "all":
        freed += retain(out, "", set(), float(a.cap_gb))
    log(f"{'would free' if a.dry_run else 'freed'} {freed / 1e9:.2f} GB")
    return 0


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    sel_help = "selection file (default: <out>/selection.json)"
    r = sub.add_parser("run", help="convert, compare, render, report")
    r.add_argument("--samples", default=SAMPLES,
                   help="folder of DWGs, optionally grouped in top-level subfolders; the reference of "
                        "<dir>/<stem>.dwg is <dir>/ref/<stem>.glb (default: samples/)")
    r.add_argument("--out", default=OUT, help="runs, cache and diffs (default: samples/compare/)")
    r.add_argument("--bin", help="use this dwg2gltf instead of building target/release/dwg2gltf")
    r.add_argument("--commit", help="commit label for --bin (default: git, when the binary is in this repository)")
    r.add_argument("--opt", action="append", help="extra dwg2gltf option (part of the cache key); repeatable")
    r.add_argument("--subset", type=int, default=0, help="only N DWGs (a fixed sample: lowest hash of the path)")
    r.add_argument("--group", nargs="*", help="only these top-level subfolders")
    r.add_argument("--list", help="only the DWGs listed in this file (paths relative to --samples, one per line)")
    r.add_argument("--settle", type=float, default=0, metavar="SECONDS",
                   help="skip DWGs modified in the last SECONDS (a folder still being filled)")
    r.add_argument("--group-names", metavar="TSV",
                   help="display names of groups for the report and collages (columns: group, display name)")
    r.add_argument("--selection", help=sel_help)
    r.add_argument("--selection-only", action="store_true", help="only the drawings in the selection file")
    r.add_argument("--skip-render", action="store_true", help="no renders, report images or collages")
    r.add_argument("--skip-collage", action="store_true", help="no collages, slides or shots")
    r.add_argument("--set-latest", action="store_true", help="point latest at a limited run too")
    r.add_argument("--plugins", nargs="*", help="metric plugin files (each defines METRICS = [fn(ctx, res)])")
    r.add_argument("--workers", type=int, default=os.cpu_count() or 4,
                   help="parallel convert+compare processes (default: all cores, fewer if memory is short)")
    r.add_argument("--timeout", type=int, default=600, help="seconds per conversion")
    r.add_argument("--max-report", type=int, default=400, help="most warn/fail files rendered in the report")
    r.add_argument("--four-views", type=int, default=40, help="worst files rendered from four directions")
    r.add_argument("--showcase", type=int, default=30, help="passing files in the report, one per group")
    r.add_argument("--failures", type=int, default=20, help="drawings on the failures sheet")
    r.add_argument("--min-free-gb", type=float, default=12)
    r.add_argument("--keep-glbs", default="4",
                   help="GLB cache cap in GB (flagged and selected drawings only), all, or none (renders convert again)")
    d = sub.add_parser("diff", help="compare two runs; exit 1 on regressions")
    d.add_argument("a", nargs="?", help="older run (default: the last full run with another binary)")
    d.add_argument("b", nargs="?", default="latest")
    d.add_argument("--out", default=OUT)
    d.add_argument("--score-drop", type=float, default=10)
    d.add_argument("--max-regressions", type=int, default=0)
    d.add_argument("--render", type=int, default=40, help="changed drawings rendered old | new | reference")
    d.add_argument("--skip-render", action="store_true")
    s = sub.add_parser("select", help="write the selection file from a run")
    s.add_argument("--run", default="latest")
    s.add_argument("--out", default=OUT)
    s.add_argument("--selection", help=sel_help)
    s.add_argument("--count", type=int, default=112)
    s.add_argument("--slides", type=int, default=6)
    s.add_argument("--shots", type=int, default=50)
    s.add_argument("--min-ssim", type=float, default=0.93)
    p = sub.add_parser("prune", help="apply the GLB retention policy to the cache")
    p.add_argument("--out", default=OUT)
    p.add_argument("--selection", help=sel_help)
    p.add_argument("--keep", type=int, default=2, help="binaries whose flagged/selected GLBs are kept")
    p.add_argument("--cap-gb", default="4", help="GLB cache cap in GB, or all")
    p.add_argument("--dry-run", action="store_true")
    a = ap.parse_args()
    sys.exit({"run": cmd_run, "diff": cmd_diff, "select": cmd_select, "prune": cmd_prune}[a.cmd](a))


if __name__ == "__main__":
    main()
