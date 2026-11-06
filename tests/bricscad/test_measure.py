# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
"""measure.py's checks, on meshes built here and small stand-in converters."""

import stat
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

from glb import Mesh  # noqa: E402
from measure import (  # noqa: E402
    Measured,
    check_file,
    compare_versions,
    measured,
    result_label,
    run_converter,
    units_for,
    write_report,
)  # noqa: E402

FACES = [
    (0, 2, 1), (0, 3, 2), (4, 5, 6), (4, 6, 7), (0, 1, 5), (0, 5, 4),
    (1, 2, 6), (1, 6, 5), (2, 3, 7), (2, 7, 6), (3, 0, 4), (3, 4, 7),
]


def box(lo, hi):
    """A closed box between two corners in millimeters, as a mesh in meters."""
    x, y, z = zip(lo, hi)
    corners = [(x[i & 1 ^ (i >> 1 & 1)], y[i >> 1 & 1], z[i >> 2 & 1]) for i in range(8)]
    corners = [tuple(c / 1000 for c in p) for p in corners]
    return Mesh([tuple(corners[i] for i in face) for face in FACES])


def sheet(up, down):
    """Unit (1 m) squares in a row, `up` facing +z and `down` -z, in meters."""
    triangles = []
    for i in range(up + down):
        a, b, c, d = (i, 0, 0), (i + 1, 0, 0), (i + 1, 1, 0), (i, 1, 0)
        triangles += [(a, b, c), (a, c, d)] if i < up else [(a, c, b), (a, d, c)]
    return Mesh(triangles)


def entry(**changes):
    values = {
        "version": "AC1027", "status": "ok", "volume": 1000.0, "area": 600.0,
        "bounds": [[0.0, 0.0, 0.0], [10.0, 10.0, 10.0]], "closed": True, "facing": None,
    }
    return values | changes


CUBE = box((0, 0, 0), (10, 10, 10))


