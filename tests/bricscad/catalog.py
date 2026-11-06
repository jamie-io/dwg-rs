# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
"""The shapes of the corpus: how each is drawn and what is right about it.

`lisp` runs in a fresh drawing, in millimeters. `entities` is what model space
must hold afterwards, as object names without AcDb. Exact values are given
where a formula exists; the others come from BricsCAD (see generate.py).
"""

from dataclasses import dataclass
from math import pi, radians, sqrt

# what `vla-saveas` calls each DWG version
VERSIONS = {
    "AC1014": 8,
    "AC1015": 12,
    "AC1018": 24,
    "AC1021": 36,
    "AC1024": 48,
    "AC1027": 60,
    "AC1032": 64,
}


@dataclass(frozen=True)
class Shape:
    family: str
    name: str
    lisp: str
    entities: tuple
    qa: str
    volume: float | None = None
    area: float | None = None
    bounds: tuple | None = None
    closed: bool = True
    facing: str | tuple | None = None


def torus_segment(minor, major, degrees):
    """Volume of a tube of radius `minor` bent through `degrees` at radius
    `major`."""
    return pi * minor**2 * radians(degrees) * major


# the wedge of 60 x 40 x 50 that `primitive` and `inverted` draw
WEDGE_AREA = 60 * 40 + 40 * 50 + 60 * 50 + 40 * sqrt(60**2 + 50**2)


def _primitives():
    def solid(name, lisp, volume, area, bounds):
        return Shape("primitive", name, lisp, ("3dSolid",), "displaced solids (trial)", volume, area, bounds)

    box_bounds = ((10, 20, 30), (110, 80, 70))
    box_area = 2 * (100 * 60 + 100 * 40 + 60 * 40)
    box_volume = 100.0 * 60 * 40
    cone_side = sqrt(40**2 + 90**2)
    return [
        solid("box", """(command "_.BOX" '(10 20 30) '(110 80 70))""", box_volume, box_area, box_bounds),
        solid(
            "box_vla",
            """(vla-addbox (vla-get-modelspace *doc*) (vlax-3d-point '(60 50 50)) 100 60 40)""",
            box_volume,
            box_area,
            box_bounds,
        ),
        solid(
            "wedge",
            """(command "_.WEDGE" '(10 20 30) '(70 60 80))""",
            60.0 * 40 * 50 / 2,
            WEDGE_AREA,
            ((10, 20, 30), (70, 60, 80)),
        ),
        solid(
            "cone",
            """(command "_.CONE" '(200 100 50) 40 90)""",
            pi * 40**2 * 90 / 3,
            pi * 40 * (40 + cone_side),
            ((160, 60, 50), (240, 140, 140)),
        ),
        solid(
            "cylinder",
            """(command "_.CYLINDER" '(200 100 50) 30 120)""",
            pi * 30**2 * 120,
            2 * pi * 30 * (30 + 120),
            ((170, 70, 50), (230, 130, 170)),
        ),
        solid(
            "sphere",
            """(command "_.SPHERE" '(200 100 50) 40)""",
            4 / 3 * pi * 40**3,
            4 * pi * 40**2,
            ((160, 60, 10), (240, 140, 90)),
        ),
        solid(
            "torus",
            """(command "_.TORUS" '(200 100 50) 60 15)""",
            2 * pi**2 * 60 * 15**2,
            4 * pi**2 * 60 * 15,
            ((125, 25, 35), (275, 175, 65)),
        ),
    ]


def _revolved(family, name, setup, degrees, volume, qa, area=None):
    """A solid from revolving a profile (set up as `e`) about the Y axis."""
    lisp = f"{setup} (corpus-revolve e '(0 0 0) '(0 1 0) {degrees})"
    return Shape(family, name, lisp, ("3dSolid",), qa, volume, area)


