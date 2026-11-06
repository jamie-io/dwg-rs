# Implementation notes

Reference for maintaining dwg2gltf. The Rust implementation in `crates/` is
the only one. It is a port of an earlier Python implementation (not
included here), which gave the same results
until the geometry review of section 12, whose fixes exist in Rust only. This file records what the code
alone doesn't tell you: file-format facts, why things are done a certain
way, bugs already hit, and how results were verified.

## 1. Pipeline

```
DWG ──LibreDWG decoder (crates/libredwg-sys, patched)──► Dwg_Data
    ──dwg::read──► Drawing: plain objects, entities, raw AcDs section
    ──model::Document──► handles, layers, blocks, materials, ACIS streams
    ──convert::Converter──► per block × inherited style: triangles/lines
    ──gltf::GltfWriter──► GLB / .gltf
```

The Python version differed in the first two steps only: it ran `dwgread`
and read its JSON (plus the AcDs section dumped to a file) into dicts.

The reverse direction (Rust only, the `gltf2dwg` command) is described in
section 9.

| Rust (`crates/dwg2gltf/src/`) | Python (removed) | Owns |
| --- | --- | --- |
| `libredwg-sys` crate | `libredwg.py` | decoding (Rust: in process; Python: `dwgread` subprocess, JSON repair) |
| `dwg.rs` | (JSON dicts) | copying `Dwg_Data` into plain data, LibreDWG quirks |
| `model.rs` | `model.py` | document access, colors (ACI table), units, layers, materials (DWG MATERIAL + embedded OFML material archive), material refs, ACIS data lookup |
| `convert.rs` | `convert.py` | entity → geometry, block instancing, style inheritance, glTF materials |
| `mesh.rs` | `mesh.py` | triangle welding, normals, UVs; line batches |
| `geom.rs` | `geom.py` | vectors, 4×4 matrices, OCS, arc/bulge/B-spline sampling, ear clipping with holes |
| `gltf.rs` | `gltf.py` | glTF 2.0 document + binary buffer, GLB container |
| `acis/sab.rs` | `acis/sab.py` | SAB (binary) and SAT (text) tokenizer → records |
| `acis/brep.rs` | `acis/brep.py` | records → bodies/faces/loops/edges, curve and surface evaluators |
| `acis/tessellate.rs` | `acis/tessellate.py` | face triangulation in the uv domain |
| `acis/acds.rs` | `acis/acds.py` | ACIS streams from the R2013+ data-storage section |
| `acis/nurbs.rs` | `acis/nurbs.py` | B-spline curve/surface evaluation |

## 2. Reading DWG with LibreDWG 0.14

The Python version ran `dwgread -O JSON -o out.json file.dwg`; its patched
build also wrote the decompressed `AcDsPrototype_1b` section to
`$LIBREDWG_ACDS_DUMP`.

JSON quirks (all handled in the Python `libredwg.py` / `model.py`):

- Non-finite doubles appear as bare `nan`, `-nan`, `inf` tokens; huge doubles
  with a trailing dot (`5957…712.`). Both are invalid JSON and must be repaired
  before parsing.
- Handle references are `[code, size, value, absolute]`; use the last element.
  An object's own handle is `[0, size, value]`.
- Block contents: `BLOCK_HEADER.entities` (R2004+). Fallback: entity
  `entmode` 2 = model space, 1 = paper space, 0 = `ownerhandle`. Polyline
  vertices: `vertex` refs, else owner, else `first_vertex..last_vertex`.
- Colors: `{"index", "rgb", "flag"}`. `rgb` is 8 hex digits; top byte is
  the method: C2 = true color, C3 = ACI (low byte), C0 = ByLayer,
  C1 = ByBlock. `index` 256 = ByLayer, 0 = ByBlock. A color without `index`
  is ByBlock. Pre-R2004 files give a bare integer.
- Layers: `flag0` bit 0 frozen, bit 1 off; negative color index also means off.
- **Flag swap (R2007+)**: LibreDWG 0.14 reads the plot-style and material
  flag bits in swapped order. A flag value of 3 in `plotstyle_flags` comes
  with a `plotstyle` handle that points to a MATERIAL. Every AutoCAD- and
  ODA-written file seen shows this. `Document.material_ref` resolves the
  material by object type, so it keeps working with a fixed LibreDWG (master
  since 76cf0d750, which the Rust build vendors, section 11).
- **MESH bug**: DXF 91 is the subdivision level, but LibreDWG reads it as
  the size of a vertex array, so every MESH with level > 0 decodes as
  garbage. Fixed upstream (PR #1435, in the vendored LibreDWG); the Python
  build patched its LibreDWG 0.14.
- `version` in `FILEHEADER` (AC1015 = R2000 … AC1032 = R2018).
- Entity graphics (`preview`) of MESH entities only hold edges (proxy
  graphics opcodes 6/19), not faces. Not usable as a fallback.

Known LibreDWG errors that don't matter here: `Invalid MESH.edges rcount1`
on very large meshes (data still decodes), `Invalid preview_size` on DXF import.

### 2.1 Reading `Dwg_Data` directly (Rust)

`dwg.rs` copies what the converter needs out of LibreDWG's structs and frees
them. What the JSON hid:

- LibreDWG keeps decoder and encoder state in static variables (the `REPEAT`
  loop counters `rcount1`/`rcount2` in decode.c, encode.c and free.c, the
  current version, error counts). Two tests reading in parallel
  threads corrupted each other (`NoModelSpace` on a valid file), so every
  read and every `DwgBuilder` holds `libredwg_sys::lock()` until its
  `Dwg_Data` is freed.
- `dwg2gltf_decode` (in `libredwg-sys/shim`) does what `dwg_read_file` does
  after reading the file: Rust reads the file, so any path works on any
  platform. A result of `DWG_ERR_CRITICAL` (128) or more is a failed decode,
  the same rule as `dwgread`'s.
- Text fields (`BITCODE_T`) are UCS-2 in R2007+ files and codepage strings
  before; `dwg2gltf_text` converts both to UTF-8.
- Entity colors of R2004+ files are stored as ENC: `index` (0..511) plus an
  RGB only when flag 0x80 is set, `method` unset. Table colors are CMC: the
  method is the top byte of `rgb` and `index` is recomputed by palette lookup.
  `dwg::Color` reproduces what the JSON printed (index if non-zero; RGB only
  when it carries a method byte), so `parse_color` is the same as in Python.
- `LAYER.off` / `frozen` are decoded for every version (the JSON `flag0` is
  R2000+ only); pre-R13 off layers also have a negative color index.
- `BLOCK_HEADER.entities` exists for R2004+ only; older files fall back to
  the owner handles, as in Python.
- `SPLINE.ctrl_pts[].w` is 0 unless `weighted` is set; the weight is then 1.
- `3DSOLID.version` < 2: `acis_data` is the decrypted SAT text; otherwise
  `sab_size` bytes of SAB. The R2013+ copy of the AcDs data is ignored, as in
  Python (section 4.1).
- The JSON prints doubles as `%.14f`, so Python sees coordinates rounded to
  14 decimals; Rust reads the exact values. That explains the triangle count
  differences between the two: with its input rounded the same way, Rust gives
  Python's counts.

## 3. Scene conversion

- **Units**: INSUNITS → meters (table in `model.py`, 0 = unitless → 1.0).
  Some OFML drawings say millimeters for meter coordinates: a warning,
  and `--units auto` (section 14).
  Root node: scale × (Z-up → Y-up), row-major
  `[[s,0,0,0],[0,0,s,0],[0,-s,0,0],[0,0,0,1]]`. glTF matrices are column-major.
- **Deviation**: 0.1 mm (`1e-4 m / unit`). Arc segments from the sagitta,
  clamped to 8..256 per full circle.
- **INSERT**: `M = OCS(extrusion) · T(ins_pt) · Rz(rotation) · [T(col·dx, row·dy)] · S(scale) · T(−base_pt)`;
  `ins_pt` is in OCS. MINSERT repeats over rows/columns. OCS uses the
  AutoCAD arbitrary-axis algorithm (|n.x|,|n.y| < 1/64 → Y × n, else Z × n).
- **Inheritance**: an INSERT hands down (color, layer, material). Entities on
  layer "0" take the INSERT's layer; ByBlock color/material take the
  INSERT's. Geometry is cached per (block, inherited style), so a block is
  one shared glTF mesh as long as the inherited style is the same.
- **MESH** is drawn as its level-0 cage; no
  Catmull-Clark. Vertex normals from XRecord `ADSK_XREC_SUBDVERTEXNORMALS`
  (doubles, groups 10..59), UVs from `EDWG_TEX_COORDS_2D` (310 binary chunks of
  little-endian OdGePoint2d; NaN → 0; used only if count == vertex count),
  else from AutoCAD's `ADSK_XREC_SUBDVERTEXTEXCOORDS` (reals 43/44/45 = u/v/w
  per vertex, same rules). Many OFML meshes carry only the latter; a box
  projection in their place gave the wrong grain.
  Without normals, a MESH is smoothed at its `EDWG_NORMAL_SMOOTHNESS` angle
  (group 50, degrees) or at 53 degrees.
