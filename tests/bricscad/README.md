# BricsCAD geometry corpus

DWG files made by BricsCAD from scripts: one known shape in each drawing, in
every DWG version from R14 to 2018, each with a right answer. They test the
converter's geometry without a reference converter, and they are made for
this repository, so they can be kept in git.

`report/v27.md` shows how a converter build does on them. A file listed as
`differs` or `failed` is usually a known bug, and the corpus keeps it in view
until it is fixed. These files do not prove the converter is right, only that
nothing they cover is broken, so add a shape when a new failure appears.

## Files

| Path | What |
|---|---|
| `catalog.py` | The shapes: how each is drawn and its exact values |
| `generate.py` | Runs BricsCAD to build, save and reopen each shape |
| `scripts/lib.lsp` | LISP helpers the shapes use |
| `measure.py` | Converts each file, compares it, writes the report |
| `glb.py` | Reads a GLB's triangles, without numpy |
| `dwg/v<major>/` | The drawings of one BricsCAD version, and `expected.json` |
| `report/` | The latest report |

## Generate

From WSL, with BricsCAD on Windows. Each shape takes about 20 seconds, the
whole corpus about half an hour. BricsCAD windows open and close while it runs.

```sh
python3 tests/bricscad/generate.py --bricscad 'C:\Program Files\Bricsys\BricsCAD V27 en_US'
python3 tests/bricscad/generate.py --bricscad '...' --family tube    # one family
python3 tests/bricscad/generate.py --bricscad '...' --shape tube_090 # one shape
```

The files go in `dwg/v<major>/`, by the version BricsCAD reports, so another
BricsCAD version sits next to `v27` and leaves it alone. If a shape times out
the generator stops only the process it started, never a BricsCAD you have open.

Every saved file is reopened in a fresh BricsCAD, which shows what saving down
did. In `expected.json`, `status` is `ok`, `changed` (reopened as something
else) or `failed` (with a `reason`, and no file is kept).

## Measure

```sh
python3 tests/bricscad/measure.py --root tests/bricscad/dwg/v27 --binary target/release/dwg2gltf --commit "$(git rev-parse --short HEAD)"
```

For each file it checks that nothing was skipped, and compares bounds, area,
volume and facing with the right answer. It then compares each shape's versions
with each other, which catches a bug even where the right answer is uncertain.

Right answers come from a formula where a shape has one, otherwise from
BricsCAD after reopening, never from the converter. For facing, a closed shape
must face outward, and an open sheet must have all its faces pointing the way
most of its area does.

Files are converted in the drawing's own units, because the converter's
tolerances depend on them. `AC1014` gets `--units mm`: R14 cannot hold INSUNITS,
and the converter takes it as meters.

Tolerances are constants at the top of `measure.py`. Faceting explains small
differences, so volume allows 1 % plus area x 0.1 mm. Do not widen one to make a failure pass.

A part under 1 mm thick is too fine for its area and volume to mean anything at
0.1 mm chord deviation: only its bounds are checked, and the report lists it as
`bounds only`, not `match`.

## Rust tests

`crates/dwg2gltf/tests/bricscad.rs` and `bricscad_surfaces_convert_in_every_version`
in `crates/dwg2gltf/tests/convert.rs` check the files a fix was made for
against `expected.json`, as part of `cargo test`.

## Add a shape

1. Add it to `catalog.py`, in the family it belongs to or a new one, with
   exact values where a formula exists. Keep one case in each drawing.
2. Set `qa` to the failure it stands for.
3. Run `python3 -m unittest discover -s tests/bricscad`, then `generate.py --shape <name>`.
4. Measure and check the report.

From a script, BricsCAD needs care in a few places:

- A command left open swallows the next call: end with `(command)`, as the
  helpers in `lib.lsp` do.
- FILLET takes one entity pick per run, picks depend on the view (use
  `corpus-fillet`), and chain selection does not work.
- PLANESURF hangs when given corners. Use CONVTOSURFACE on a region.
- A dialog stops the run until the timeout. The template's Architectural
  LUNITS with INSUNITS 4 raises one on reopening, so `lib.lsp` sets LUNITS 2.