def _tubes():
    qa = "tube bend drawn as a full ring"
    circle = "(setq e (corpus-circle '({major} 0 0) {minor}))"

    def tube(name, minor, major, degrees):
        setup = circle.format(minor=minor, major=major)
        return _revolved("tube", name, setup, degrees, torus_segment(minor, major, degrees), qa)

    hollow = (
        "(setq e (corpus-minus (corpus-region (corpus-circle '(100 0 0) 12))"
        " (corpus-region (corpus-circle '(100 0 0) 9))))"
    )
    bend_length = 100 + pi * 50 / 2 + 100
    swept = (
        "(setq e (corpus-circle '(0 0 0) 5))"
        """ (setq p (corpus-call '("_.PLINE" (0 0) (100 0) "_A" "_CE" (100 50) "_A" 90 "_L" (150 150) "")))"""
        " (corpus-keep (corpus-sweep e p))"
    )
    return [
        tube("tube_030", 10, 100, 30),
        tube("tube_090", 10, 100, 90),
        tube("tube_180", 10, 100, 180),
        tube("tube_270", 10, 100, 270),
        tube("tube_thin", 2, 100, 90),
        tube("tube_thick", 45, 100, 90),
        _revolved(
            "tube", "tube_hollow", hollow, 90, torus_segment(12, 100, 90) - torus_segment(9, 100, 90), qa
        ),
        Shape("tube", "tube_swept_bend", swept, ("3dSolid",), qa, pi * 5**2 * bend_length),
    ]


def _revolves():
    qa = "tube bend drawn as a full ring"
    ring = "(setq e (corpus-rect '(50 0) '(70 20)))"
    half_disc = (
        "(setq e (corpus-minus (corpus-region (corpus-circle '(0 0 0) 40))"
        " (corpus-region (corpus-rect '(-50 -50) '(0 50)))))"
    )
    triangle = """(setq e (corpus-call '("_.PLINE" (0 0) (40 0) (0 90) "_C")))"""
    ring_volume = 2 * pi * 60 * 20 * 20
    sphere = 4 / 3 * pi * 40**3
    return [
        _revolved("revolve", "revolve_ring_360", ring, 360, ring_volume, qa),
        _revolved("revolve", "revolve_ring_120", ring, 120, ring_volume / 3, qa),
        _revolved("revolve", "revolve_half_disc_360", half_disc, 360, sphere, qa),
        _revolved("revolve", "revolve_half_disc_180", half_disc, 180, sphere / 2, qa),
        _revolved("revolve", "revolve_triangle_360", triangle, 360, pi * 40**2 * 90 / 3, qa),
    ]


# a cushion: edges rounded to nearly half the thickness
CUSHION = {"side": 400, "thickness": 100, "radius": 47}


def _fillets():
    qa = "missing cushion, flat disc"

    def rounded(name, a, b, c, r):
        lisp = f"(corpus-rounded-box '(20 30 40) '({20 + a} {30 + b} {40 + c}) {r})"
        return Shape("fillet", name, lisp, ("3dSolid",), qa, bounds=((20, 30, 40), (20 + a, 30 + b, 40 + c)))

    side, thick, radius = CUSHION["side"], CUSHION["thickness"], CUSHION["radius"]
    return [
        rounded("fillet_small", 100, 60, 40, 2),
        rounded("fillet_large", 100, 60, 40, 15),
        rounded("fillet_cushion", side, side, thick, radius),
        rounded("fillet_cushion_long", side * 2, side, thick, radius),
    ]


def _sweeps():
    qa = "spiky needles"
    path = "(setq p (corpus-spline '((0 0 0) (40 80 30) (120 40 60) (200 120 90))))"
    long_path = "(setq p (corpus-spline '((0 0 0) (300 20 0) (600 -20 10) (900 0 0))))"
    helix = """(setq p (corpus-call '("_.HELIX" (0 0 0) 30 30 120)))"""
    circle = "(setq e (corpus-circle '(0 0 0) 4))"
    square = "(setq e (corpus-rect '(-3 -3) '(3 3)))"
    slot = "(setq e (corpus-rect '(-4 -1) '(4 1)))"

    def sweep(name, profile, along):
        return Shape("sweep", name, f"{along} {profile} (corpus-keep (corpus-sweep e p))", ("3dSolid",), qa)

    return [
        sweep("sweep_circle_spline", circle, path),
        sweep("sweep_square_spline", square, path),
        sweep("sweep_circle_helix", circle, helix),
        sweep("sweep_square_helix", square, helix),
        sweep("sweep_long_thin", slot, long_path),
    ]


