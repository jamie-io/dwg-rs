# dwg-rs

A DWG <-> glTF toolkit in Rust:

- `dwg2gltf` converts AutoCAD DWG drawings to glTF 2.0 (`.glb` or `.gltf`):
  meshes, curves, block instances, materials and textures, and ACIS solids
  and surfaces, tessellated by its own B-rep reader (no ACIS kernel needed).
  It reads the OFML extension data some exporters store in DWG files
  (per-vertex UVs and normals, embedded material archives, texture mapping).
- `gltf2dwg` converts glTF, OBJ and STL meshes back to DWG.
- `three-dwg-loader` runs the same converter in the browser, compiled to
  WebAssembly, as a three.js loader and exporter.

Self-contained executables: DWG decoding and encoding by
[LibreDWG](https://www.gnu.org/software/libredwg/), vendored and compiled
in, so nothing else needs to be installed. Builds with just Rust and a C
compiler: no autotools, no system libraries, no network access (the Rust
crates are vendored in `vendor/` too).

## Build

Needs Rust 1.85 or later ([rustup](https://rustup.rs)) and a C compiler
(Xcode command line tools, gcc/clang, or mingw-w64).

```sh
cargo build --release                  # -> target/release/dwg2gltf, gltf2dwg
cargo install --path crates/dwg2gltf   # or install them as commands
```

All dependencies are in the repository: LibreDWG's sources (patched, in
`crates/libredwg-sys/libredwg/`) and every Rust crate (`vendor/`, used through
`.cargo/config.toml`), so a checkout builds offline (`cargo build --release
--offline`). After changing dependencies, run `cargo vendor --locked vendor`
and commit `vendor/`.

Checked on macOS (arm64), Linux (arm64 and x86_64, gcc) and Windows x64
(cross-compiled with `--target x86_64-pc-windows-gnu` and mingw-w64). The
result depends only on the system's C runtime.

## Usage

```sh
dwg2gltf chair.dwg                  # -> chair.glb next to the input
dwg2gltf chair.dwg -o chair.gltf    # embedded-buffer .gltf
dwg2gltf drawings/ -o out/          # whole folder, recursively (subfolders kept)
```

Options: `--no-lines` drops wire geometry, `--plan-symbols` keeps the lines
of OFML 2D plan symbols (on layers `72_*_D2*` or in blocks `PEC_GFJ_GEO*`,
left out by default), `--units mm|cm|m|in|ft|...|auto` overrides the drawing
units, `--z-up` keeps DWG axes, `--format gltf` writes `.gltf` when `-o` is a
directory. Two inputs that would write the same output file (`chair.dwg`
next to `chair.DWG`, or `model.glb` next to `model.stl` for `gltf2dwg`) are
refused instead of overwriting each other.

A drawing with nothing to convert (no triangles and no lines, for example
only text or only paper space) is an error: no file is written, the summary
says what was skipped, and `--allow-empty` writes the empty file anyway. In
a folder run every file is converted regardless of the others; a file that
fails is reported as `FAILED <file>: <reason>` (an internal error, a bug, as
`FAILED <file>: internal error: <message>`) and the run goes on.

Exit codes:

| Code | Meaning |
| --- | --- |
| 0 | every input converted |
| 1 | at least one input failed (unreadable, internal error, cannot write) |
| 2 | bad arguments, or no input files found |
| 3 | no input failed, but at least one had nothing to convert (no file written for it) |

`--seriesdwg` follows the same rules: a series DWG whose blocks all have
nothing to show is code 3 unless `--allow-empty`.

### Warnings

Besides the `skipped:` counts (each skipped entity is counted once, with one
reason), these losses are reported as `warning:` lines:

- `LibreDWG reported recoverable decode errors (0x...): output may be
  incomplete`: LibreDWG skipped parts of the file it could not decode (the
  bits are its `DWG_ERR_*` codes).
- `paper space has N entities that are not converted`: only model space is
  converted; layouts are not. `model space is empty, but paper space has N
  entities, which are not converted` when that is all there is.
- `INSERT of external reference <name> (not resolved)`: the block lives in
  another drawing, which is not read (counted as `skipped: INSERT (xref)`).
- `the drawing needs more than N block instances|triangles|line points (the
  limit); the rest is left out` (see Limits).
- `the drawing has no units (INSUNITS 0) ...`, `the drawing is less than 1 cm
  across ...` (see below) and the series DWG hint.

### Limits

Crafted or damaged drawings could make the converter allocate without end
or recurse until the stack overflows. These limits stop that; each was
measured (raised until the process was killed or overflowed, then set below
that point; see `crates/dwg2gltf/src/limits.rs` and
`examples/limit_probe.rs`). Real drawings stay far below them:

| Limit | Value | Measured crash point | Largest real value seen |
| --- | --- | --- | --- |
| block instances per drawing (MINSERT cells included) | 1 000 000 | ~1.4 M MINSERT cells or ~3.3 M nested instances at 8 GB | 5 708 |
| triangles per drawing (before instancing) | 10 000 000 | ~13.5 M at 8 GB | 2.2 M |
| polyline points per drawing | 60 000 000 | ~95 M at 8 GB | ~142 000 |
| INSERT nesting depth | 1 000 | 2 000-5 000 on a 2 MiB stack, 20 000-50 000 on 8 MiB | 16 |
| SPLINE degree | 100 | degree 2000: 82 s for one spline | 5 (AutoCAD allows 25) |
| ACIS subtype nesting | 10 000 | 20 000-40 000 on a 2 MiB stack | 8 |
| triangles per ACIS face | 100 000 | (time: 3.4 s per face at the limit) | |

The memory limits are sized so one drawing stays below about 6 GB; reaching
two of them in the same drawing can add up. A block that inserts itself
(directly or through other blocks; AutoCAD refuses these) is left out as
`INSERT (block inserts itself)` instead of being repeated down to the nesting limit.
Values that are garbage rather than large are rejected without a limit: an
arc sweeps at most one full turn, non-finite angles give no geometry, and an
ACIS spline whose knots ask for more control points than its data holds is
not read. Running out of memory or stack outside these limits still aborts
the process (Rust cannot catch that); please report such a file.

### Drawings in meters that say millimeters

```sh
dwg2gltf --units auto drawings/ -o out/
```

Some exporters of OFML geometry store drawings in meters but declare
millimeters (INSUNITS 4), so they come out 1000 times too small. Nothing
inside a file tells which is right, and the same folder can hold genuine
millimeter parts too. So the units are only changed on request:

- A drawing in millimeters, centimeters, decimeters, inches, feet or yards
  that comes out less than 1 cm across gets a warning.
- `--units auto` takes those drawings as meters and keeps the others. A
  series DWG is judged by its largest block, for all of its blocks.
- `--units m` takes every drawing as meters.

### Series DWGs (block libraries)

```sh
dwg2gltf --seriesdwg library.dwg -o out/   # -> out/library/<block>.glb, one per article
```

A series DWG keeps its articles as block definitions and leaves model space
empty, so a plain conversion gives an empty file (and a warning that points
here). `--seriesdwg` (alias `--blocks`) writes every named block that no
`INSERT` uses, with its base point at the origin; blocks it inserts stay
instances inside it. File names are the block names, made safe for Windows;
names equal apart from case get `~2`, `~3`, ... Blocks with nothing to show
(only annotation or hidden layers) are listed and skipped. A drawing without
such blocks fails with a hint to convert it without `--seriesdwg`.

### Compressed output

```sh
dwg2gltf chair.dwg --compress       # about 2.5x smaller, for the web
```

`--compress` optimizes and compresses the file the way gltfpack and
glTF-Transform do:

- vertex positions are stored as 16-bit integers over each mesh's bounds and
  normals as 16-bit (`KHR_mesh_quantization`);
- triangles and vertices are reordered for the GPU vertex cache;
- vertex and index data are compressed with meshoptimizer's codecs
  (`EXT_meshopt_compression`);
- textures are re-encoded as WebP at quality 85 when that is smaller
  (`EXT_texture_webp`); normal maps losslessly.

The error is at most half a quantization step, 1/(2 x 65535) of a mesh's
size (0.015 mm for a 2 m sofa).
three.js, Babylon.js, model-viewer and glTF-Transform read these files.
Importers without meshopt support (Blender 4.2, for one) do not, so plain
output stays the default. Equal textures are stored once either way.

## Convert into DWG

```sh
gltf2dwg chair.glb                     # -> chair.dwg next to the input
gltf2dwg model.obj -o out/             # OBJ (+ MTL), STL, .gltf work the same
gltf2dwg chair.glb --units mm          # drawing units (glTF meters are scaled)
gltf2dwg part.stl --input-units mm     # OBJ and STL carry no units (default m)
gltf2dwg chair.glb --dwg-version r2013 # r2010, r2013 or r2018 (default)
```

Each mesh becomes a `MESH` entity (split above about a million faces, the
most LibreDWG's encoder writes); a mesh used several times, also through
`EXT_mesh_gpu_instancing`, becomes a block with one `INSERT` per use. Each
material becomes a layer in its color, a DWG `MATERIAL`, and an embedded
OFML material archive with the PBR values and the texture, with the UVs,
normals and double-sided flag stored on the mesh as OFML records. OFML
applications show the textures; AutoCAD and BricsCAD show the colors.
`--z-up` keeps the input's axes (glTF and OBJ are taken as Y-up, STL as Z-up).
Faces without normals get the ones their format means: flat for glTF and
STL, and for OBJ flat or smoothed by their `s` smoothing group.

Files are written uncompressed (LibreDWG cannot compress yet), so even a small
drawing takes about 300 KB.

## three.js loader

```sh
rustup target add wasm32-wasip1   # once, plus wasi-sdk in ~/opt/wasi-sdk (or $WASI_SDK_PATH)
scripts/build-wasm.sh             # -> packages/three-dwg-loader/wasm/dwg2gltf.wasm
```

```js
import { DWGLoader } from 'three-dwg-loader';
scene.add( await new DWGLoader().loadAsync( 'chair.dwg' ) );
```

`packages/three-dwg-loader` is a three.js loader. It needs no change to
three.js. The converter (`crates/dwg2gltf-wasm`, LibreDWG included) runs as
WebAssembly in a worker and writes the same GLB as `dwg2gltf`. three's
`GLTFLoader` then builds the scene. Series DWGs become one object per
article. `DWGExporter` writes three.js objects as DWG, with the `gltf2dwg`
code. The module is about 1.3 MB with brotli. Conversion takes 1.3 to
1.5 times as long as with the native tool. Options, errors, deployment and
tests are in [its README](packages/three-dwg-loader/README.md). Like
everything here, it is GPL-3.0-or-later.

## What gets converted

| DWG | glTF |
| --- | --- |
| `MESH` (SubDMesh), `POLYLINE` polyface/polygon mesh, `3DFACE`, `SOLID`/`TRACE` | triangles with smoothed normals (30 degree crease; a `MESH` without normals at the angle in its `EDWG_NORMAL_SMOOTHNESS` record, or 53 degrees) |
| `LINE`, `ARC`, `CIRCLE`, `ELLIPSE`, `LWPOLYLINE` (bulges), 2D/3D `POLYLINE`, `SPLINE` | `LINES`; with thickness they become extruded surfaces (a circle a closed cylinder, a `SOLID` a prism); polylines with a width become filled bands |
| `3DSOLID`, `BODY`, `REGION`, `SURFACE` and the other ACIS surfaces | triangles, tessellated from the B-rep (see below) |
| `INSERT` / `MINSERT` | one node per instance sharing the block's mesh |
| layer/ByBlock colors, `MATERIAL` objects, embedded OFML material archives | PBR materials, embedded textures |
| `INSUNITS` | scale to meters; Z-up becomes Y-up |

OFML extension data: per-vertex UVs from the `EDWG_TEX_COORDS_2D` XRecord
(or AutoCAD's `ADSK_XREC_SUBDVERTEXTEXCOORDS`), vertex normals from
`ADSK_XREC_SUBDVERTEXNORMALS`, materials embedded as zip archives
(`material.mat` + textures) under `EDWG_MATERIAL_DICT`, base materials
linked through `EDWG_BASE_MATERIAL`, OFML `auto`/`prjx|y|z`/`rotate`
texture mapping, AutoCAD material mappers (`ACAD_MATERIAL_MAPPER`), layer-0
and ByBlock inheritance from the INSERT, double-sided faces, 0.1 mm curve
deviation. `MESH` is drawn as its level-0 cage.

Hidden/frozen layers are skipped (an `INSERT` on an off layer still shows
what its block draws on other layers, as in AutoCAD). Every run prints what
was converted, what was skipped and why, and what had to be repaired.

### Orientation

Every triangle is wound counter-clockwise around its front, and its normals
point the same way:

- ACIS faces face outwards by their face and surface sense, so solids are
  closed and outward.
- Faces stored inside out are turned to match their neighbours, and a
  closed shell that faces inwards is turned outwards (`repaired: triangle
  turned to match its neighbours`). Examples are a polyface wound one way
  in places and the other way elsewhere, or a pyramid of `3DFACE`s wound
  inwards.
- A `MESH` triangle whose winding disagrees with its stored vertex normals
  is turned to face them.
- Self-intersecting (bow-tie) and non-planar polygons are split so that no
  triangle faces backwards.

The tests check this on synthetic drawings, and `tests/bricscad/measure.py`
checks the facing of every BricsCAD test drawing.

## ACIS solids

The converter reads the solid modeler data itself (no ACIS kernel needed):

- **Storage**: SAT text (R2000 to R2004, decrypted by LibreDWG), SAB binary
  inline in the entity (R2007 to R2010, some R2013+), and SAB in the R2013+
  `AcDsPrototype_1b` data-storage section, including streams paged across
  `blob01` segments. ACIS 4.0 up to ASM 223 (`ASM BinaryFile4`).
- **Geometry**: planes, cones/cylinders, spheres, tori and NURBS surfaces;
  straight, elliptic and NURBS edge curves; body transforms (for example
  bodies stored in mm, Y-up, with a transform into the drawing).
- **Procedural surfaces** (blends, sweeps, offsets) use their embedded NURBS
  approximation when it fits the face boundary. Pipes and rolling-ball
  blends saved without one are evaluated from their spine (a circle swept
  along it). Anything else gets a Coons patch over the boundary (3/4-sided
  faces) or a flat triangulation.
- **Tessellation**: boundary edges are sampled once per edge (watertight),
  faces are triangulated in their parameter domain (seams of periodic
  surfaces, including closed NURBS, and poles closed on the face's side,
  holes bridged), Delaunay-flipped and refined until the surface deviates
  less than 0.1 mm (and never finer than 0.1 % of the body size). Faces
  come out facing outwards (ACIS face sense and surface sense).
- **Fallbacks are reported**: a face without usable surface data is filled
  from its boundary, and the summary says so.

## Limitations

- Text, dimensions, hatches, leaders, viewports, lights and images are not exported.
- Dynamic blocks are exported in their current state (LibreDWG's anonymous block).
- `gltf2dwg`: AutoCAD/BricsCAD show material colors, not textures (DWG
  materials refer to external image files). Not written: glTF animation,
  skins, cameras, lights, vertex colors. Texture transforms are applied to
  the UVs and morph targets at the mesh's default weights. Reads
  meshopt-compressed and quantized files and WebP textures (stored as PNG
  in the material archive); a file that requires another extension (Draco,
  KTX2, ...) is refused. LibreDWG's r2010+ writer is new upstream; check
  important files in your CAD program.

## LibreDWG

`crates/libredwg-sys` holds the decoder and encoder sources of a LibreDWG
master snapshot (commit `4f2aef33`, 2026-11-05), which has the security fixes
made since the 0.14 release, with six patches (details in
[its README](crates/libredwg-sys/README.md)):

- The decompressed AcDs section, where R2013+ solids keep their ACIS data, is
  kept for the converter instead of being freed.
- R14 to R2010 procedural surfaces (`EXTRUDEDSURFACE`, `LOFTEDSURFACE`,
  `REVOLVEDSURFACE`, `SWEPTSURFACE`) are decoded as far as their ACIS data,
  instead of being kept as unknown entities.
- The embedded MTEXT of R2010+ `ATTRIB`/`ATTDEF` no longer reads past the end
  of the handle stream on a damaged file.
- Writing: functions to add MESH and MATERIAL objects, and encoder fixes for
  large objects (whose handles were cut off), size limits below what real
  files use, the R2010+ section sizes ODA checks, and materials that were
  replaced by placeholders or dropped from entities.

Two fixes the converter needed are upstream now: LibreDWG 0.14 and older read
the MESH smoothness level (DXF 91) as a vertex count, so every `MESH` with a
non-zero level decoded as garbage, and read the R2007+ plot-style and
material flag bits in swapped order.

## Layout

```
crates/dwg2gltf/            the converter library and the dwg2gltf / gltf2dwg commands
crates/dwg2gltf-wasm/       the WebAssembly module's C ABI for the three.js loader
crates/libredwg-sys/        LibreDWG bindings, vendored C sources and their patches
packages/three-dwg-loader/  three.js loader and exporter (npm package, WebAssembly)
tools/qa/                   comparison of converted GLBs with reference GLBs (qa.py) and its helpers
tests/bricscad/             generated BricsCAD drawings and their exact expected values
docs/                       implementation notes
scripts/                    build-wasm.sh
vendor/                     vendored Rust crates (offline builds)
```

`samples/` is ignored by git: a place for your own drawings, conversions and
QA runs.

| Crate / module | Owns |
| --- | --- |
| `libredwg-sys` | vendored LibreDWG, `config.h`, C shim, bindings (64-bit and wasm32) |
| `dwg2gltf::dwg` | decoding a file and copying `Dwg_Data` into plain Rust data |
| `dwg2gltf::model` | handles, layers, colors, units, materials (DWG and embedded OFML material archives), ACIS lookup |
| `dwg2gltf::convert` | entity geometry, block instancing, style inheritance, glTF materials |
| `dwg2gltf::mesh`, `geom`, `gltf` | welding and normals, curve sampling and triangulation, GLB writer |
| `dwg2gltf::compress` | `--compress`: quantization, meshopt coding, WebP textures |
| `dwg2gltf::acis` | AcDs section, SAB/SAT tokens, B-rep, NURBS, tessellation |
| `dwg2gltf::input`, `export`, `dwg_writer`, `ofml` | glTF/OBJ/STL reading, DWG writing (`gltf2dwg`), the OFML records and material archives |
| `dwg2gltf-wasm` | the WebAssembly module's C ABI (`dgl_*`) for the three.js loader and exporter |
| `packages/three-dwg-loader` | `DWGLoader`, `DWGExporter`, their shared worker pool, the WASI shim, demo and Node tests |

[docs/IMPLEMENTATION_NOTES.md](docs/IMPLEMENTATION_NOTES.md) has the
file-format details (LibreDWG quirks, OFML records, ACIS/SAB/AcDs layouts),
the tessellation algorithm and the verification workflow.

## Testing

```sh
cargo test --release                # unit tests, synthetic drawings, BricsCAD drawings
cargo clippy --all-targets
cargo test -p dwg2gltf-wasm         # the WebAssembly module's C ABI, natively
scripts/build-wasm.sh && (cd packages/three-dwg-loader && npm install && npm test)   # wasm against the CLI
```

`tests/bricscad/` holds drawings made by BricsCAD from scripts: one known
shape in each, in every DWG version from R14 to 2018, each with exact
expected bounds, area, volume and facing (see
[its README](tests/bricscad/README.md)). To measure a build against all of
them:

```sh
cargo build --release
python3 tests/bricscad/measure.py --root tests/bricscad/dwg/v27 \
    --binary target/release/dwg2gltf --commit "$(git rev-parse --short HEAD)"
python3 -m unittest discover -s tests/bricscad   # the generator's and measurer's own tests
```

It writes `tests/bricscad/report/v27.md` and prints the counts (`match`,
`bounds only`, `differs`, `failed`).

## Comparing with reference GLBs

`tools/qa/qa.py` converts a folder of DWGs, compares every GLB with a reference GLB of the same
drawing (from any other converter), renders both with Blender, writes an HTML report and diffs runs.

```sh
python3 -m venv .venv
.venv/bin/pip install numpy scipy pillow scikit-image
```

Renders need Blender (`BLENDER=/path/to/blender`, default `/usr/bin/blender`); Noto Sans fonts are
used for collages when present. `node tools/qa/validate.js *.glb` runs the Khronos validator
(`npm install gltf-validator`).

### Layout

DWGs go in a folder (default `samples/`, git-ignored), directly or in top-level subfolders
("groups"). The reference of `<dir>/<stem>.dwg` is `<dir>/ref/<stem>.glb`; DWGs without one are
counted and skipped. A drawing is identified as `<group>/<file>`.

```
samples/
  a.dwg             ref/a.glb
  solids/b.dwg      solids/ref/b.glb
  surfaces/c.dwg    surfaces/ref/c.glb
```

### Running

```sh
.venv/bin/python tools/qa/qa.py run                                  # build, convert, compare, render, report
.venv/bin/python tools/qa/qa.py run --bin path/to/dwg2gltf --skip-render
.venv/bin/python tools/qa/qa.py run --subset 40 --group solids --skip-collage
```

| Option | Meaning |
| --- | --- |
| `--samples DIR`, `--out DIR` | input folder (default `samples/`), output folder (default `samples/compare/`) |
| `--bin PATH` | use this binary instead of `cargo build --release` |
| `--opt ARG` | extra dwg2gltf option, part of the cache key (repeatable) |
| `--subset N`, `--group G ...` | a fixed sample of N drawings; only these subfolders |
| `--skip-render`, `--skip-collage` | no Blender renders at all; no collages, slides or shots |
| `--plugins FILE ...` | extra metrics |
| `--workers N`, `--timeout 600` | parallel convert+compare processes (default: all cores, fewer when memory is short, about 2.5 GB each); seconds per conversion |
| `--keep-glbs 4` | GLB cache cap in GB, `all` or `none` |
| `--min-free-gb 12` | refuse to start with less free disk space |

Limited runs (`--subset`, `--group`, `--skip-render`) do not move `latest` unless `--set-latest`.

Every drawing is converted with the new binary, but a comparison is only computed when the GLB is new:
results are cached by the GLB's content (`cache/compare-glb/`), so a change that touches a few drawings
compares only those. Drawings run slowest first, using the times of the previous run, so one long
conversion does not hold up the end of a run.

| Output (in `--out`) | Content |
| --- | --- |
| `runs/<id>/results.csv` | one row per DWG: status, score, categories, every metric |
| `runs/<id>/run.json` | converter, options, thresholds, metric code hash, counts, timings |
| `runs/<id>/report/index.html` | warn/fail rows worst first: reference, ours, SSIM difference; a passing sample |
| `runs/<id>/collage/`, `shots/` | collage and slide sheets of the selection, `failures_NN.png` |
| `runs/<id>/renders_index.json` | rendered images per drawing |
| `latest`, `renders_index.json` | the last full run |
| `diffs/<a>__<b>/` | `index.html` and `diff.json` of `qa.py diff` |
| `cache/` | converted GLBs, comparisons, renders, file hashes |

The run id is `<date>-<commit>[-dirty]-<binary sha256>`. Our GLBs are cached by DWG hash, binary hash
and options, comparisons also by reference hash and metric code, renders by settings and camera box,
so an unchanged drawing is not converted, compared or rendered again. GLBs of passing drawings are
deleted after the comparison and converted again when a render needs them; `qa.py prune` applies
the same policy to the whole cache.

### Diff and selection

```sh
.venv/bin/python tools/qa/qa.py diff [A] [B]      # default: last full run with another binary vs latest
```

A regression is a worse status or a score drop of more than `--score-drop` (10). Exit code 1 when
there are more than `--max-regressions` (0). Drawings whose DWG or reference changed are listed,
not judged. `qa.py select` picks passing drawings for collages, slides and shots and writes
`<out>/selection.json` (`--selection PATH` elsewhere); without that file those images are skipped.

### Plugins

A plugin is a Python file with `METRICS = [fn(ctx, res), ...]`; each function reads the prepared
context (both meshes in the reference's frame, surface samples, nearest-point queries; see
`compare.prepare`) and writes columns into `res`. Plugin files are part of the metric code hash.

### Metrics and thresholds

Both meshes are brought into the reference's frame (a constant axis or unit transform is detected
and fails), duplicate faces removed, 30,000 surface points sampled per side. Distances are
relative to the bounding-box diagonal; a point farther than 1 % of it lies on a part the other side
lacks. A value at or below *warn* passes, above *fail* fails.

| Check | Warn | Fail | Measures |
| --- | --- | --- | --- |
| chamfer | 0.002 | 0.01 | symmetric mean surface distance |
| haus95 | 0.01 | 0.05 | 95th percentile of the larger one-sided distance |
| extent_err | 0.01 | 0.05 | largest relative bounding-box size difference |
| area_err | 0.10 | 0.30 | \|log(area ratio)\| without duplicate faces |
| missing | 0.01 | 0.05 | share of the reference surface with nothing of ours nearby |
| extra | 0.01 | 0.05 | share of our surface with nothing of the reference nearby |
| inside_out | 0.01 | 0.05 | share of coincident surface with opposite normals |
| color | 0.10 | 0.25 | mean sRGB distance of base colour on coincident surface |
| texture | 0.10 | 0.40 | share of coincident surface textured on one side only |
| normal_map | 0.10 | 1.01 | share with a normal map only in the reference (warns only) |
| tex_scale | 0.18 | 0.41 | \|log\| of the median texture repeats-per-metre ratio |
| center_off | 0.01 | 0.05 | bounding-box centre offset (placement; warns only) |

The score starts at 100 and loses 10 to 40 per check above *warn*, 40 for a transform and 5 for an
offset. `inside_out` is not judged when more than 30 % of the reference's faces are duplicated with
both windings.

## License

Copyright (C) 2026 Jamie Jahn. GPL-3.0-or-later, see [LICENSE](LICENSE).
LibreDWG (GPL-3.0-or-later, Free Software Foundation) is included in
`crates/libredwg-sys/libredwg/`, taken from upstream master (commit
`4f2aef33`) plus the patches listed above.
