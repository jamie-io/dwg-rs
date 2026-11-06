#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
"""Build the corpus with BricsCAD, from WSL.

    python3 tests/bricscad/generate.py --bricscad "C:\\Program Files\\Bricsys\\BricsCAD V27 en_US"

One BricsCAD process per shape builds it and saves it in every DWG version; a
second one reopens each saved file and measures it, which shows what saving
down did. A fresh process keeps a broken shape from reaching the next one.
"""

import argparse
import csv
import io
import json
import re
import shutil
import subprocess
from dataclasses import dataclass, field
from pathlib import Path

from catalog import SHAPES, VERSIONS

HERE = Path(__file__).parent
TIMEOUT = 170.0
# how far BricsCAD's reopened values may be from a shape's formula
AGREEMENT = 2e-4


def windows_path(path):
    """A WSL path as a Windows path with forward slashes, as LISP wants."""
    return subprocess.run(["wslpath", "-m", str(path)], capture_output=True, text=True, check=True).stdout.strip()


class Processes:
    """The running BricsCAD processes on Windows."""

    def listing(self):
        out = subprocess.run(
            ["tasklist.exe", "/FI", "IMAGENAME eq bricscad.exe", "/FO", "CSV", "/NH"],
            capture_output=True,
            text=True,
            errors="replace",  # the Windows console's encoding
            check=True,
        ).stdout
        rows = csv.reader(io.StringIO(out.replace("\r", "")))
        return {int(row[1]) for row in rows if len(row) > 1 and row[1].isdigit()}

    def stop(self, pid):
        subprocess.run(["taskkill.exe", "/PID", str(pid), "/F"], capture_output=True, check=False)