def _lofts():
    qa = "spiky needles"

    def loft(name, sections, volume=None):
        lisp = (
            f"(setq s (list {sections})) "
            """(corpus-keep (corpus-call (append '("_.LOFT") s '("" ""))))"""
        )
        return Shape("loft", name, lisp, ("3dSolid",), qa, volume)

    disc = "(corpus-circle '(0 0 {z}) {r})"
    frustum = pi * 100 / 3 * (30**2 + 30 * 15 + 15**2)
    return [
        loft("loft_two", f"{disc.format(z=0, r=30)} {disc.format(z=100, r=15)}", frustum),
        loft(
            "loft_three",
            f"{disc.format(z=0, r=30)} {disc.format(z=100, r=15)} {disc.format(z=200, r=25)}",
        ),
        loft(
            "loft_circle_square",
            f"{disc.format(z=0, r=30)} (corpus-call '(\"_.RECTANG\" (-20 -20 100) (20 20 100)))",
        ),
    ]


def _trims():
    qa = "trimmed surfaces"
    sphere = """(setq a (corpus-call '("_.SPHERE" (0 0 0) 40)))"""
    cylinder = """(setq a (corpus-call '("_.CYLINDER" (0 0 0) 30 100)))"""
    along_z = """(setq b (corpus-call '("_.CYLINDER" (0 0 -60) 10 120)))"""
    across = """(setq b (corpus-call '("_.CYLINDER" {start} 10 "_A" {end})))"""
    x_at_0 = across.format(start="(-60 0 0)", end="(60 0 0)")
    x_at_50 = across.format(start="(-60 0 50)", end="(60 0 50)")
    y_at_50 = across.format(start="(0 -60 50)", end="(0 60 50)")
    crossing = (
        """(setq a (corpus-call '("_.CYLINDER" (0 0 -50) 20 100)))"""
        """ (setq b (corpus-call '("_.CYLINDER" (-50 0 0) 20 "_A" (50 0 0))))"""
    )
    torus = """(setq a (corpus-call '("_.TORUS" (0 0 0) 60 15)))"""
    half_space = """(setq b (corpus-call '("_.BOX" (-100 0 -100) (100 100 100))))"""
    cone = """(setq a (corpus-call '("_.CONE" (0 0 0) 40 90)))"""
    top = """(setq b (corpus-call '("_.BOX" (-100 -100 45) (100 100 200))))"""

    def cut(name, *setup, volume=None, join="corpus-minus"):
        return Shape("trim", name, f"{' '.join(setup)} ({join} a b)", ("3dSolid",), qa, volume)

    r = 20
    return [
        # a hole through a sphere: the volume depends on its height alone
        cut("trim_sphere_hole", sphere, along_z, volume=4 / 3 * pi * (40**2 - 10**2) ** 1.5),
        cut("trim_sphere_hole_seam", sphere, x_at_0, volume=4 / 3 * pi * (40**2 - 10**2) ** 1.5),
        cut("trim_cylinder_hole", cylinder, y_at_50),
        cut("trim_cylinder_hole_seam", cylinder, x_at_50),
        cut("trim_crossing", crossing, volume=2 * pi * r**2 * 100 - 16 / 3 * r**3, join="corpus-union"),
        cut("trim_cut_torus", torus, half_space, volume=pi**2 * 60 * 15**2),
        cut("trim_cut_cone", cone, top, volume=pi * 45 / 3 * (40**2 + 40 * 20 + 20**2)),
    ]


