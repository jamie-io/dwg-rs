# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
"""glb.py on small GLB files built here."""

import json
import struct
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).parent))

from glb import read_mesh  # noqa: E402

CUBE = [(0, 0, 0), (1, 0, 0), (1, 1, 0), (0, 1, 0), (0, 0, 1), (1, 0, 1), (1, 1, 1), (0, 1, 1)]
# counter-clockwise seen from outside
CUBE_TRIS = [
    (0, 2, 1), (0, 3, 2), (4, 5, 6), (4, 6, 7), (0, 1, 5), (0, 5, 4),
    (1, 2, 6), (1, 6, 5), (2, 3, 7), (2, 7, 6), (3, 0, 4), (3, 4, 7),
]


def square(z, up):
    """A unit square at height z, facing +z or -z."""
    points = [(0, 0, z), (1, 0, z), (1, 1, z), (0, 1, z)]
    return points, [(0, 1, 2), (0, 2, 3)] if up else [(0, 2, 1), (0, 3, 2)]


def primitive(points, tris, mode=4):
    """Binary data and the primitive's description, offsets filled in later."""
    vertices = b"".join(struct.pack("<3f", *p) for p in points)
    indices = b"".join(struct.pack("<3I", *t) for t in tris)
    return vertices, indices, mode


def write_glb(path, parts, node):
    """`parts` from primitive(), all in one mesh of a node with extra keys
    `node`, under a root node."""
    blob, views, accessors, primitives = b"", [], [], []
    for vertices, indices, mode in parts:
        for data, kind, component in (
            (vertices, "VEC3", 5126),
            (indices, "SCALAR", 5125),
        ):
            views.append({"buffer": 0, "byteOffset": len(blob), "byteLength": len(data)})
            count = len(data) // (12 if kind == "VEC3" else 4)
            accessors.append(
                {"bufferView": len(views) - 1, "componentType": component, "count": count, "type": kind}
            )
            blob += data
        primitives.append(
            {"attributes": {"POSITION": len(accessors) - 2}, "indices": len(accessors) - 1, "mode": mode}
        )
    document = {
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0]}],
        "nodes": [{"children": [1]}, {"mesh": 0, **node}],
        "meshes": [{"primitives": primitives}],
        "accessors": accessors,
        "bufferViews": views,
        "buffers": [{"byteLength": len(blob)}],
    }
    text = json.dumps(document).encode()
    text += b" " * (-len(text) % 4)
    blob += b"\0" * (-len(blob) % 4)
    total = 12 + 8 + len(text) + 8 + len(blob)
    Path(path).write_bytes(
        struct.pack("<3I", 0x46546C67, 2, total)
        + struct.pack("<2I", len(text), 0x4E4F534A)
        + text
        + struct.pack("<2I", len(blob), 0x004E4942)
        + blob
    )


class Measures(unittest.TestCase):
    def setUp(self):
        self.dir = tempfile.TemporaryDirectory()
        self.addCleanup(self.dir.cleanup)
        self.path = Path(self.dir.name) / "t.glb"

    def mesh(self, parts, **node):
        write_glb(self.path, parts, node)
        return read_mesh(self.path)

    def test_unit_cube_measures(self):
        m = self.mesh([primitive(CUBE, CUBE_TRIS)])
        self.assertAlmostEqual(m.area(), 6)
        self.assertAlmostEqual(m.volume(), 1)
        self.assertEqual(m.bounds(), ([0, 0, 0], [1, 1, 1]))

    def test_node_scale_is_applied(self):
        m = self.mesh([primitive(CUBE, CUBE_TRIS)], scale=[0.001, 0.001, 0.001])
        self.assertAlmostEqual(m.volume() * 1e9, 1)

    def test_matrix_node_is_applied(self):
        matrix = [1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 5, 6, 7, 1]
        m = self.mesh([primitive(CUBE, CUBE_TRIS)], matrix=matrix)
        self.assertEqual(m.bounds(), ([5, 6, 7], [6, 7, 8]))

    def test_mirrored_node_keeps_positive_volume(self):
        # glTF reverses the winding under a negative determinant
        m = self.mesh([primitive(CUBE, CUBE_TRIS)], scale=[-1, 1, 1])
        self.assertAlmostEqual(m.volume(), 1)
        self.assertEqual(m.bounds(), ([-1, 0, 0], [0, 1, 1]))

    def test_facing_share_counts_area(self):
        parts = [primitive(*square(z, up)) for z, up in ((0, True), (1, True), (2, True), (3, False))]
        m = self.mesh(parts)
        self.assertAlmostEqual(m.facing_share((0, 0, 1)), 0.75)

    def test_line_primitives_are_counted_not_measured(self):
        m = self.mesh([primitive(CUBE, CUBE_TRIS), primitive(CUBE, [(0, 1, 2)], mode=1)])
        self.assertEqual(m.lines, 1)
        self.assertEqual(len(m.triangles), 12)


if __name__ == "__main__":
    unittest.main()