class CheckFile(unittest.TestCase):
    def test_exact_match_has_no_problems(self):
        self.assertEqual(check_file(entry(), CUBE, ""), [])

    def test_displaced_bounds_are_reported_with_values(self):
        problems = check_file(entry(), box((-5, -5, -5), (5, 5, 5)), "")
        self.assertEqual(len(problems), 1)
        self.assertIn("bounds", problems[0])
        self.assertIn("-5", problems[0])

    def test_excess_area_is_reported(self):
        problems = check_file(entry(area=600 / 1.63), CUBE, "")
        self.assertEqual(len(problems), 1)
        self.assertIn("area", problems[0])

    def test_faceting_within_tolerance_passes(self):
        self.assertEqual(check_file(entry(volume=1005.0, area=603.0), CUBE, ""), [])

    def test_thin_shape_gets_the_faceting_allowance(self):
        # moving 600 mm2 of surface by 0.1 mm changes the volume by 60 mm3
        self.assertEqual(check_file(entry(volume=1050.0), CUBE, ""), [])

    def test_volume_outside_the_faceting_allowance_is_reported(self):
        self.assertTrue(any("volume" in p for p in check_file(entry(volume=1100.0), CUBE, "")))

    def test_area_of_a_part_finer_than_the_tolerance_is_not_checked(self):
        # 0.6 mm thick at 0.1 mm chord deviation
        part = box((0, 0, 0), (2, 1, 0.6))
        expected = entry(volume=None, area=part.area() * 1e6 * 1.2, bounds=[[0.0, 0.0, 0.0], [2.0, 1.0, 0.6]])
        self.assertEqual(check_file(expected, part, ""), [])

    def test_area_of_a_part_coarser_than_the_tolerance_is_checked(self):
        part = box((0, 0, 0), (20, 10, 6))
        expected = entry(volume=None, area=part.area() * 1e6 * 1.2, bounds=[[0.0, 0.0, 0.0], [20.0, 10.0, 6.0]])
        self.assertTrue(any("area" in p for p in check_file(expected, part, "")))

    def test_fine_part_has_no_volume_check(self):
        # 0.1 mm of 11 mm2 is more than the part's whole volume
        part = box((0, 0, 0), (2, 1, 0.6))
        expected = entry(volume=99.0, area=None, bounds=[[0.0, 0.0, 0.0], [2.0, 1.0, 0.6]])
        self.assertEqual(check_file(expected, part, ""), [])

    def test_fine_part_with_wrong_bounds_differs(self):
        part = box((5, 0, 0), (7, 1, 0.6))
        expected = entry(volume=None, area=None, bounds=[[0.0, 0.0, 0.0], [2.0, 1.0, 0.6]])
        self.assertTrue(any("bounds" in p for p in check_file(expected, part, "")))

    def test_fine_part_says_only_its_bounds_were_checked(self):
        part = box((0, 0, 0), (2, 1, 0.6))
        self.assertEqual(result_label([], measured(entry(), part)), "bounds only")
        self.assertEqual(result_label(["x"], measured(entry(), part)), "differs")
        self.assertEqual(result_label([], measured(entry(), CUBE)), "match")

    def test_volume_allowance_comes_from_the_expected_area(self):
        problems = check_file(entry(volume=1070.0, area=60.0), CUBE, "")
        self.assertTrue(any("volume" in p for p in problems))

    def test_open_shape_has_no_volume_check(self):
        self.assertEqual(check_file(entry(closed=False, volume=None), CUBE, ""), [])

    def test_skipped_entity_is_a_failure(self):
        stats = "  skipped: EXTRUDEDSURFACE (no ACIS data)=1\n"
        problems = check_file(entry(), CUBE, stats)
        self.assertEqual(len(problems), 1)
        self.assertIn("EXTRUDEDSURFACE", problems[0])

    def test_outward_needs_positive_volume(self):
        inside_out = Mesh([(a, c, b) for a, b, c in CUBE.triangles])
        expected = entry(facing="outward", volume=None)
        self.assertEqual(check_file(expected, CUBE, ""), [])
        self.assertTrue(any("outward" in p for p in check_file(expected, inside_out, "")))

    def test_facing_axis_needs_every_face(self):
        expected = entry(closed=False, volume=None, area=None, bounds=None, facing=[0, 0, 1])
        self.assertEqual(check_file(expected, sheet(10, 0), ""), [])
        self.assertTrue(any("facing" in p for p in check_file(expected, sheet(7, 3), "")))

    def test_decode_warning_is_recorded_not_failed(self):
        stats = "  warning: LibreDWG reported recoverable decode errors (0x44): output may be incomplete\n"
        self.assertEqual(check_file(entry(), CUBE, stats), [])


class CompareVersions(unittest.TestCase):
    def results(self, **meshes):
        return {code: measured(entry(version=code), mesh) for code, mesh in meshes.items()}

    def test_versions_that_agree_have_no_problems(self):
        self.assertEqual(compare_versions(self.results(AC1014=CUBE, AC1027=CUBE)), [])

    def test_versions_that_disagree_are_named(self):
        moved = box((-5, -5, -5), (5, 5, 5))
        problems = compare_versions(self.results(AC1014=CUBE, AC1018=CUBE, AC1021=moved))
        self.assertEqual(len(problems), 1)
        self.assertIn("AC1021", problems[0])

    def test_versions_with_the_right_answer_are_the_reference(self):
        moved = box((-5, -5, -5), (5, 5, 5))
        problems = compare_versions(self.results(AC1014=moved, AC1015=moved, AC1021=moved, AC1018=CUBE, AC1027=CUBE))
        named = " ".join(p.split()[0] for p in problems)
        self.assertEqual(named, "AC1014 AC1015 AC1021")

    def test_changed_entry_is_left_out_of_the_comparison(self):
        results = self.results(AC1018=CUBE, AC1027=CUBE)
        results["AC1014"] = measured(entry(version="AC1014", status="changed"), box((5, 5, 5), (9, 9, 9)))
        self.assertEqual(compare_versions(results), [])

    def test_a_file_that_failed_is_left_out_of_the_comparison(self):
        results = self.results(AC1018=CUBE, AC1027=CUBE)
        results["AC1014"] = measured(entry(version="AC1014"), None)
        self.assertEqual(compare_versions(results), [])