- **Normals**: area-weighted, merged across faces within 30 degrees (other
  entities), and exactly flipped neighbours are merged too.
  Positions within 1 um of each other are welded (searching the neighbouring
  grid cells too: rounding to a 1 um grid alone split points that straddle
  a cell border). Triangles that are collinear, collapse after welding, or
  would collapse or turn over when stored as float32, are dropped
  (otherwise glTF zero-length normals).
- **Orientation** (`TriangleBuilder::orient`, per block and material): the
  winding is made consistent within each piece joined by edges that exactly
  two triangles share. A closed piece faces outwards (signed volume). An
  open piece follows its triangles of known orientation: stored MESH
  normals first, then ACIS faces. Without those, it keeps the orientation
  of most of its area (an equal split, as in a thin panel of two sheets, is
  a coin flip; that is why ACIS faces anchor).
  - A MESH triangle whose winding disagrees with its stored normals is turned
    to face them first.
  - A triangle with stored normals on the wrong side of its piece is turned
    with its normals negated, unless its normals agree with its neighbours'.
    The exception is a fold in the source: triangles lying back over their
    neighbour, which then keep facing their normals.
  - Polygons are triangulated without backward triangles. A bow-tie quad
    takes the diagonal whose worse triangle faces the quad's normal most. A
    non-planar n-gon (up to 32 corners) gets the least-area triangulation
    (dynamic programming, backward triangles counted tenfold) instead of an
    ear clip of its projection, which left a sliver standing upright in a
    table leg.
  - Every repair is counted in the summary under `repaired:`.
- **Precision**: a block whose coordinates are so large that float32
  would round them by more than the weld distance is written relative to
  the centre of its bounds, with the offset moved into the nodes. Curve
  deviation and weld distance are divided by the largest scale a block is
  inserted at (nested scales multiplied), so a scaled-up block keeps 0.1 mm
  in the world.