class Bricscad:
    """Runs a script; False when it timed out. After a timeout it stops only
    what this run started, never a BricsCAD the user had open."""

    def __init__(self, processes=None, launch=subprocess.run, to_windows=windows_path):
        self.processes = processes or Processes()
        self.launch = launch
        self.to_windows = to_windows

    def __call__(self, exe, scr, timeout):
        before = self.processes.listing()
        command = [exe, "/nologo", "/b", self.to_windows(scr).replace("/", "\\")]
        try:
            self.launch(command, cwd="/mnt/c", timeout=timeout, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
        except subprocess.TimeoutExpired:
            for pid in self.processes.listing() - before:
                self.processes.stop(pid)
            return False
        return True


@dataclass
class ShapeResult:
    shape: object
    folder: Path
    bricscad: str | None = None
    entries: dict = field(default_factory=dict)


def _write_scripts(shape, folder, to_windows):
    shutil.copy(HERE / "scripts" / "lib.lsp", folder / "lib.lsp")
    lib = to_windows(folder / "lib.lsp")
    win = lambda name: to_windows(folder / name)  # noqa: E731
    versions = " ".join(f'("{code}" {number})' for code, number in VERSIONS.items())
    (folder / "build.lsp").write_text(
        f'(load "{lib}")\n'
        f'(corpus-begin "{win("build.tsv")}")\n'
        f"(corpus-case '(lambda () {shape.lisp}))\n"
        "(corpus-built)\n"
        f"(corpus-save \"{win(folder.name)}\" '({versions}))\n"
        "(corpus-end)\n"
    )
    measures = "".join(f'(corpus-measure "{code}" "{win(f"{folder.name}_{code}.dwg")}")\n' for code in VERSIONS)
    (folder / "measure.lsp").write_text(
        f'(load "{lib}")\n(setq *corpus-log* "{win("measure.tsv")}")\n{measures}(corpus-end)\n'
    )
    for name in ("build", "measure"):
        (folder / f"{name}.scr").write_bytes(f'(load "{win(name + ".lsp")}")\r\n_.QUIT\r\n_Y\r\n'.encode())


def _read_log(path):
    if not path.exists():
        return []
    text = path.read_text(errors="replace").replace("\r", "")
    return [line.split("\t") for line in text.splitlines() if line]


def _number(text):
    return float(text) if text else None


def _names(text):
    return sorted(n for n in text.split(",") if n)


def _entry(shape, code, bricscad):
    return {
        "family": shape.family,
        "shape": shape.name,
        "version": code,
        "bricscad": bricscad,
        "entities": sorted(shape.entities),
        "volume": shape.volume if shape.closed else None,
        "area": shape.area,
        "bounds": [list(map(float, c)) for c in shape.bounds] if shape.bounds else None,
        "closed": shape.closed,
        "facing": shape.facing,
        "qa": shape.qa,
        "status": "ok",
    }


def _disagreement(shape, volume, bounds):
    """What differs between BricsCAD's values and the shape's formula."""
    if shape.volume is not None and volume is not None:
        if abs(volume - shape.volume) > AGREEMENT * abs(shape.volume):
            return f"volume {volume} against the formula's {shape.volume}"
    if shape.bounds is not None and bounds is not None:
        lo, hi = shape.bounds
        size = max(hi[k] - lo[k] for k in range(3))
        for got, want in zip(bounds[0] + bounds[1], [*lo, *hi]):
            if abs(got - want) > AGREEMENT * size:
                return f"bounds {bounds} against the formula's {shape.bounds}"
    return None


def _fail_all(result, reason):
    for code in VERSIONS:
        entry = _entry(result.shape, code, result.bricscad)
        entry.update(status="failed", reason=reason)
        result.entries[code] = entry
    return result


def run_shape(shape, exe, workdir, runner, to_windows=windows_path, timeout=TIMEOUT):
    folder = Path(workdir) / shape.family / shape.name
    if folder.exists():
        shutil.rmtree(folder)
    folder.mkdir(parents=True)
    _write_scripts(shape, folder, to_windows)
    result = ShapeResult(shape, folder)

    if not runner(exe, folder / "build.scr", timeout):
        return _fail_all(result, "timed out while building")
    log = _read_log(folder / "build.tsv")
    fields = {row[0]: row[1] if len(row) > 1 else "" for row in log}
    result.bricscad = fields.get("acadver")
    errors = {row[1]: row[2] for row in log if row[0] == "error" and len(row) > 2}
    if "case" in errors:
        return _fail_all(result, f"case: {errors['case']}")
    if "done" not in fields:
        return _fail_all(result, "the build did not finish")
    built = _names(fields.get("entities", ""))
    if built != sorted(shape.entities):
        return _fail_all(result, f"built {built}, not {sorted(shape.entities)}")
    built_area = _number(fields.get("area", ""))

    if not runner(exe, folder / "measure.scr", timeout):
        return _fail_all(result, "timed out while measuring")
    log = _read_log(folder / "measure.tsv")
    files = {row[1]: row for row in log if row[0] == "file"}
    for row in log:
        if row[0] == "error" and len(row) > 2:
            errors[row[1]] = row[2]

    for code in VERSIONS:
        entry = _entry(shape, code, result.bricscad)
        result.entries[code] = entry
        problem = errors.get(f"save {code}") or errors.get(f"open {code}")
        if problem or code not in files:
            entry.update(status="failed", reason=problem or "not measured")
            continue
        row = files[code]
        entry["entities"] = _names(row[2])
        volume, area = _number(row[3]), _number(row[4])
        numbers = [_number(v) for v in row[5:11]]
        bounds = [numbers[:3], numbers[3:]] if None not in numbers else None
        if entry["volume"] is None and shape.closed:
            entry["volume"] = volume
        if entry["area"] is None:
            entry["area"] = area if area is not None else built_area
        if entry["bounds"] is None:
            entry["bounds"] = bounds
        if entry["entities"] != sorted(shape.entities):
            entry.update(status="changed", reason=f"reopened as {entry['entities']}")
        elif (reason := _disagreement(shape, volume, bounds)) is not None:
            entry.update(status="failed", reason=reason)
    return result


def version_folder(acadver):
    return "v" + acadver.split(".")[0].split()[0]


def collect(results, repo):
    """Copy the files into `repo`/dwg/v<major>/ and merge expected.json."""
    known = [r.bricscad for r in results if r.bricscad]
    if not known:
        raise SystemExit("BricsCAD did not report a version; nothing to collect")
    root = Path(repo) / "dwg" / version_folder(known[0])
    path = root / "expected.json"
    expected = json.loads(path.read_text()) if path.exists() else {}
    for result in results:
        shape = result.shape
        for code, entry in result.entries.items():
            name = f"{shape.name}_{code}.dwg"
            relative = f"{shape.family}/{shape.name}/{name}"
            target = root / relative
            source = result.folder / name
            if entry["status"] == "failed" or not source.exists():
                target.unlink(missing_ok=True)
            else:
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copy(source, target)
            expected[relative] = entry
    root.mkdir(parents=True, exist_ok=True)
    path.write_text(json.dumps(expected, indent=1, sort_keys=True) + "\n")


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--bricscad", required=True, help="the Windows install folder")
    parser.add_argument("--family")
    parser.add_argument("--shape")
    parser.add_argument("--timeout", type=float, default=TIMEOUT)
    args = parser.parse_args()

    exe = subprocess.run(
        ["wslpath", "-u", args.bricscad.rstrip("\\") + "\\bricscad.exe"], capture_output=True, text=True, check=True
    ).stdout.strip()
    temp = subprocess.run(["cmd.exe", "/c", "echo %TEMP%"], capture_output=True, text=True, check=True, cwd="/mnt/c")
    workdir = Path(
        subprocess.run(["wslpath", "-u", temp.stdout.strip()], capture_output=True, text=True, check=True).stdout.strip()
    ) / "dwg_corpus"
    shapes = [
        s for s in SHAPES if (not args.family or s.family == args.family) and (not args.shape or s.name == args.shape)
    ]
    runner = Bricscad()
    # for a shape that never got to report it
    installed = re.search(r"V(\d+)", args.bricscad)
    version = f"{installed.group(1)}.0" if installed else None
    for shape in shapes:
        result = run_shape(shape, exe, workdir, runner, timeout=args.timeout)
        result.bricscad = result.bricscad or version
        collect([result], HERE)
        states = sorted({e["status"] for e in result.entries.values()})
        reasons = sorted({e.get("reason", "") for e in result.entries.values()} - {""})
        print(f"{shape.family}/{shape.name}: {', '.join(states)} {'; '.join(reasons)[:200]}", flush=True)


if __name__ == "__main__":
    main()
