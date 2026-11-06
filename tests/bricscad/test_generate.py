# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
"""generate.py with a stand-in for BricsCAD."""

import json
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

from catalog import VERSIONS, Shape  # noqa: E402
from generate import Bricscad, collect, run_shape, version_folder  # noqa: E402

BOX = Shape("primitive", "box", '(command "_.BOX")', ("3dSolid",), "", volume=8.0, area=24.0)
OTHER = Shape("primitive", "other", '(command "_.BOX")', ("3dSolid",), "")


def fake_bricscad(entities="3dSolid", error="", reopened=None, version="27.0", volume="8.0"):
    """Answers a build and a measure run the way lib.lsp does."""

    def run(exe, scr, timeout):
        folder = scr.parent
        if scr.name == "build.scr":
            lines = [f"acadver\t{version}", f"entities\t{entities}", "area\t24.0"]
            if error:
                lines.append(f"error\tcase\t{error}")
            for code in VERSIONS:
                (folder / f"{folder.name}_{code}.dwg").write_bytes(b"AC10")
                lines.append(f"saved\t{code}")
            (folder / "build.tsv").write_text("\n".join(lines + ["done"]) + "\n")
        else:
            lines = []
            for code in VERSIONS:
                held = (reopened or {}).get(code, entities)
                lines.append(f"file\t{code}\t{held}\t{volume}\t\t0\t0\t0\t2\t2\t2")
            (folder / "measure.tsv").write_text("\n".join(lines + ["done"]) + "\n")
        return True

    return run


class Base(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.work = Path(self.dir.name)

    def run_shape(self, shape, runner):
        return run_shape(shape, "exe", self.work, runner, to_windows=str)


class RunShape(Base):
    def test_good_shape_is_ok_in_every_version(self):
        result = self.run_shape(BOX, fake_bricscad())
        self.assertEqual(sorted(result.entries), sorted(VERSIONS))
        for entry in result.entries.values():
            self.assertEqual(entry["status"], "ok")
            self.assertEqual(entry["volume"], 8.0)
            self.assertEqual(entry["area"], 24.0)
            self.assertEqual(entry["bounds"], [[0.0, 0.0, 0.0], [2.0, 2.0, 2.0]])

    def test_failed_command_marks_only_that_shape(self):
        first = self.run_shape(BOX, fake_bricscad(error="bad argument"))
        second = self.run_shape(OTHER, fake_bricscad())
        self.assertEqual(first.entries["AC1027"]["status"], "failed")
        self.assertIn("bad argument", first.entries["AC1027"]["reason"])
        self.assertEqual(second.entries["AC1027"]["status"], "ok")

    def test_wrong_entity_count_is_a_failure(self):
        result = self.run_shape(BOX, fake_bricscad(entities="3dSolid,3dSolid"))
        self.assertEqual(result.entries["AC1027"]["status"], "failed")

    def test_timeout_marks_the_shape_and_goes_on(self):
        result = self.run_shape(BOX, lambda exe, scr, timeout: False)
        self.assertEqual(result.entries["AC1027"]["status"], "failed")
        self.assertIn("timed out", result.entries["AC1027"]["reason"])

    def test_saving_down_that_changes_the_entity_is_recorded(self):
        result = self.run_shape(BOX, fake_bricscad(reopened={"AC1014": "Region"}))
        self.assertEqual(result.entries["AC1014"]["status"], "changed")
        self.assertEqual(result.entries["AC1014"]["entities"], ["Region"])
        self.assertEqual(result.entries["AC1032"]["status"], "ok")

    def test_formula_that_disagrees_with_bricscad_fails(self):
        result = self.run_shape(BOX, fake_bricscad(volume="9.0"))
        self.assertEqual(result.entries["AC1027"]["status"], "failed")
        self.assertIn("formula", result.entries["AC1027"]["reason"])

    def test_lisp_paths_come_from_to_windows(self):
        run_shape(BOX, "exe", self.work, fake_bricscad(), to_windows=lambda p: "C:/x/" + p.name)
        text = (self.work / "primitive" / "box" / "build.lsp").read_text()
        self.assertIn('"C:/x/lib.lsp"', text)


class Lib(unittest.TestCase):
    def test_units_cannot_raise_the_incompatible_units_dialog(self):
        # the template's Architectural LUNITS with INSUNITS 4 stops every
        # reopen at a dialog
        text = (Path(__file__).parent / "scripts" / "lib.lsp").read_text()
        self.assertIn('(setvar "LUNITS" 2)', text)


class Collect(Base):
    def test_version_folder_comes_from_acadver(self):
        self.assertEqual(version_folder("26.0"), "v26")
        self.assertEqual(version_folder("27.1 BricsCAD"), "v27")

    def test_other_version_leaves_v27_alone(self):
        repo = self.work / "repo"
        old = repo / "dwg" / "v27" / "expected.json"
        old.parent.mkdir(parents=True)
        old.write_text("{}")
        result = self.run_shape(BOX, fake_bricscad(version="26.0"))
        collect([result], repo)
        self.assertEqual(old.read_text(), "{}")
        self.assertTrue((repo / "dwg/v26/primitive/box/box_AC1027.dwg").exists())
        expected = json.loads((repo / "dwg/v26/expected.json").read_text())
        self.assertEqual(expected["primitive/box/box_AC1027.dwg"]["bricscad"], "26.0")

    def test_collect_keeps_entries_of_other_shapes(self):
        repo = self.work / "repo"
        collect([self.run_shape(BOX, fake_bricscad())], repo)
        collect([self.run_shape(OTHER, fake_bricscad())], repo)
        expected = json.loads((repo / "dwg/v27/expected.json").read_text())
        self.assertIn("primitive/box/box_AC1027.dwg", expected)
        self.assertIn("primitive/other/other_AC1027.dwg", expected)


class FakeProcesses:
    def __init__(self, *listings):
        self.listings = list(listings)
        self.stopped = []

    def listing(self):
        return self.listings.pop(0)

    def stop(self, pid):
        self.stopped.append(pid)


class Timeouts(unittest.TestCase):
    def test_only_stops_processes_it_started(self):
        def hang(*args, **kwargs):
            raise subprocess.TimeoutExpired("bricscad", 1)

        processes = FakeProcesses({100}, {100, 200})
        runner = Bricscad(processes, launch=hang, to_windows=str)
        self.assertFalse(runner("exe", Path("/x/build.scr"), 1))
        self.assertEqual(processes.stopped, [200])

    def test_finished_run_stops_nothing(self):
        processes = FakeProcesses({100})
        runner = Bricscad(processes, launch=lambda *a, **k: None, to_windows=str)
        self.assertTrue(runner("exe", Path("/x/build.scr"), 1))
        self.assertEqual(processes.stopped, [])


if __name__ == "__main__":
    unittest.main()