class Converter(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.work = Path(self.dir.name)

    def script(self, text):
        path = self.work / "stand-in"
        path.write_text("#!/bin/sh\n" + text)
        path.chmod(path.stat().st_mode | stat.S_IEXEC)
        return path

    def test_no_output_file_is_a_failed_file(self):
        binary = self.script('echo "nothing to convert"\nexit 3\n')
        result = run_converter(binary, self.work / "a.dwg", self.work / "a.glb", 5)
        self.assertIsNone(result.mesh)
        self.assertIn("exit 3", result.error)
        self.assertIn("nothing to convert", result.error)

    def test_timeout_is_a_failed_file_and_the_run_goes_on(self):
        binary = self.script("exec sleep 5\n")
        result = run_converter(binary, self.work / "a.dwg", self.work / "a.glb", 0.1)
        self.assertIsNone(result.mesh)
        self.assertIn("timed out", result.error)

    def test_units_reach_the_converter(self):
        log = self.work / "args"
        binary = self.script(f'echo "$@" > {log}\nexit 3\n')
        run_converter(binary, self.work / "a.dwg", self.work / "a.glb", 5, units="mm")
        self.assertIn("--units mm", log.read_text())

    def test_only_r14_needs_its_units_named(self):
        # AC1014 cannot hold INSUNITS, and would come out 1000 times too large
        self.assertEqual(units_for("AC1014"), "mm")
        self.assertIsNone(units_for("AC1015"))

    def test_failed_file_keeps_the_skip_reason(self):
        filler = "converted: nothing here " * 20
        binary = self.script(f'echo "{filler}"\necho "  skipped: EXTRUDEDSURFACE (no ACIS data)=1"\nexit 3\n')
        result = run_converter(binary, self.work / "a.dwg", self.work / "a.glb", 5)
        self.assertIn("EXTRUDEDSURFACE (no ACIS data)=1", result.error)

    def test_unreadable_output_is_a_failed_file(self):
        binary = self.script('echo junk > "$4"\n')
        result = run_converter(binary, self.work / "a.dwg", self.work / "a.glb", 5)
        self.assertIsNone(result.mesh)
        self.assertIn("unreadable", result.error)

    def test_crash_is_a_failed_file(self):
        binary = self.script("kill -SEGV $$\n")
        result = run_converter(binary, self.work / "a.dwg", self.work / "a.glb", 5)
        self.assertIsNone(result.mesh)
        self.assertIsNotNone(result.error)


class Report(unittest.TestCase):
    def test_report_names_commit_binary_and_each_file(self):
        with tempfile.TemporaryDirectory() as folder:
            path = Path(folder) / "sub" / "v27.md"
            rows = [("a/b/b_AC1027.dwg", "match", ""), ("a/b/b_AC1021.dwg", "differs", "bounds off")]
            write_report(path, rows, ["a/b: AC1021 disagrees"], commit="abc1234", binary_hash="f00d", warnings=2)
            text = path.read_text()
        for needle in ("abc1234", "f00d", "b_AC1021.dwg", "bounds off", "1 match", "1 differs", "AC1021 disagrees"):
            self.assertIn(needle, text)


class Types(unittest.TestCase):
    def test_measured_holds_millimeters(self):
        result = measured(entry(), CUBE)
        self.assertIsInstance(result, Measured)
        self.assertAlmostEqual(result.volume, 1000)
        self.assertAlmostEqual(result.area, 600)


if __name__ == "__main__":
    unittest.main()