def _regions():
    qa = "REGION gave no triangles"
    square = "(setq e (corpus-region (corpus-rect '(0 0) '(40 40))))"

    def hole(center, radius):
        return f"(setq e (corpus-minus e (corpus-region (corpus-circle '{center} {radius}))))"

    def region(name, lisp, area, bounds=None):
        return Shape("region", name, lisp, ("Region",), qa, area=area, bounds=bounds, closed=False)

    flat = ((0, 0, 0), (40, 40, 0))
    tilted = f"""{square} {hole("(20 20 0)", 10)} (command "_.ROTATE3D" e "" "_X" '(0 0 0) 30)"""
    return [
        region("region_plain", square, 1600.0, flat),
        region("region_hole", f"""{square} {hole("(20 20 0)", 10)}""", 1600 - pi * 100, flat),
        region(
            "region_two_holes",
            f"""{square} {hole("(12 20 0)", 6)} {hole("(28 20 0)", 6)}""",
            1600 - 2 * pi * 36,
            flat,
        ),
        region("region_tilted", tilted, 1600 - pi * 100),
    ]


def _surfaces():
    qa = "SURFACE with no data or no triangles"

    def surface(name, lisp, entity, area=None, bounds=None):
        return Shape("surface", name, lisp, (entity,), qa, area=area, bounds=bounds, closed=False)

    line = """(setq e (corpus-call '("_.LINE" {a} {b} "")))"""
    path = "(setq p (corpus-spline '((0 0 0) (40 80 30) (120 40 60) (200 120 90))))"
    slant = sqrt(20**2 + 20**2)
    return [
        surface(
            "surface_extruded",
            line.format(a="(100 100 0)", b="(160 140 0)") + " (corpus-keep (corpus-extrude e 50))",
            "ExtrudedSurface",
            sqrt(60**2 + 40**2) * 50,
            ((100, 100, 0), (160, 140, 50)),
        ),
        surface(
            "surface_revolved",
            line.format(a="(50 0 0)", b="(70 20 0)") + " (corpus-keep (corpus-revolve e '(0 0 0) '(0 1 0) 180))",
            "RevolvedSurface",
            pi * (50 + 70) * slant / 2,
        ),
        surface(
            "surface_lofted",
            "(setq a (corpus-spline '((0 0 0) (30 20 0) (60 -10 0) (90 0 0))))"
            " (setq b (corpus-spline '((0 0 80) (30 -20 80) (60 10 80) (90 0 80))))"
            """ (corpus-keep (corpus-call (list "_.LOFT" a b "" "")))""",
            "LoftedSurface",
        ),
        surface(
            "surface_swept",
            f"""{path} {line.format(a="(-5 0 0)", b="(5 0 0)")} (corpus-keep (corpus-sweep e p))""",
            "SweptSurface",
        ),
        surface(
            "surface_planar",
            # PLANESURF hangs a script when given corners, and does nothing on an outline
            """(setq e (corpus-region (corpus-rect '(0 0) '(40 30)))) (corpus-call (list "_.CONVTOSURFACE" e ""))""",
            "PlaneSurface",
            1200.0,
            ((0, 0, 0), (40, 30, 0)),
        ),
    ]