- **Materials** (`Document.material`):
  - DWG MATERIAL: `diffuse_color.flag == 1` → `diffuse_color.rgb`
    (C2 method byte); `opacity_percent`; `diffusemap.transmatrix` (2×3 of it
    is the UV transform); `diffusemap.filename`.
  - The exporter that writes the material archive (below) folds its
    `scale sx sy`, `offset ox oy` and `rotate 0 0 θ` (degrees) into
    `diffusemap.transmatrix` as
    M = S·T·R(θ), R = [cos −sin; sin cos] (rotate first, then offset, then
    scale, as OMATS 2.2 §3 orders them), for `import`, `auto` and
    no-mapping materials with a diffuse map; `prjx|y|z` materials keep the
    identity. With both an offset and a rotation the matrix is mostly
    S·R(θ)·T instead (the offset turns with the texture; a few such
    materials match S·T·R, or neither). The transmatrix is therefore used
    as is and the archive values are not applied again (doing so rotated a
    `rotate 0 0 90` texture by 180°). Derived materials have their own
    transmatrix; the base's archive transform does not apply to them.
  - Embedded OFML material archive ("material archive" or "archive"
    below): a zip per material name in an XRECORD (310 chunks) under the
    named-object dictionary `PXCAD_DICT → EDWG_MATERIAL_DICT`. Contents:
    `id.txt`, `material.mat` (lines: `dif r g b`, `metallic`, `roughness`,
    `tex image <ext> <name>`, `bumps`, `scale`, `rotate rx ry rz`, mapping
    `auto` / `prjx|prjy|prjz` / `import`), plus the textures. Archives made
    by other tools name the spec after its material (`diffuse.mat`,
    `chrom.mat`): without `material.mat` the first
    `*.mat` is read. The image is the spec's `tex image` file, else the
    MATERIAL's `diffusemap.filename`, else that name without its
    `<material name>_` prefix (zip entry names compared ignoring case).
  - Alpha: the archive's `tra S` (OMATS transparency, alpha = 1 − S) when
    present, else the DWG `opacity_percent`. Many glass materials have
    `opacity_percent` at 100 % while their archive says `tra 0.87`.
  - Block mapping `auto <triple> [<word>]` (OMATS 2.2 §3.2: U direction on
    the front, right and top sides) is kept as `MaterialDef::auto_mapping`,
    from the archive or from the modifiers in a derived material's name
    (`base; auto xzx yz; rotate 0 0 90`), which win. The word after the
    triple (`yz`, `z`, `xz`) is not in OMATS 2.2. The box
    projection does not use it yet.
  - Derived materials have no material archive of their own. Their extension
    dictionary has `EDWG_BASE_MATERIAL` (340 → base MATERIAL); use the base's
    archive.
  - UVs: explicit UVs win; otherwise box (`auto`) or planar projection of
    block-local coordinates in meters; then `v = 1 − v` for glTF; then the
    material's 2×3 transform, as it goes into KHR_texture_transform:
    scale and offset as they are, the rotation conjugated by the flip
    (`u' = a·u − b·v + tx`, `v' = −c·u + d·v + ty`). Applying the transform
    before the flip shifted a scaled texture by (1 − scale) mod 1.
  - Box (`auto`, OMATS 3.2 block mapping): each face by its dominant normal
    axis; U along Y on the X sides and along X on the Y and Z sides (DWG
    axes), V along Z resp. Y. Seen from outside, U runs the same way round
    the block, so U is negated on the back (+Y), left (-X) and bottom (-Z)
    sides. This is OFML's default triple `xzx`. Not yet
    honoured: other U-axis triples of `auto <front><right><top>` (parsed
    into `MaterialDef::auto_mapping`). The materials seen with other triples
    (yyz, xyx, xzz, yyx ...) have stored UVs on their faces, so the per-side
    signs for those triples are not checked yet.
  - Normal maps: the archive's `bumps <ext> <stem>` is a tangent-space normal map
    (OMATS 2.2 §2.4; no strength parameter, so no `normalTexture.scale`).
    The image is passed through as the archive holds it.
    It uses the base color's UVs; `nscale`/`noffset`/`nrotate` (each falling
    back to `scale`/`offset`/`rotate`) change it relative to them: with B and
    N the S·T·R transforms of the two key sets, the normal map takes
    M·B⁻¹·N where the base color takes M (the material's 2×3 transform,
    both applied after the v flip as above), so its UVs are
    G(M·B⁻¹·N)·G(M)⁻¹ of the glTF UVs (G: a 2×3 map as it acts on the
    flipped UVs, `mesh::gltf_uv_affine`), written as the normalTexture's
    `KHR_texture_transform`
    (omitted when it is the identity, the usual case; dropped when it
    shears, which `KHR_texture_transform` cannot express). A material with a
    normal map but no base color texture and an identity transmatrix takes
    N as its UV transform: the exporter mostly folds the archive transform
    into the DWG diffuse map only, so such a normal map (a `scale 20 20`
    metal grain) has its transform only in the archive; when such a
    material has a transmatrix, that stays.
    `MaterialDef::has_maps` says whether faces need UVs (base color or
    normal map); `convert::normal_transform` gives the transform (not
    `MaterialDef::normal_transform`, which assumes the flip after M); the
    converter registers the image with `GltfWriter::add_image_texture`
    and passes it as `MaterialDesc::normal`.
  - OFML geometry scale: the exporter that writes these records stretches a
    block with the INSERT's scale and records it in the INSERT's
    `EDWG_OFML_MAPPING_INFO` XRECORD (300 text `geoscale sx sy sz`). OFML
    maps textures on the scaled geometry, so the projection uses block
    coordinates times that scale (a sofa body at 1.2 and its cushions at 0.6
    in X keep one 0.4 m weave). Only its size counts: a mirrored instance
    (`geoscale -1 1 1`) keeps sharing the mesh and shows the texture
    mirrored. Without that record an enlarged block is mapped on its scaled
    geometry too, so the projection scale is the product of the |INSERT
    scales| below the nearest record, taking only INSERTs that scale by >= 1
    on every axis (blocks in meters inserted x100 into a centimeter drawing:
    3.3 repeats per meter instead of 0.03). Shrunk blocks are not
    consistent: some files need block coordinates (blocks in mm inserted
    x0.001/x0.01), others the scale, at times two materials under the same
    INSERT disagree; nothing in the MATERIAL or INSERT tells them apart, so
    shrinking INSERTs keep block coordinates. A block shown at several such
    scales gets one mesh per scale only when it has projected textures
    (faces without stored UVs); otherwise its instances share one mesh.
  - AutoCAD material mapper (2026-11-06): DWGs can carry the
    AcGiMapper of an entity or INSERT as XDATA under APPID
    `ACAD_MATERIAL_MAPPER` (LibreDWG raw EED codes): three 70s (projection
    1 planar, 2 box, 3 cylinder, 4 sphere; tiling 0 inherit, 1 tile, 2 crop,
    3 clamp, 4 mirror; auto-transform 1 none, 2 object, 4 model), then four
    11 points: origin O and the ends of the X, Y, Z axes (axes
    `a = end − O`). Faces without stored UVs whose mapper applies are
    projected in the mapper's unit frame `p' = A⁻¹(p − O)` (A = [ax ay az]),
    planar as (x', y'), box as the OFML box above (side by the normal in that
    frame, same mirroring), with no unit or geometry scale; then the v flip
    and the material transform as usual. Which mapper applies: it goes
    with the material. An entity with a ByBlock material takes the mapper
    of the INSERT its material comes from (an INSERT with a ByBlock
    material passes on the one it got; an identity mapper counts too),
    else its own; an entity with any other material its own. An identity
    mapper (O = 0, unit axes) is the entity's coordinates, one repeat per
    drawing unit. Without a mapper the material's projection in meters as
    before. The points are in the coordinates of the entity that carries
    the mapper (for an INSERT: its block's); an INSERT's mapper is applied
    unchanged to the coordinates of whichever block the faces are in, also
    below further INSERTs, not carried through their transforms (not
    checked against AutoCAD). Carried through, a 1000 mm mapper on an
    INSERT in a meter block over faces in millimeter blocks inserted x0.001
    would give one repeat per km instead of one per meter. Texture offset
    and the box sides under nested rotated INSERTs are not checked. Not
    followed: cylindrical and spherical mappers (counted as skipped
    "material mapper (cylindrical, not followed)" with a warning; projected
    as without a mapper), the tiling modes (crop, clamp, mirror are drawn
    tiled) and the auto-transform flag. Mappers on VERTEX_PFACE(_FACE)
    sub-entities are not read (the POLYLINE_PFACE carries one too in the
    files seen).
  - Colors are sRGB; glTF `baseColorFactor` is linear → convert.
  - All triangle materials are double-sided. Lines use KHR_materials_unlit.
  - 2D plan symbols: OFML articles carry their plan view as wire geometry
    on layers `72_<program>_D2_*` / `72_<program>_D2DETAIL_*` (layer-0
    content inheriting such a layer included) and in blocks `PEC_GFJ_GEO*`
    (and blocks nested in them), meant for plan views only. Their lines,
    arcs, circles, polylines and ACIS isolines are left out (counted as
    skipped "2D plan symbol"); faces, thick curves and wide polylines on
    those layers stay. Other wire geometry is still exported unless
    `--no-lines`.
- **Entities LibreDWG does not decode** arrive as `UNKNOWN_ENT` with only
  their common data; they are named by their class (`dwg.rs`). The plain
  `SURFACE` class (`AcDbSurface`) is an ACIS entity whose data is in the
  AcDs section, so it converts like a 3DSOLID. One wall panel has 80 of
  them: the rounded rim of the panel, a third of its triangles.
- **2D entities**: `SOLID`/`TRACE` corners are 1-2-4-3; given around the
  perimeter the outline crosses itself and fills two triangles meeting at
  the crossing (as AutoCAD draws it). With thickness a SOLID is a prism, a
  CIRCLE a closed cylinder; walls of other closed curves face outwards.
  2D polylines with a width (constant or per segment) become filled bands,
  mitred at the vertices, slabs with thickness. Faces of 2D entities point
  along the extrusion direction.
- **Off and frozen**: an INSERT on a frozen layer hides the block; on an off
  layer it hides only what inherits that layer (layer-0 content).
- Annotation (text, dimensions, hatches, leaders, viewports, lights…) is
  skipped and reported. HELIX is unsupported. A drawing without units
  (INSUNITS 0) is taken as meters, with a warning.
- **Losses that are reported** (2026-11-05): each skipped entity is counted
  once with one reason (before, a missing-block or zero-scale INSERT was
  also counted as converted, and unreadable ACIS data twice). ACIS
  reasons: `no ACIS data`, `unreadable ACIS data`, `ACIS data gave no
  triangles`, `triangle limit reached`. Warnings, one line each per file:
  LibreDWG's recoverable error bits (`dwg2gltf_decode` already returned
  them; `dwg::read` used to drop everything below `DWG_ERR_CRITICAL`),
  paper-space entities that would be
  geometry in model space (`*Paper_Space*` block entities, and in pre-R2004
  files entities with `entmode` 1), and INSERTs of xref blocks
  (`BLOCK_HEADER.blkisxref` / `xrefoverlaid` / flag bits 4 and 8; the
  block has no entities, its content is in the other drawing). A drawing
  with no triangles and no lines is `Error::Empty` (exit code 3) unless
  `--allow-empty` (drawings with only text, points, rays, helices, MLINEs,
  an unsupported 2D polyline).

### 3.1 Limits against crafted input (`limits.rs`)

An audit with crafted and mutated files (2026-11) found ways to abort the
process: a block inserting itself twice (2^64 instance paths: OOM), a
MINSERT of 65535 x 65535 cells (a 4.9 GB allocation), an arc with a sweep of
1e300 ("capacity overflow"), a SAT `@<len>` string length that wrapped
`usize`, ACIS subtype blocks nested a million deep (stack overflow while
reading, walking or freeing them), a spline surface whose knot
multiplicities sized a 960 GB control net from 854 bytes. Fixed by:

- **Structural checks, no limit**: a block on the current insert path is not
  inserted again (`INSERT (block inserts itself)`; checked when a block's
  geometry is built, and again on emitting as a fallback). Arc and ellipse
  sweeps are at most one turn (`geom::arc_end`), non-finite angles give no
  geometry. SAT string lengths
  saturate. Knot vectors longer than the remaining tokens, and control nets
  whose `nu * nv * 3` exceeds them, are rejected before allocating.
- **Measured limits** (`examples/limit_probe.rs` builds scaled synthetic
  input in-process; each run under `systemd-run --user --scope -p
  MemoryMax=8G -p MemorySwapMax=0` and `timeout`, peak RSS from `VmHWM`):

| Input | Curve | Kill/overflow point | Limit |
| --- | --- | --- | --- |
| nested INSERT fan-out | 2.45 KB/instance, 2.1 M = 5.1 GB / 7 s | 4.2 M (killed at 8 GB) | 1 M instances |
| MINSERT cells | 5.8 KB/cell, 1 M = 5.8 GB / 9 s | 2 M (killed) | same 1 M budget |
| MESH triangles | ~600 B and ~4 us per triangle; 8 M = 4.8 GB / 33 s, 12 M = 7.0 GB / 55 s | 16 M (killed) | 10 M triangles |
| arc points | ~82 B/point; 25.6 M = 2.1 GB | 256 M (killed), ~95 M estimated | 60 M points |
| SPLINE degree d | O(d^3): 300 = 0.25 s, 1000 = 8.5 s, 2000 = 82 s | (time) | 100 |
| ACIS `{` nesting | linear, 200 000 = 58 MB | 2 MiB thread: 20 000-40 000; 8 MiB main: 200 000-250 000 | 10 000 |
| INSERT chain depth (distinct blocks) | 20 000 = 99 MB / 0.4 s | 2 MiB thread: 2 000-5 000; 8 MiB main: 20 000-50 000 | 1 000 (was 64) |

  The memory limits share a budget of about 6 GB per drawing. They are
  counted once per block definition (instancing adds no triangles), against
  `Options::limits` (`Limits::default()` = the constants; tests pass small
  ones). The entity that crosses a limit is skipped with `(triangle limit
  reached)`, `(line limit reached)` or `(instance limit reached)` and the
  drawing gets one warning per limit. For ACIS the remaining triangle
  budget goes to `Tessellator::budget`, which stops between faces.
- **Per file in a batch**: `files::batch` runs each conversion under
  `catch_unwind`, so a panic is `FAILED <file>: internal error: <msg>` and
  the run continues (exit code 1). OOM and stack overflow abort the
  process; only the limits prevent those.
- `dwg.rs` checks `tio.object` data pointers for NULL as it did for
  entities (a damaged object read as `Data::Other`).

Not covered: tessellation time. A face that reaches the 100 000-triangle
refinement cap takes ~3.4 s, so a crafted file with many such faces can
still run for minutes (within the memory limits).

## 4. ACIS (3DSOLID, BODY, REGION, surfaces)

### 4.1 Where the data is

| Files | Location | Format |
| --- | --- | --- |
| R2000 to R2004 | entity `acis_data`, list of text lines (LibreDWG decrypts) | SAT 4.0 text |
| R2007 to R2010 | entity `acis_data` = `["ACIS BinaryFile", hex]` | SAB |
| R2013+ | `AcDsPrototype_1b` section (`has_ds_data: 1`); LibreDWG sometimes copies it inline, but its copy of paged streams is garbled, so the AcDs copy wins | SAB, ASM ≥ 21800 |

The procedural surfaces (EXTRUDEDSURFACE, LOFTEDSURFACE, REVOLVEDSURFACE,
SWEPTSURFACE) keep their ACIS data in the entity up to R2010 too, but
upstream LibreDWG decodes them only with `DEBUG_CLASSES`: without it they
are UNKNOWN_ENT and the data stays in the unread bits.
`libredwg-surface-acis.patch` decodes them as far as the ACIS data
(2026-11-06; before, R14 to R2010 surfaces were skipped with `no ACIS
data`). NURBSURFACE is still a debugging class (no sample).

### 4.2 AcDs section (`acis/acds.py`)

- Segments: header of 48 bytes starting `AC D5`, 6-char name
  (`segidx`, `datidx`, `_data_`, `schidx`, `schdat`, `search`, `blob01`,
  `prvsav`, `freesp`), then `RL segment_index` at offset 8.
- `_data_` segments: index records `RL 20, RL 1, RLL handle, RL offset`, then
  `0x62` padding bytes, then the data area. `offset` is relative to the data
  area. Entries can belong to other schemas (the first is often a PNG
  thumbnail).
- Entry at `offset`: either `RL size` + data, or a blob reference:
  `RL 0xBB106BB1, RLL total, RL pages, RL ref_size`, 16 unknown bytes, then
  `(RL segment_index, RL page_size) × pages`; `ref_size` counts the bytes
  after the magic.
- `blob01` segments: after the 48-byte header, `RLL total, RLL offset,
  RL page, RL pages, RLL page_size`, then the page data. Concatenate pages in
  order; the result length must equal `total`.

### 4.3 SAB/SAT tokens (`acis/sab.py`)

- SAB header: `ACIS BinaryFile` + 4 × int32 (version, records, bodies,
  history), or `ASM BinaryFile4` (AutoCAD 2018+) + int64 version + 2 × int32.
  Then strings (product, ACIS version, date) and 3 doubles (units in mm,
  resabs, resnor). The declared unit is not reliable; ignore it.
- SAB tags: 0x04 int32, 0x06 double, 0x07/0x08/0x09/0x12 string with 1/2/4/4
  byte length, 0x0A true, 0x0B false, 0x0C pointer (int32), 0x0D ident
  (ends a type name), 0x0E sub-ident (type-name prefix, joined with "-"),
  0x0F `{`, 0x10 `}`, 0x11 end of record, 0x13/0x14 position/vector (3
  doubles), 0x15 enum (int32), 0x16 int64, 0x17 2D vector. Stream ends at the
  `End-of-ACIS-data` / `End-of-ASM-data` record.
- SAT: whitespace tokens; `$n` pointer, `@n str` counted string, `#` end of
  record; an optional `-n` record number before the type name. Inside `{ }`
  words become `("type", word)` to match SAB.

### 4.4 Record layouts

Every record starts with a common prefix: 3 fields in ASM ≥ 21500
(attrib, history id, one more pointer), 1 field in SAT 4.0. **`transform`
records have only 2 (attrib, history)**: missing that dropped the mm→m
transform of bodies stored in millimeters. Fields after the prefix:

| Record | Fields |
| --- | --- |
| body | lump, wire, transform |
| lump | next, shell, body |
| shell | next, subshell, face, wire, lump |
| face | next, loop, shell, subshell, surface, sense (reversed?), double-sided |
| spline-surface | sense (reversed?), then the subtype block |
| loop | next, coedge, face |
| coedge | next, prev, partner, edge, sense (reversed?), loop, pcurve |
| edge | v0, t0, v1, t1, coedge, curve, sense, convexity (SAT 4.0: no t0/t1) |
| vertex | edge, int, point |
| point | position |
| straight-curve | root, direction (P = root + t·dir) |
| ellipse-curve | center, normal, major axis, ratio (P = c + major·cos t + ratio·(n×major)·sin t) |
| plane-surface | root, normal, u-direction, flags |
| cone-surface | center, axis, major axis, ratio, …, sin(half angle), cos(half angle), scale |
| sphere-surface | center, radius, u-direction, pole |
| torus-surface | center, axis, major radius, minor radius, u-direction |
| spline-surface / intcurve-curve | subtype block `{…}` with bs3 data |
| transform | text: 9 matrix values (row-vector convention) + translation + scale + `rotate/no_rotate …`; `p' = scale·(p·A) + t` |

`tedge-edge`, `tcoedge-coedge` and `tvertex-vertex` (tolerant variants) share the
leading fields. In SAT, logicals are words (`forward`/`reversed`,
`single`/`double`) and vectors are loose numbers (grouped per type, see
`_SAT_LAYOUT`).

Edge parameters: if the edge sense is reversed, the curve parameter is the
negated edge parameter. Always validate against the vertex positions; when
there are no parameters (SAT 4.0), project the vertices onto the curve.

### 4.5 bs3 curves/surfaces (in subtype blocks)

- Curve: `nubs|nurbs, degree, closure, n_knots, (knot, multiplicity)×n, control points (x y z [w])`.
- Surface: `nubs|nurbs, deg_u, deg_v, [rational: both|u|v], closure_u, closure_v, sing_u, sing_v, n_knots_u, n_knots_v, knots_u, knots_v, control points`.
- End multiplicities are stored as `degree`; a clamped knot vector needs
  one more at each end. Control points: **u varies fastest**.
- The closure words (`open`, `closed`, `periodic`; enums 0/1/2 in SAB)
  matter: a closed direction has the period of its domain, and evaluation
  and the inverse wrap there. Without that, faces on extruded ovals and
  revolved splines were triangulated across the seam (a hole in a
  table's fabric and a wedge on its top). Poles of splines are found
  geometrically (a boundary row of control points that is one point).
- `ref n` points to the n-th subtype block in the stream (pre-order, refs
  excluded).
- Procedural surfaces (`rb_blend_spl_sur`, `VBL_SURF`, `sweep_spl_sur`,
  `sum_spl_sur`, …) embed their support geometry's bs3 data too, and may store
  `nullbs` (no approximation). Taking the first bs3 found was wrong for ~60 %
  of the spline faces of Autodesk's condominium example drawing. Now all
  candidates are tried and the one that fits the face boundary is used (`SplineChoice`); if none fits
  within 10× tolerance, use the fallback.
- Pipes (`pipe_spl_sur`) and rolling-ball blends (`rb_blend_spl_sur`) saved
  with `nullbs` only (51 faces of one chair) are evaluated as a
  `Pipe`: a circle swept along a spine. For a blend the spine is the ball
  centre curve (the `off_int_cur`, offset intersection of the supports) and
  the face is the part of the pipe between the contact curves. Every bs3
  curve in the subtype (nested `ref`s resolved) is a candidate spine
  (`PipeChoice`); the radius is the boundary's mean distance from it, and
  the candidate is used if the boundary stays within 10× tolerance of that
  radius. u is arc length along the spine, v the angle times r from a
  rotation-minimising frame; past its ends the spine runs on along the end
  tangents. ACIS's normal convention for these is not in the file, so the
  triangles are oriented by the coedge loops (the face lies to their left).

### 4.6 Tessellation (`acis/tessellate.rs`)

1. Sample every edge once (cached per edge, so shared edges match). Straight
   edges are just their two ends; curves are subdivided adaptively to the
   deviation. The edge's sense picks the curve parameter's sign; the other
   sign only when it fits the vertices clearly better: both fit a half
   circle from angle -pi, but trace opposite halves. A closed spline edge
   whose parameters do not fit starts at its vertex and wraps.
2. Map loop points to the surface's own (u, v): plane = projection; cone =
   (arc length on the base, axial distance); sphere/torus = angles times
   radius; splines their own parameters. A point on a pole (sphere pole, cone apex)
   has no u of its own and becomes a pole segment from its predecessor's u
   to its successor's; a loop through a pole never runs around the axis, the
   pole segment takes the other way round instead.
3. Unwrap periodic parameters. Loops that wrap around the seam are closed:
   two wrapping loops form a band (cut both at the same u, at a vertex
   that is not on a u-constant piece of the loop; on a torus B is moved by
   whole periods in v to the face's side of A); one wrapping loop is closed
   over the pole/apex on the face's side. Loops that wrap in v only (a
   torus elbow) are closed the same way with u and v swapped. The seam cut
   and the meridians to a pole are boundary but no edge: they are sampled to
   the deviation too. Faces without loops cover the whole periodic domain
   (splines: their parameter domain).
4. Outer loop = largest area; holes are bridged into it (a bridge must leave
   both rings into the region and touch no edge, not even at a vertex);
   ear clipping. Ears need a clearly positive area (near-collinear corners
   would be zero-area triangles, dropped later, leaving cracks); a
   remainder without ears is split along an inner diagonal, fanned only as
   the last resort and then only with forward triangles; boundary vertices
   left over on a straight run are inserted into the triangle edge they lie
   on. A zero-area loop (out and back along itself) gives nothing.
5. Lawson edge flips to Delaunay (planes too), then split interior edges
   whose surface midpoint lies further than the tolerance from the edge
   (less the distance the end points lie off the surface: a spline that
   only approximates the face would otherwise refine its border forever),
   flip again; at most 64 rounds and 100,000 triangles per face, and
   refinement stops when the largest deviation has not halved in 8 rounds
   (a smooth surface quarters it every round; a face whose uv triangles do
   not follow the surface, where the parameterisation folds or jumps, kept
   the same deviation until it reached the triangle limit). Each of these
   is reported. An edge from a pole is measured along the meridian
   of its other end; the uv midpoint follows a spiral that never converges.
   On curved surfaces the flips are Delaunay in the surface's own metric:
   the uv points of each quad are mapped by the mean first fundamental form
   (E, F, G from differences) of its corners. In plain uv a stretched
   parameterisation (most spline surfaces; a torus whose tube is wider
   than its ring) gave triangles that were round in uv but slivers on the
   surface, and refining slivers until their long edges met the tolerance
   took many times the triangles, often up to the per-face limit. The
   deviation is measured from the edge, not from its midpoint: where the
   parameter runs unevenly, the uv midpoint lies to one side along the
   surface, an offset that shrinks only with the edge length.
6. The tolerance is per body: `max(0.1 mm / body scale, 0.1 % of the body
   diagonal)`. The body transform scale matters because bodies are often in mm.
7. Orientation: the uv triangulation winds around Su x Sv; it is turned
   where `face.reversed XOR surface reversed` (cone with cos < 0, sphere
   with radius < 0, torus with minor radius < 0, spline-surface marked
   reversed), and everything is turned for a mirroring body transform.
   Coedge loops run counter-clockwise around the outward normal, so the
   boundary-only fallbacks and pipes are oriented by them. Ignoring the
   sense left many faces inside out.
8. Fallbacks (reported as `ACIS face filled from its boundary only`): a 3- or
   4-edge face without a usable surface gets a Coons patch over its edges;
   anything else a triangulation of its boundary. Two faces of the
   condominium drawing (VBL_SURF without NURBS data) still need it. Flat
   fills of curved blends show as fans of creases (a chair seat before
   pipes were evaluated).

Normals come from the surfaces: at every corner the tessellator inverts
the point to (u, v) and takes Su x Sv (turned with the triangles). Smoothing
the facets instead (`mesh.rs`, as for meshes) shaded a chair's coarse
and sliver triangles in visible patches. Where the surface has no normal
(pole, apex) a corner takes its triangle's other corners' normals; faces
filled from their boundary only are smoothed by `mesh.rs` as before.

## 5. glTF output

- One node per INSERT instance, meshes shared per (block, style). Node
  `extras`: block name and handle.
- Accessors: float32 positions with exact float32 min/max; uint16 indices
  when ≤ 65535 vertices, else uint32. Buffers padded to 4 bytes.
- `json.dumps(..., allow_nan=False)`: a non-finite value is a bug upstream
  (it happened with garbage ACIS wire points ≈ 1e300).
- Empty drawings get a scene without `nodes` (an empty array is invalid).
- Equal texture images are stored once (files often give several MATERIALs
  the same texture).
- A block whose mesh is written relative to an offset (section 3,
  precision) gets the offset in its node matrix, or in a child node holding
  the mesh when the node also has block instances below it.

### 5.1 Compression (`--compress`, `compress.rs`)

A pass over the finished document, like gltfpack or glTF-Transform's
`optimize`; it relies on the writer's layout (one packed view per accessor).

- **Quantization** (`KHR_mesh_quantization`): positions as uint16 relative to
  the mesh's bounds, one step for all axes (extent / 65535), so the
  dequantization `T(min) * S(step)` is uniform and normals stay valid. Every
  node showing the mesh multiplies it into its matrix; a node with children
  hands the mesh to a new child instead. Normals are int16 normalized: with
  int8 (gltfpack's default) large smooth upholstery surfaces showed
  blotchy shading in Blender. UVs stay float32: they often leave 0..1
  (glTF-Transform's `quantize` skips them for the same reason).
- **Order**: meshoptimizer's vertex cache optimization for triangles, then
  vertices in first-use order (unused ones dropped). Triangles that
  collapse or turn over on the quantization grid are left out first (up to
  half the triangles of thin polyline bands in a 30 m drawing; slivers
  below 1/65535 of the mesh size elsewhere).
- **Coding** (`EXT_meshopt_compression`): vertex codec version 0
  (ATTRIBUTES, strides 8/8/8 for positions/normals/UVs), index codec
  version 1 (TRIANGLES, or INDICES for lines). The views point into a
  fallback buffer without data; the extensions are required.
- **Textures** (`EXT_texture_webp`): decoded with the `image` crate,
  encoded with libwebp at quality 85 (normal maps losslessly: they are
  data, and lossy coding bends the normals), kept only when smaller.
- Libraries: `meshopt-rs` (a pure Rust port of meshoptimizer 0.15, encoder
  and decoder), `libwebp-sys` (libwebp's C sources, compiled with `cc`), and
  `image` (pure Rust PNG/JPEG/WebP decoders). Draco and KTX2 would need C++
  encoders.
- Compatibility: Blender 4.2 reads quantization and WebP but not meshopt.
  Hence opt-in.
- The glTF reader (`input/gltf.rs`, for `gltf2dwg`) decodes meshopt views
  (with the octahedral, quaternion and exponential filters, strides up to 64
  bytes), reads quantized accessors, and turns WebP textures into PNG for
  the material archive.

Tests: `tests/convert.rs` checks a textured mesh, a block and a circle
(faces, lines, every vertex within one step, UVs, the WebP texture back as
PNG). The Windows build writes the same bytes.
Through `gltf2dwg` and back, slivers narrower than one step can collapse
when the DWG is read again (up to 1 % of the triangles of a dense chair).

## 6. Verification workflow

- `cargo test --release`: unit tests and the tests in
  `crates/dwg2gltf/tests/`, among them the BricsCAD shapes a fix was made
  for (`bricscad.rs`, section 10).
- Khronos validator: `npm install gltf-validator`, `node tools/qa/validate.js out/*.glb`.
- Visual check: `blender -b -P tools/qa/render.py -- in.glb out.png`, tile
  with `tools/qa/sheet.py`. Rendering with backface culling shows faces
  that point away from the viewer as holes.
- Cross-check: LibreDWG's `example_2000.dwg` (SAT text) and
  `example_2018.dwg` (ASM v4 in AcDs) are the same drawing and give the same
  35 triangles.

Triangle counts depend on the tolerance and refinement order; a change of
more than a few percent means a decoding bug.

## 7. Open issues

- Normals don't use the analytic surface normals; very coarse curved faces
  can show facets at the 30 degree crease.
- Source faults that stay visible to a topology check (they render
  correctly): folded MESH triangles lying back over a neighbour that faces
  the same way, and REGION and SURFACE sheets covering the same area. The
  condominium drawing's two VBL_SURF faces without NURBS data are filled
  from their boundary.
- `EDWG_DOUBLE_SIDED_FACES` is read for nothing: every glTF material is
  double-sided, which shows MESHes marked single-sided from both sides.
- Not exported: text, dimensions, hatches, leaders, images, lights,
  viewports, HELIX, MLINE, proxy entities.
- Shells that come out inside out because the source says so (kept, as
  every material is double-sided): a MESH whose stored normals point
  inwards, and a box split over three 3DSOLIDs that are not closed each.
- Speed: the Python version needed up to ~30 s for big ACIS files; the
  Rust version is about 20 times faster (single thread, Apple M-series).

## 8. The Rust port

- LibreDWG is linked in as a static library and `Dwg_Data` is read directly:
  no JSON, no JSON repair, no subprocess. GPL-3.0 stays required.
- Anyone with Rust and a C compiler can build it. `crates/libredwg-sys`
  vendors the decoder and encoder sources of a LibreDWG master snapshot
  (section 11; `vendor.sh` recreates them from the commit's archive and
  checks its SHA-256), and `build.rs` compiles them with
  the `cc` crate after writing `config.h` for the target, so autotools,
  network access and an installed LibreDWG are not needed. The bindings are
  generated once (`gen-bindings.sh`, bindgen) and checked in; their layout
  assertions hold on 64-bit little-endian targets, the only ones supported.
- The AcDs section is no longer dumped to a file: `libredwg-acds-keep-
  section.patch` keeps it in `Dwg_Data.acds.raw`. The MESH fix is upstream
  in the vendored snapshot (section 11).
- The module boundaries are the Python ones (section 1). The converter works
  on plain Rust data (`dwg::Drawing`), so its tests build drawings in memory,
  as the Python tests build JSON.
- The port keeps the Python semantics where they decide results: rounding
  half to even (`round`), the first element on ties (`min`, `max`), and the
  iteration order of the Delaunay flips. Python math errors mark a face as
  failed; in Rust `round` of NaN does the same, and so does a face whose
  triangles come out non-finite (a division by zero).
- Deliberate differences: exact coordinates instead of JSON-rounded ones
  (section 2.1); NURBS data with fewer control points than degree + 1 is
  rejected (Python evaluated it with wrapped negative indices); an ACIS edge,
  arc or ellipse whose start angle is so large that adding 2 pi changes
  nothing stops after 1000 turns (Python looped forever).
- Not yet tested: building with MSVC (`x86_64-pc-windows-msvc`).
  `config.h` has the MSVC settings and LibreDWG supports MSVC upstream.

## 9. DWG output (`gltf2dwg`)

```
glTF/GLB, OBJ, STL ──input::load──► Scene: meshes, materials, nodes
    ──export::write──► dwg_writer::DwgBuilder ──LibreDWG encoder──► DWG
```

| Rust (`crates/dwg2gltf/src/`) | Owns |
| --- | --- |
| `input/` | glTF/GLB (own reader on serde_json), OBJ + MTL, STL into one `Scene` |
| `export.rs` | instancing, transforms (Y-up to Z-up, units), layers, materials, material archives, stats |
| `dwg_writer.rs` | the only writing code that touches LibreDWG |
| `ofml.rs` | EDWG names and chunk sizes shared with the reader, material archives |

Output: r2010, r2013 or r2018 (default). Textures need MESH (r2010+) and
MATERIAL (r2007+); LibreDWG cannot write r2007. Each primitive becomes a MESH
(level-0 cage, faces as given, unwelded vertices, only the vertices the
faces use); a mesh used by several nodes becomes a block with one INSERT per
node when the node matrix splits into OCS extrusion, rotation and scale
(`export::decompose`, checked against the reader's insert matrix), else a
transformed copy. Each material becomes a layer (its color), a MATERIAL
(diffuse color, opacity) and an embedded OFML material archive.

- Primitives above 2^20 faces are split into several MESHes: past 16M list
  values LibreDWG's object encoder returns early and writes an empty MESH
  without the error reaching the caller (`dwg_writer` refuses such sizes).
- Reading: a file that requires an extension outside the supported list is
  refused; `EXT_mesh_gpu_instancing` instances become nodes;
  `KHR_texture_transform` (and its `texCoord`) is applied to the UVs; morph
  targets at the mesh's default weights. Faces without normals get flat
  ones (glTF, STL) or, for OBJ, flat or smoothed per `s` group; a MESH
  without normals would otherwise be smoothed at 53 degrees (section 3).
  Partial `vt` keeps the others (the
  rest get 0, 0, reported). STL is binary unless the text starts with
  `solid` (after a byte order mark or blank lines), has a `facet` and no NUL
  bytes; the binary triangle count is taken from the header.
- OBJ and STL carry no units: `--input-units`, else meters with a warning.
  A primitive without a material is single-sided for glTF (its default
  material is).

### 9.1 What the written records contain

- The material archive is found by MATERIAL name only: NOD → `PXCAD_DICT` →
  `EDWG_MATERIAL_DICT` → XRECORD (310 chunks of any size, joined).
- `id.txt` holds an id that starts with `::`. We write
  `::gltf2dwg::<name>_<content hash>`, so equally named materials of
  different files stay apart.
- `material.mat` starts with `type common`; `dif r g b` (sRGB), `metallic`,
  `roughness`, `tex image png|jpg <stem>`. Transparency is `tra S` (OMATS 2.2:
  alpha = 1 − S); the writer does not emit it.
  `import` keeps the UVs stored on the mesh; `prjz` and the other mappings
  compute new ones.
- Each MESH gets its MATERIAL explicitly, not ByLayer.
- `EDWG_TEX_COORDS_2D`: chunks of whole u/v pairs (we write 112 bytes), one
  pair per vertex, v upwards. `ADSK_XREC_SUBDVERTEXNORMALS` (40/41/42 per
  vertex) applies at subdivision level 0 only; without normals a MESH is
  smoothed at `EDWG_NORMAL_SMOOTHNESS` degrees (default 53).
  `EDWG_DOUBLE_SIDED_FACES` 0 means single-sided; absent means double-sided.
- MESHes in files with these records have `dlevel` 2, `is_watertight` 0,
  level 0, `unknown_b1` 1, `unknown_b2` 0 and one zero crease per edge.

### 9.2 LibreDWG's encoder

`patches/libredwg-add-mesh-material.patch` in `crates/libredwg-sys`:

- Add API: `dwg_add_MESH` (edges derived from the faces), `dwg_add_MATERIAL`
  (creates `ACAD_MATERIAL` with ByLayer/ByBlock/Global and the header
  handles), `dwg_add_xdictionary`, and bulk `dwg_add_XRECORD_chunks` /
  `_reals` (the existing ones walk the whole value list per value).
  `dwg_add_XRECORD` now sets the owner handle and reactor.
- `dwg_add_Document` marks a drawing as a DXF and JSON import. Under the DXF
  flag the encoder replaced every MATERIAL with a placeholder; the patch
  exempts drawings made by the add API (`DWG_OPTS_ADDAPI`). The JSON flag cuts
  XRECORD data at 32 KB and keeps layer flags as given, so the shim clears it
  while encoding. The DXF flag stays: it fills the r2004+ file header.
- The encoder rejected vectors of more than 0xff00 values, point lists of
  more than 65534 and repeats of more than 20000; real files exceed all three
  (one file: 115866 face values, 47596 edges). The bound is now 16M.
- Objects over 0x7fff bytes: the size field grows from one word to two and
  the encoder moves the object 2 bytes. It moved `size + 3` bytes, but the
  placeholder header is 6 bytes (a 0 handle-stream size is written as 4-byte
  UMC), so the last 3 bytes, the handles, were lost: big MESHes lost their
  layer and material. The patch moves the whole object.
- The encoder drops `material_flags` 3 unless built with `DEBUG_CLASSES`.
  With 0.14 the writer put the material into LibreDWG's plotstyle slot,
  which was the file's material slot (the flag swap, section 2). Upstream
  fixed the swap, so `patches/libredwg-entity-material.patch` keeps
  `material_flags` 3 for drawings made by the add API and the writer sets
  `material`. The entity bits in the file are the same as before;
  `tests/to_dwg.rs` checks that the MESHes read back with flags 3 and the
  MATERIAL as their first handle, as in files that carry EDWG records.
- Moving from 0.14 to master left MESH/MATERIAL output byte for byte the
  same (apart from the header timestamps). LINEs differ: 0.14's
  `bit_write_DD` compared the wrong words and always wrote the end point as
  a full double; master picks the shorter default-relative forms, which
  decode to the same values.
- Header values (shim `dwg2gltf_new`): `zero_one_or_three` 3 and the
  writer's `dwg_version`/`maint_version` as AutoCAD 2010 (29/37), ODA for
  r2013 (33/55) and AutoCAD 2018 (33/4). With `maint_version` 0 an r2013
  file had no `hsize` in its classes section while readers expect one.
- r2004+ sections are written uncompressed in zero-padded pages, plus an
  empty preview page: every file is at least about 300 KB.
- `patches/libredwg-r2010-section-sizes.patch`: in R2010+ the header
  variables and classes sections are `sentinel, RL size, RL high size word,
  data, CRC-16 (seed 0xC0C1 over size, high word and data), end sentinel`,
  and AutoCAD/ODA count only the data in `size` and write 0 as the high
  word. LibreDWG 0.14 counted the high word too (and wrote bitsize/8 into it
  for the classes), so ODA read the CRC 4 bytes too late and refused every
  file ("CRC does not match"). LibreDWG's reader does not
  check these CRCs, so the files read back fine here.

### 9.3 Verification

- `tests/to_dwg.rs`: a textured glTF with a block, two INSERTs and a sheared
  copy round-trips for r2010, r2013 and r2018 (world bounds, UVs, texture
  bytes, MATERIAL and PBR values); OBJ + MTL and STL inputs.
- Building the encoder into the library left reading unchanged: the GLBs
  were byte-identical before and after.
- `tests/to_dwg.rs` checks the header and classes sections byte by byte the
  way ODA reads them (size, high word, CRC, end sentinel) for every version.
  ODA File Converter 26 (Linux) opens the written files; before the section
  size patch it refused all of them.
- Validation in AutoCAD/TrueView and BricsCAD is manual.
- Review cases (section 12): mirrored instances and copies, shear,
  non-uniform scale, strips, fans, sparse and strided accessors, UV set 1,
  instancing, morph targets, missing normals, far from the origin. Each
  triangle comes back with its winding, and normals within 1e-4.

## 10. Test drawings

`tests/bricscad`: DWG files made by BricsCAD from scripts, one known shape
in each drawing, in every DWG version from R14 to 2018, each with a right
answer in `expected.json` (from a formula where a shape has one, otherwise
from BricsCAD after reopening, never from the converter).
`tests/bricscad/measure.py` converts each file and compares bounds, area,
volume and facing with the right answer; `crates/dwg2gltf/tests/bricscad.rs`
checks the files a fix was made for. See `tests/bricscad/README.md`.

`tools/qa` compares the converter's GLBs with reference GLBs of the same
drawings, of any origin, over a folder of DWGs.

## 11. Upstream (LibreDWG)

The Rust build vendors master
[`4f2aef33`](https://github.com/LibreDWG/libredwg/commit/4f2aef33865e0d5bf3774acc439953fba15dea3e)
(2026-11-05) instead of the 0.14 release (2026-06-27): there is no 0.14.1
tarball or tag yet, and master has the security fixes that `NEWS` lists
under "0.14.1" (heap overflows in the R2004/R2007 section readers, the
3DSOLID double free and more; `crates/libredwg-sys/libredwg/NEWS`). The
removed Python build stayed on the 0.14 tarball with its own patches, as it
needed a configure script, which the master archive does not have.

- MESH fix: PR https://github.com/LibreDWG/libredwg/pull/1435, merged
  2026-10-12. In the snapshot; the Rust build no longer applies the patch.
- Flag swap: issue https://github.com/LibreDWG/libredwg/issues/1436, fixed by
  76cf0d750 (2026-10-12, issue closed 2026-10-29). In the snapshot: the
  reader takes `material_flags` as decoded, and the writer sets `material`
  (section 9.2).
- Open issue #1411 (AcDs section not read): section 4.2 above is the
  layout that issue needs; `libredwg-acds-keep-section.patch` stays.
- Candidates from the DWG writer (section 9.2), not yet offered upstream: the
  overlarge-object fix (master changed only the shrinking case), the R2010+
  header/classes section sizes, the MESH/MATERIAL/xdictionary add functions,
  the XRECORD owner, the bulk XRECORD appends, the encoder bounds and keeping
  explicit entity materials (`libredwg-entity-material.patch`).
- Found while moving to master, not yet reported upstream: the new embedded
  MTEXT decoding of R2010+ ATTRIB/ATTDEF reads `num_reactors` handles
  without checking the end of the stream, so a corrupt count hangs the
  decoder (`libredwg-embedded-mtext-reactors.patch` stops the loop).
- Not reported upstream: the four procedural surface classes are debugging
  classes, so R14 to R2010 surfaces lose their ACIS data without
  `DEBUG_CLASSES` (`libredwg-surface-acis.patch` decodes only that part).
- Upstream changed the add API's import flag from DXF + JSON to DXF only
  (GH #1499); the shim clears the JSON flag before encoding either way.

## 12. Geometry review (2026-10-11)

What a full review of the conversion's geometry found, and how. Tools (not
in the repository): a mesh checker over GLBs (welds by float32 position;
counts triangles whose normals oppose their winding, edges that two
neighbours run the same way, closed shells with negative volume), a
Blender ray test (a triangle whose front sees nothing of the outside while
its back does faces inwards; a closed room's floor is a false positive), a
per-face ACIS loop check (each coedge must run along its face's triangles
in their winding), and the Khronos validator.

- Missing faces: 80 `SURFACE` entities of a wall panel were
  skipped (section 3); ear clipping fell back to fans over self-crossing
  loops and dropped holes it could not bridge; boundary vertices left over
  on straight runs cracked shells open.
- Inside out: ACIS face and surface sense were ignored (10 to 39 % of the
  surface area of the ACIS files faced inwards); sphere caps always closed
  over the north pole; torus bands could take the other part of the tube;
  noise could mirror a cone's apex.
- Garbage: periodic NURBS faces triangulated across their seam, semicircle
  edges sampled along the wrong half.
- MESH: stored normals against the winding (18 triangles), slivers turning
  over in float32, the `EDWG_NORMAL_SMOOTHNESS` angle ignored, bow-tie and non-planar
  polygons with backward triangles, source winding faults.
- gltf2dwg: batch outputs overwriting each other, instancing and large
  meshes dropped silently, STL misdetection, smoothed instead of flat
  normals, unitless OBJ/STL.

## 13. WebAssembly build (three.js loader, 2026-10-13)

`crates/dwg2gltf-wasm` builds the converter for `wasm32-wasip1`, and
`packages/three-dwg-loader` runs it in the browser (`DWGLoader`, a module
worker per conversion slot).

- **Toolchain**: Rust's `wasm32-wasip1` target, with wasi-sdk 34 (clang 23)
  compiling LibreDWG. The vendored `cc` crate finds
  `$WASI_SDK_PATH/bin/wasm32-wasip1-clang` and the sysroot by itself.
  wasi-libc has everything the decoder uses. iconv and `USE_TRACING` are
  off, and LibreDWG uses no setjmp, threads, signals or mmap.
- **Rejected**:
  - Emscripten: a large SDK, JS glue, and a Rust target that changes often.
  - `wasm32-unknown-unknown`: no libc.
- **The module** imports only `wasi_snapshot_preview1` (12 functions,
  `src/wasi.js` implements them) and exports `memory` and the `dgl_*` C ABI.
  It is 5.9 MB after `wasm-opt -O3` (`-Oz` is smaller raw but larger with
  brotli: 940 KB against 913 KB).
- **Linking**: `-zstack-size=8388608 --stack-first`, because ACIS recursion
  needs more than the 1 MiB default, and a stack overflow should trap
  rather than overwrite statics.
- **Bindings**: wasm32 has 32-bit pointers and `size_t`, so
  `src/bindings_wasm32.rs` is generated separately. `gen-bindings.sh wasm32`
  needs:
  - `-fvisibility=default`. Without it, clang's wasm targets default to
    hidden visibility, and bindgen silently drops every hidden function,
    which here means all of the shim's.
  - With wasi-sdk's libclang, `--target=<host>` and `--sysroot=/` for the
    host file. Its default target is wasm32 and it has no glibc headers.
    Host regeneration with that libclang reproduces `bindings.rs` byte for
    byte. `config.h` takes `SIZEOF_SIZE_T` from the target.
- **One instance per file**: LibreDWG keeps state in statics, wasm memory
  never shrinks, and a trap leaves an instance unusable, so `core.js`
  instantiates the compiled module afresh for every conversion. That takes a
  few milliseconds. Panics abort; `core.js` turns traps into `DWGError`s
  (`panic`, `out-of-memory`, `crash`) using the captured stderr.
- **Results against native** (Node tests, `packages/three-dwg-loader/test`):
  - The summaries are identical.
  - The GLBs are byte-identical, except that an ACIS file can have float
    components one float32 ulp apart (at most 6e-8 m in the files tested)
    with identical JSON and indices: glibc and wasi-libc (musl) differ in
    the last bit of some `sin`, `cos` and `atan2` results.
  - Output is deterministic although `HashMap` seeds come from `random_get`.
- **Speed**: Node takes 1.3 to 1.5 times the native time, including
  instantiation. Memory is 10 to 75 MB per file on the files tested.

## 14. Drawings in meters that declare millimeters (2026-10-28)

OFML article geometry is often stored in meters in DWGs that declare
millimeters (INSUNITS 4), so INSUNITS makes it 1000x too small. Other
parts are genuine millimeters, also next to meter parts, and meters appear
under INSUNITS 1 and 5 too. No header variable (INSUNITS, MEASUREMENT,
LUNITS, extents, limits, text sizes, about 60 in all), writer (AppInfo: ODA
Teigha and AutoCAD in both groups), class list, dictionary, layer name or
ACIS header unit separates the two groups, and their sizes overlap.

So the shown units only change on request.

- Warning (`convert::tiny_drawing`): INSUNITS 1, 2, 4, 5, 10 or 14, no
  `units` given, and the scene less than 1 cm across (largest side of the
  world bounding box from the POSITION accessors' min/max through the node
  matrices, `GltfWriter::bounds`, before compression). Series DWGs: the
  largest of all series blocks, also when only some are converted (the
  loader's `only`); blocks are measured until one is 1 cm or more across.
- `--units auto` (`Options::auto_units`; wasm and three.js `units: 'auto'`):
  converts such a drawing again in meters (the curve deviation depends on
  the unit), so its output equals `--units m`; every other file's equals the
  default.

## 15. C interface and visible edges (2026-11-06)

`crates/dwg2gltf-capi` is the converter as a shared library for native
viewers (`include/dwg2gltf.h`, `bindings/delphi/Dwg2Gltf.pas`).

- **No second conversion path**: a scene is `convert_document` as for a
  GLB file (never compressed); the crate then copies the `GltfWriter`'s
  document and binary buffer into flat arrays. So the scene and the GLB
  (`dwg_scene_to_glb`, made from a clone of the writer on first use)
  cannot disagree, and the GLB is byte for byte the command's.
  `GltfWriter` only gained `Clone` and `binary()`.
- **Meshes**: the primitives of one glTF mesh go into one vertex buffer,
  one submesh each, indices widened to uint32 and made absolute. Line
  primitives have no normals or UVs: those are zero. UVs are present
  when any triangle submesh is textured (zero for the others). A base
  color texture's transform is already in the UVs (section 3); only a
  normal map can have its own `KHR_texture_transform`, which the material
  carries.
- **Nodes** are the glTF nodes in depth-first order from the root, so a
  parent comes before its children. World matrices are computed in double
  from the column-major glTF matrices; instances carry them as float.
  The unit scale is the length of the root matrix's first column (the
  units `--units auto` chose included).
- **Options**: `DwgOptions` begins with its `size`; fields past the
  caller's size take their defaults, so the struct can grow. All flags
  clear and all fields zero is the default (lines on, edges on, an empty
  drawing is an empty scene, as in the WebAssembly module). Units 0 is
  "the drawing's"; overriding with INSUNITS 0 would mean meters, which
  code 6 says as well. The curve deviation (0.1 mm) is a constant, not an
  option.
- **Errors and panics**: every function runs inside `catch_unwind` and
  reports a status code, with the message in a thread-local string.
  Conversion runs on a thread with a 256 MiB stack (reserved address
  space): a host thread may have only 1 MiB, and recursion over nested
  blocks and ACIS subtypes needs more (section 3.1).
- **Layout**: every struct field sits at a multiple of its size, so C,
  Rust `repr(C)` and Delphi (`{$ALIGN 8}`) agree without packing. A test
  prints `sizeof`/`offsetof` of every field from the header with the C
  compiler and compares them, and the `#define`s, with the Rust structs;
  another does the same for the Delphi unit with Free Pascal (Delphi
  mode), when installed, and runs `examples/delphi/SceneInfo.dpr` against
  the Linux library. `scripts/test-delphi.sh` repeats that with
  the Delphi compiler under Wine against the Windows DLL.
- **Visible edges** (`edges.rs`), per mesh over all its triangle
  submeshes:
  - Weld positions within 1e-5 of the mesh's bounding box diagonal (grid
    of that cell size, neighbouring cells searched), so vertices split for
    normals, UVs or materials are one. Edge indices point at the first
    vertex at a position.
  - Drop triangles that collapse, have collinear corners, or repeat
    another (also with the opposite winding, as a back face would).
  - An edge with one triangle is a boundary, with three or more
    non-manifold; with two it is a crease when the face normals differ by
    more than the crease angle (30 degrees by default) and the vertex
    normals of the two triangles differ along the edge (by more than about
    2.6 degrees at either end). The face normals are compared as if the two
    triangles were wound consistently (a pair running along the edge in
    the same direction has one normal negated), because triangles of
    different materials are oriented separately and a flat pair wound
    oppositely must not count as a 180 degree fold.
  - The vertex normals are the converter's own decision between smooth and
    hard (ACIS surface normals, MESH normals or smoothing, 30 degree
    smoothing elsewhere): a curved surface tessellated more coarsely than
    the crease angle but shaded smooth shows no facet edges (a helix sweep
    of a small circle had 174 edges by face angle alone, 32 with the
    normals: its two end rims).
  - A coplanar split therefore gives no edge, a smooth seam (cylinder)
    none. BricsCAD solids at 30 degrees: box 12, wedge 9, cylinder 128
    (two rims of 64 segments), cone 64 (its base rim), sphere and torus 0;
    every one of them welds closed (no boundary edges). Two cylindrical
    fillets meeting at a mitre show that seam (their faces fold by more
    than 30 degrees there; at 60 degrees it goes).
  - The meshes are shared out over the available cores.
