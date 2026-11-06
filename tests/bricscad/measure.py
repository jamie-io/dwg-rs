#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
"""Convert the corpus with a dwg2gltf binary and compare it with the right
answers in expected.json, and each shape's versions with each other.

    python3 tests/bricscad/measure.py --root tests/bricscad/dwg/v27 \\
        --binary PATH --commit HASH [--family NAME]

Files are converted the way users convert them, in the drawing's own units:
the converter's tolerances depend on the units. The GLB holds meters.
"""

import argparse
import hashlib
import json
import re
import struct
import subprocess
import tempfile
from dataclasses import dataclass
from pathlib import Path

from glb import Mesh, read_mesh

MM_PER_METER = 1000.0
TIMEOUT = 120.0
# faceting makes a mesh a little different from the exact shape
BOUNDS_MM = 0.2
BOUNDS_OF_DIAGONAL = 1e-4
AREA_SHARE = 0.01
VOLUME_SHARE = 0.01
# the converter's chord deviation at 1 mm per unit: no point of the mesh is
# further than this from the surface, so a thin part may lose more volume
# than VOLUME_SHARE says
DEVIATION_MM = 0.1
# a part thinner than this many deviations is too fine for its area and
# volume to mean anything: only its bounds are checked, and the report says so
FINE_DEVIATIONS = 10
FACING_SHARE = 0.999
SKIPPED = re.compile(r"^\s*skipped: (.+)$", re.M)
WARNING = re.compile(r"recoverable decode errors")


@dataclass
class Measured:
    entry: dict
    area: float | None
    volume: float | None
    bounds: list | None
    mesh: Mesh | None


@dataclass
class Converted:
    mesh: Mesh | None
    stats: str
    error: str | None


def measured(entry, mesh):
    """`mesh`'s values in millimeters."""
    if mesh is None or not mesh.triangles:
        return Measured(entry, None, None, None, mesh)
    lo, hi = mesh.bounds()
    bounds = [[v * MM_PER_METER for v in lo], [v * MM_PER_METER for v in hi]]
    return Measured(entry, mesh.area() * MM_PER_METER**2, mesh.volume() * MM_PER_METER**3, bounds, mesh)


def _is_fine(result):
    """Whether a part is too thin for its area and volume to mean anything
    at the converter's tolerance."""
    thinnest = min(hi - lo for lo, hi in zip(*result.bounds))
    return 0 < thinnest < FINE_DEVIATIONS * DEVIATION_MM


def result_label(problems, result):
    if problems:
        return "differs"
    return "bounds only" if _is_fine(result) else "match"


def _bounds_tolerance(bounds):
    diagonal = sum((bounds[1][k] - bounds[0][k]) ** 2 for k in range(3)) ** 0.5
    return BOUNDS_MM + BOUNDS_OF_DIAGONAL * diagonal


def _bounds_differ(a, b):
    tolerance = _bounds_tolerance(b)
    return any(abs(x - y) > tolerance for x, y in zip(a[0] + a[1], b[0] + b[1]))


def _share_differs(a, b, share):
    return abs(a - b) > share * abs(b)


def _rounded(bounds):
    return [[round(v, 3) for v in corner] for corner in bounds]


def check_file(entry, mesh, stats):
    """What is wrong with one conversion; empty means it matches."""
    problems = [f"skipped: {line.strip()}" for line in SKIPPED.findall(stats)]
    result = measured(entry, mesh)
    if result.area is None:
        return problems + ["no triangles"]
    if entry.get("bounds") and _bounds_differ(result.bounds, entry["bounds"]):
        problems.append(f"bounds {_rounded(result.bounds)}, expected {_rounded(entry['bounds'])}")
    fine = _is_fine(result)
    if entry.get("area") and not fine and _share_differs(result.area, entry["area"], AREA_SHARE):
        problems.append(f"area {result.area:.1f}, expected {entry['area']:.1f}")
    closed = entry.get("closed", True)
    surface = entry.get("area") or result.area
    allowance = VOLUME_SHARE * abs(entry.get("volume") or 0) + DEVIATION_MM * surface
    if closed and entry.get("volume") and not fine and abs(result.volume - entry["volume"]) > allowance:
        problems.append(f"volume {result.volume:.1f}, expected {entry['volume']:.1f}")
    facing = entry.get("facing")
    if facing == "outward":
        if result.volume <= 0:
            problems.append(f"not facing outward: volume {result.volume:.1f}")
    elif facing:
        share = mesh.facing_share(tuple(facing))
        if share < FACING_SHARE:
            problems.append(f"facing: {share:.3f} of the area faces {facing}")
    return problems


def _same(a, b):
    return not (
        _bounds_differ(a.bounds, b.bounds)
        or _share_differs(a.area, b.area, AREA_SHARE)
        or (a.entry.get("closed", True) and abs(a.volume - b.volume) > VOLUME_SHARE * abs(b.volume) + DEVIATION_MM * b.area)
    )