def _inverted():
    qa = "inside-out faces"
    wedge = """(setq w (corpus-call '("_.WEDGE" (10 20 30) (70 60 80))))"""
    volume = 60.0 * 40 * 50 / 2
    block = f"""{wedge} (corpus-block "W" w)"""

    def box(sx, sy, sz, at):
        ax = sorted((at[0] + sx * 10, at[0] + sx * 70))
        ay = sorted((at[1] + sy * 20, at[1] + sy * 60))
        az = sorted((at[2] + sz * 30, at[2] + sz * 80))
        return ((ax[0], ay[0], az[0]), (ax[1], ay[1], az[1]))

    def inserted(name, sx, sy, sz):
        at = (200, 100, 50)
        lisp = f"""{block} (corpus-insert "W" '{at} {sx}.0 {sy}.0 {sz}.0)""".replace(",", "")
        return Shape(
            "inverted",
            name,
            lisp,
            ("BlockReference",),
            qa,
            volume,
            WEDGE_AREA,
            box(sx, sy, sz, at),
            facing="outward",
        )

    nested = (
        f"""{block} (setq i (corpus-insert "W" '(0 0 0) -1.0 1.0 1.0)) (corpus-block "OUTER" i)"""
        """ (corpus-insert "OUTER" '(200 100 50) {sx}.0 1.0 1.0)"""
    )
    mirror = f"""{wedge} (command "_.MIRROR3D" w "" "_YZ" '(0 0 0) "_Y")"""
    cavity = (
        """(setq a (corpus-call '("_.BOX" (0 0 0) (60 60 60))))"""
        """ (setq b (corpus-call '("_.BOX" (20 20 20) (40 40 40))))"""
        " (corpus-minus a b)"
    )

    def solid(name, lisp, volume_, bounds, entity="3dSolid", area=WEDGE_AREA):
        return Shape("inverted", name, lisp, (entity,), qa, volume_, area, bounds, facing="outward")

    return [
        solid("inverted_mirror3d", mirror, volume, ((-70, 20, 30), (-10, 60, 80))),
        inserted("inverted_insert_x", -1, 1, 1),
        inserted("inverted_insert_y", 1, -1, 1),
        inserted("inverted_insert_z", 1, 1, -1),
        inserted("inverted_insert_xy", -1, -1, 1),
        inserted("inverted_insert_xyz", -1, -1, -1),
        solid(
            "inverted_nested_even",
            nested.format(sx=-1),
            volume,
            ((210, 120, 80), (270, 160, 130)),
            "BlockReference",
        ),
        solid(
            "inverted_nested_odd",
            nested.format(sx=1),
            volume,
            ((130, 120, 80), (190, 160, 130)),
            "BlockReference",
        ),
        solid("inverted_cavity", cavity, 60.0**3 - 20**3, ((0, 0, 0), (60, 60, 60)), area=6.0 * (60**2 + 20**2)),
    ]


SHEET_CELLS = 10
SHEET_FLIPPED = 3
CELL = 20


def majority_share():
    return (SHEET_CELLS - SHEET_FLIPPED) / SHEET_CELLS


def _point(p):
    return f"({p[0]} {p[1]} {p[2]})"


def _points(points):
    return "'(" + " ".join(_point(p) for p in points) + ")"


# cells facing +z, then cells facing -z, by width: the -z side has most of
# the area, but fewer faces and not the first one
AREA_SHEET_WIDTHS = ((5,) * 7, (40,) * 3)


def _strip(cells, wave=False):
    """Two rows of corners along a strip, and cells as corner numbers (from
    0). `cells`: (width, faces up)."""
    points = [(0, 0, 0), (0, CELL, 0)]
    faces = []
    x = 0
    for i, (width, up) in enumerate(cells):
        x += width
        z = 4 if wave and (i + 1) % 2 else 0
        points += [(x, 0, z), (x, CELL, z)]
        a, b, c, d = 2 * i, 2 * i + 2, 2 * i + 3, 2 * i + 1
        faces.append((a, b, c, d) if up else (d, c, b, a))
    return points, faces


def _sheet_mesh(wave):
    """Most cells facing +z, the last ones turned over."""
    return _strip([(CELL, i < SHEET_CELLS - SHEET_FLIPPED) for i in range(SHEET_CELLS)], wave)


def _closed_mesh():
    """A cube of side 20 with all its faces turned inwards."""
    s = 20
    points = [(0, 0, 0), (s, 0, 0), (s, s, 0), (0, s, 0), (0, 0, s), (s, 0, s), (s, s, s), (0, s, s)]
    outward = [(0, 3, 2, 1), (4, 5, 6, 7), (0, 1, 5, 4), (1, 2, 6, 5), (2, 3, 7, 6), (3, 0, 4, 7)]
    return points, [tuple(reversed(f)) for f in outward]


def _faces_lisp(points, cells):
    return " ".join(f"(corpus-face {' '.join(chr(39) + _point(points[i]) for i in c)})" for c in cells)


def _pface_lisp(points, cells):
    faces = "'(" + " ".join("(" + " ".join(str(i + 1) for i in c) + ")" for c in cells) + ")"
    return f"(corpus-pface {_points(points)} {faces})"


