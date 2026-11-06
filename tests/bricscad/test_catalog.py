# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
"""The catalog of shapes."""

import re
import sys
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

from catalog import AREA_SHEET_WIDTHS, CUSHION, SHAPES, majority_share, torus_segment  # noqa: E402

FAMILIES = ["primitive", "tube", "revolve", "fillet", "sweep", "loft", "trim", "region", "surface", "inverted", "sheet", "scale"]


def family(name):
    return [s for s in SHAPES if s.family == name]


class Catalog(unittest.TestCase):
    def test_names_are_unique_and_path_safe(self):
        names = [s.name for s in SHAPES]
        self.assertEqual(len(names), len(set(names)))
        for name in names:
            self.assertRegex(name, re.compile(r"^[a-z0-9_]+$"))

    def test_every_family_in_the_spec_is_present(self):
        for name in FAMILIES:
            self.assertTrue(family(name), name)

    def test_every_shape_says_what_it_should_hold(self):
        for shape in SHAPES:
            self.assertTrue(shape.entities, shape.name)

    def test_tube_angles(self):
        names = {s.name for s in family("tube")}
        self.assertTrue({"tube_030", "tube_090", "tube_180", "tube_270"} <= names)

    def test_cushion_is_filleted_near_half_its_thickness(self):
        self.assertGreaterEqual(CUSHION["radius"], 0.9 * CUSHION["thickness"] / 2)
        self.assertIn("fillet_cushion", {s.name for s in family("fillet")})

    def test_region_with_hole_area(self):
        shape = next(s for s in SHAPES if s.name == "region_hole")
        self.assertAlmostEqual(shape.area, 1285.84, places=2)

    def test_open_shapes_say_so(self):
        for shape in family("surface") + family("region"):
            self.assertFalse(shape.closed, shape.name)

    def test_surface_covers_the_five_kinds(self):
        names = {s.name for s in family("surface")}
        for kind in ("extruded", "lofted", "revolved", "swept", "planar"):
            self.assertIn(f"surface_{kind}", names)

    def test_closed_inverted_shapes_expect_outward(self):
        for shape in family("inverted"):
            self.assertEqual(shape.facing, "outward", shape.name)
        for shape in family("sheet"):
            if shape.closed:
                self.assertEqual(shape.facing, "outward", shape.name)

    def test_sheets_are_split_about_70_30(self):
        self.assertTrue(0.65 <= majority_share() <= 0.75)

    def test_cavity_keeps_its_volume(self):
        shape = next(s for s in SHAPES if s.name == "inverted_cavity")
        self.assertAlmostEqual(shape.volume, 60**3 - 20**3)
        self.assertGreater(shape.volume, 0)

    def test_sheet_exists_as_pface_3dface_and_mesh(self):
        names = {s.name for s in family("sheet")}
        for kind in ("3dface", "pface", "mesh"):
            self.assertIn(f"sheet_{kind}", names)
            self.assertIn(f"closed_{kind}", names)

    def test_open_sheets_name_the_way_most_area_faces(self):
        for shape in family("sheet"):
            if not shape.closed and "area" not in shape.name:
                self.assertEqual(shape.facing, (0, 0, 1), shape.name)

    def test_area_sheets_pin_the_rule(self):
        # most area wins, against most faces, the first face and always +z
        up, down = AREA_SHEET_WIDTHS
        self.assertLess(len(down), len(up))
        self.assertGreater(sum(down), 1.5 * sum(up))
        for kind in ("3dface", "pface", "mesh"):
            shape = next(s for s in SHAPES if s.name == f"sheet_area_{kind}")
            self.assertEqual(shape.facing, (0, 0, -1))

    def test_inverted_shapes_know_their_area(self):
        for shape in family("inverted"):
            self.assertIsNotNone(shape.area, shape.name)

    def test_scale_covers_small_large_and_far(self):
        names = {s.name for s in family("scale")}
        for base in ("tube", "fillet", "trim"):
            for kind in ("small", "large", "far"):
                self.assertIn(f"scale_{base}_{kind}", names)

    def test_scaled_values_follow_the_scale(self):
        shapes = {s.name: s for s in SHAPES}
        small = shapes["scale_tube_small"]
        self.assertAlmostEqual(small.volume, shapes["tube_090"].volume * 0.01**3)
        far = shapes["scale_fillet_far"]
        self.assertEqual(far.bounds[0][0], 1000020)

    def test_tube_formula(self):
        self.assertAlmostEqual(torus_segment(10, 100, 90), 49348.02, places=2)


if __name__ == "__main__":
    unittest.main()
