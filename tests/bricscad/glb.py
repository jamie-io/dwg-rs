# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
"""Read a GLB's triangles in world space, the way a viewer shows them.

Standard library only. Handles what dwg2gltf writes without --compress.
"""

import array
import json
import math
import struct
from dataclasses import dataclass, field
from pathlib import Path

TRIANGLES = 4
COMPONENTS = {5126: "f", 5125: "I", 5123: "H", 5121: "B"}
IDENTITY = [[float(r == c) for c in range(4)] for r in range(4)]


def _sub(a, b):
    return (a[0] - b[0], a[1] - b[1], a[2] - b[2])


def _cross(a, b):
    return (
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    )


def _dot(a, b):
    return a[0] * b[0] + a[1] * b[1] + a[2] * b[2]


def _normal(t):
    """The triangle's normal, as long as twice its area."""
    return _cross(_sub(t[1], t[0]), _sub(t[2], t[0]))


@dataclass
class Mesh:
    # three corners each, wound counter-clockwise seen from the front
    triangles: list = field(default_factory=list)
    lines: int = 0

    def area(self):
        return sum(math.sqrt(_dot(n, n)) / 2 for n in map(_normal, self.triangles))

    def volume(self):
        """Signed volume; positive for a closed mesh that faces outward."""
        return sum(_dot(t[0], _cross(t[1], t[2])) for t in self.triangles) / 6

    def bounds(self):
        """(lo, hi) corners, or None without triangles."""
        if not self.triangles:
            return None
        points = [p for t in self.triangles for p in t]
        return (
            [min(p[k] for p in points) for k in range(3)],
            [max(p[k] for p in points) for k in range(3)],
        )

    def facing_share(self, axis):
        """The share of the area whose front faces along `axis`."""
        total = along = 0.0
        for n in map(_normal, self.triangles):
            size = math.sqrt(_dot(n, n))
            total += size
            if _dot(n, axis) > 0:
                along += size
        return along / total if total else 0.0


def _multiply(a, b):
    return [[sum(a[r][k] * b[k][c] for k in range(4)) for c in range(4)] for r in range(4)]


def _node_matrix(node):
    if "matrix" in node:
        m = node["matrix"]
        return [[m[c * 4 + r] for c in range(4)] for r in range(4)]
    result = [row[:] for row in IDENTITY]
    if "scale" in node:
        for k in range(3):
            result[k][k] = node["scale"][k]
    if "rotation" in node:
        x, y, z, w = node["rotation"]
        rotation = [
            [1 - 2 * (y * y + z * z), 2 * (x * y - z * w), 2 * (x * z + y * w), 0],
            [2 * (x * y + z * w), 1 - 2 * (x * x + z * z), 2 * (y * z - x * w), 0],
            [2 * (x * z - y * w), 2 * (y * z + x * w), 1 - 2 * (x * x + y * y), 0],
            [0, 0, 0, 1],
        ]
        result = _multiply(rotation, result)
    if "translation" in node:
        for k in range(3):
            result[k][3] = node["translation"][k]
    return result


def _determinant(m):
    a, b, c = m[0][:3], m[1][:3], m[2][:3]
    return _dot(a, _cross(b, c))


def _read_accessor(doc, binary, index):
    accessor = doc["accessors"][index]
    view = doc["bufferViews"][accessor["bufferView"]]
    size = {"SCALAR": 1, "VEC3": 3}[accessor["type"]]
    values = array.array(COMPONENTS[accessor["componentType"]])
    start = view.get("byteOffset", 0) + accessor.get("byteOffset", 0)
    values.frombytes(binary[start : start + accessor["count"] * size * values.itemsize])
    return values


def _chunks(path):
    data = Path(path).read_bytes()
    length = struct.unpack_from("<I", data, 12)[0]
    doc = json.loads(data[20 : 20 + length])
    return doc, data[20 + length + 8 :]


def read_mesh(path):
    doc, binary = _chunks(path)
    mesh = Mesh()

    def add(primitive, matrix):
        if primitive.get("mode", TRIANGLES) != TRIANGLES:
            mesh.lines += 1
            return
        p = _read_accessor(doc, binary, primitive["attributes"]["POSITION"])
        world = [
            tuple(
                matrix[r][0] * p[i] + matrix[r][1] * p[i + 1] + matrix[r][2] * p[i + 2] + matrix[r][3]
                for r in range(3)
            )
            for i in range(0, len(p), 3)
        ]
        indices = _read_accessor(doc, binary, primitive["indices"])
        mirrored = _determinant(matrix) < 0
        for i in range(0, len(indices), 3):
            a, b, c = (world[indices[i + k]] for k in range(3))
            mesh.triangles.append((a, c, b) if mirrored else (a, b, c))

    def walk(index, parent):
        node = doc["nodes"][index]
        matrix = _multiply(parent, _node_matrix(node))
        if "mesh" in node:
            for primitive in doc["meshes"][node["mesh"]]["primitives"]:
                add(primitive, matrix)
        for child in node.get("children", []):
            walk(child, matrix)

    for root in doc["scenes"][doc.get("scene", 0)]["nodes"]:
        walk(root, IDENTITY)
    return mesh