def compare_versions(results):
    """The versions of one shape that differ from the others. Only files
    that converted and that BricsCAD held unchanged take part."""
    usable = {c: r for c, r in results.items() if r.entry["status"] == "ok" and r.area is not None}
    if len(usable) < 2:
        return []
    # the reference is a version with the right answer, if any has it, else
    # the one most others agree with: wrong versions can be the majority
    def standing(code):
        result = usable[code]
        right = not check_file(result.entry, result.mesh, "")
        return (right, sum(_same(result, r) for r in usable.values()))

    reference = max(usable, key=standing)
    problems = []
    for code, result in sorted(usable.items()):
        if not _same(result, usable[reference]):
            base = usable[reference]
            problems.append(
                f"{code} differs from {reference}: bounds {_rounded(result.bounds)} / {_rounded(base.bounds)}, "
                f"area {result.area:.1f} / {base.area:.1f}, volume {result.volume:.1f} / {base.volume:.1f}"
            )
    return problems


def units_for(version):
    """The --units a version needs: R14 has no INSUNITS, so the converter
    takes it as meters, a known issue that would hide its geometry."""
    return "mm" if version == "AC1014" else None


def run_converter(binary, dwg, out, timeout, units=None):
    command = [str(binary), "--z-up", str(dwg), "-o", str(out)]
    if units:
        command += ["--units", units]
    try:
        done = subprocess.run(command, capture_output=True, text=True, errors="replace", timeout=timeout)
    except subprocess.TimeoutExpired:
        return Converted(None, "", f"timed out after {timeout} s")
    text = done.stdout + done.stderr
    if done.returncode != 0:
        skipped = "; ".join(line.strip() for line in SKIPPED.findall(text))
        return Converted(None, text, f"exit {done.returncode}: {' '.join(text.split())[:120]} ... skipped: {skipped}")
    if not Path(out).exists():
        return Converted(None, text, "no output file")
    try:
        return Converted(read_mesh(out), text, None)
    except (ValueError, KeyError, IndexError, OSError, struct.error) as e:
        return Converted(None, text, f"unreadable output: {e!r}")


def write_report(path, rows, version_problems, commit, binary_hash, warnings):
    """`rows`: (file, result, detail), result being match, bounds only (a
    part too fine for its area and volume to be checked), differs, failed or
    absent."""
    counts = {}
    for _, result, _ in rows:
        counts[result] = counts.get(result, 0) + 1
    lines = [
        "# BricsCAD corpus report",
        "",
        f"Converter: commit `{commit}`, binary SHA-256 `{binary_hash}`.",
        "",
        ", ".join(f"{n} {result}" for result, n in sorted(counts.items())) + ".",
        f"LibreDWG decode warnings on {warnings} files (recorded, not counted as failures).",
        "AC1014 is measured with `--units mm`: R14 cannot hold INSUNITS, and without it comes out",
        "1000 times too large (unitless R14 drawings are an open issue).",
        "",
        "## Versions that disagree",
        "",
        *([f"- {p}" for p in version_problems] or ["None."]),
        "",
        "## Files",
        "",
        "| File | Result | Detail |",
        "|---|---|---|",
    ]
    order = {"failed": 0, "differs": 1, "absent": 2, "bounds only": 3, "match": 4}
    for file, result, detail in sorted(rows, key=lambda r: (order.get(r[1], 9), r[0])):
        lines.append(f"| `{file}` | {result} | {detail.replace('|', '/')} |")
    Path(path).parent.mkdir(parents=True, exist_ok=True)
    Path(path).write_text("\n".join(lines) + "\n")


def measure_all(root, binary, family=None):
    expected = json.loads((root / "expected.json").read_text())
    rows, by_shape, warnings = [], {}, 0
    with tempfile.TemporaryDirectory() as folder:
        out = Path(folder) / "out.glb"
        for relative, entry in sorted(expected.items()):
            if family and entry["family"] != family:
                continue
            if entry["status"] == "failed":
                rows.append((relative, "absent", entry.get("reason", "BricsCAD could not make it")))
                continue
            out.unlink(missing_ok=True)
            done = run_converter(binary, root / relative, out, TIMEOUT, units_for(entry["version"]))
            warnings += bool(WARNING.search(done.stats))
            note = f" (BricsCAD: {entry['reason']})" if entry["status"] == "changed" else ""
            key = f"{entry['family']}/{entry['shape']}"
            by_shape.setdefault(key, {})[entry["version"]] = measured(entry, done.mesh)
            if done.error:
                rows.append((relative, "failed", done.error + note))
                continue
            problems = check_file(entry, done.mesh, done.stats)
            label = result_label(problems, by_shape[key][entry["version"]])
            rows.append((relative, label, "; ".join(problems) + note))
    version_problems = [f"{key}: {p}" for key, results in sorted(by_shape.items()) for p in compare_versions(results)]
    return rows, version_problems, warnings


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--root", required=True, type=Path)
    parser.add_argument("--binary", required=True, type=Path)
    parser.add_argument("--commit", required=True)
    parser.add_argument("--family")
    parser.add_argument("--report", type=Path, help="default: report/<root's name>.md next to this script")
    args = parser.parse_args()
    report = args.report or Path(__file__).parent / "report" / f"{args.root.name}.md"
    rows, version_problems, warnings = measure_all(args.root, args.binary, args.family)
    binary_hash = hashlib.sha256(args.binary.read_bytes()).hexdigest()[:12]
    write_report(report, rows, version_problems, args.commit, binary_hash, warnings)
    counts = {r: sum(row[1] == r for row in rows) for r in ("match", "bounds only", "differs", "failed", "absent")}
    print(report, counts, f"{len(version_problems)} version problems")


if __name__ == "__main__":
    main()