def _mesh_lisp(points, cells):
    faces = "'(" + " ".join("(" + " ".join(map(str, c)) + ")" for c in cells) + ")"
    return f"(corpus-mesh {_points(points)} {faces})"


def _sheets():
    qa = "patchy faces"
    flat = _sheet_mesh(False)
    wavy = _sheet_mesh(True)
    closed = _closed_mesh()
    up, down = AREA_SHEET_WIDTHS
    mixed = _strip([(w, True) for w in up] + [(w, False) for w in down])
    mixed_width = sum(up) + sum(down)
    bounds = ((0, 0, 0), (SHEET_CELLS * CELL, CELL, 0))
    cube = ((0, 0, 0), (20, 20, 20))

    def sheet(name, lisp, entities, wave=False):
        return Shape(
            "sheet",
            name,
            lisp,
            entities,
            qa,
            area=None if wave else float(SHEET_CELLS * CELL * CELL),
            bounds=None if wave else bounds,
            closed=False,
            facing=(0, 0, 1),
        )

    def by_area(name, lisp, entities):
        return Shape(
            "sheet",
            name,
            lisp,
            entities,
            qa,
            area=float(mixed_width * CELL),
            bounds=((0, 0, 0), (mixed_width, CELL, 0)),
            closed=False,
            facing=(0, 0, -1),
        )

    def box(name, lisp, entities):
        return Shape("sheet", name, lisp, entities, qa, 8000.0, 2400.0, cube, facing="outward")

    return [
        sheet("sheet_3dface", _faces_lisp(*flat), ("Face",) * SHEET_CELLS),
        sheet("sheet_pface", _pface_lisp(*flat), ("PolyFaceMesh",)),
        sheet("sheet_mesh", _mesh_lisp(*flat), ("SubDMesh",)),
        sheet("sheet_wavy_3dface", _faces_lisp(*wavy), ("Face",) * SHEET_CELLS, wave=True),
        sheet("sheet_wavy_pface", _pface_lisp(*wavy), ("PolyFaceMesh",), wave=True),
        sheet("sheet_wavy_mesh", _mesh_lisp(*wavy), ("SubDMesh",), wave=True),
        by_area("sheet_area_3dface", _faces_lisp(*mixed), ("Face",) * (len(up) + len(down))),
        by_area("sheet_area_pface", _pface_lisp(*mixed), ("PolyFaceMesh",)),
        by_area("sheet_area_mesh", _mesh_lisp(*mixed), ("SubDMesh",)),
        box("closed_3dface", _faces_lisp(*closed), ("Face",) * 6),
        box("closed_pface", _pface_lisp(*closed), ("PolyFaceMesh",)),
        box("closed_mesh", _mesh_lisp(*closed), ("SubDMesh",)),
    ]


FAR = 1_000_000


def _scaled(base, kind, factor=1.0, offset=0):
    def times(p):
        return tuple(v * factor + offset for v in p)

    lisp = base.lisp
    if factor != 1.0:
        lisp += f" (corpus-scale-all {factor})"
    if offset:
        lisp += f" (corpus-move-all '({offset}.0 {offset}.0 {offset}.0))"
    return Shape(
        "scale",
        f"scale_{base.family}_{kind}",
        lisp,
        base.entities,
        "",
        base.volume * factor**3 if base.volume else None,
        base.area * factor**2 if base.area else None,
        (times(base.bounds[0]), times(base.bounds[1])) if base.bounds else None,
    )


def _scales(shapes):
    by_name = {s.name: s for s in shapes}
    result = []
    for name in ("tube_090", "fillet_large", "trim_sphere_hole"):
        base = by_name[name]
        result += [
            _scaled(base, "small", 0.01),
            _scaled(base, "large", 10000.0),
            _scaled(base, "far", offset=FAR),
        ]
    return result


SHAPES = [
    *_primitives(),
    *_tubes(),
    *_revolves(),
    *_fillets(),
    *_sweeps(),
    *_lofts(),
    *_trims(),
    *_regions(),
    *_surfaces(),
    *_inverted(),
    *_sheets(),
]
SHAPES += _scales(SHAPES)
