// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! DWG -> glTF scene conversion.
//!
//! Blocks become shared meshes instanced by INSERT nodes, entities on layer
//! "0" and ByBlock colors/materials inherit from the referencing INSERT,
//! drawing units are scaled to meters, tessellation deviation is 0.1 mm,
//! faces are double sided, and the OFML extension data (EDWG_TEX_COORDS_2D,
//! embedded material archives) is honoured.

use std::cell::Cell;
use std::collections::{BTreeMap, HashMap};
use std::f64::consts::TAU;
use std::fmt;
use std::rc::Rc;

use serde_json::{Value, json};

use crate::acis::brep::Brep;
use crate::acis::sab;
use crate::acis::tessellate::Tessellator;
use crate::dwg::{Data, Entity, Handle, Object, Shape, XValue};
use crate::geom::{
    Mat, Vec2, Vec3, ZERO, add, apply, arc_end, arc_points, arc_segments, bspline_points,
    bulge_points, catmull_rom, chain, cross, dot, finite, is_identity, length, mat_mul, mul,
    normalize, ocs_axes, ocs_matrix, rotate_z, scale, sub, translate,
};
use crate::gltf::{GltfWriter, LINES, MaterialDesc, NormalTexture, TRIANGLES, TextureTransform};
use crate::limits::{Limits, MAX_NESTING, MAX_SPLINE_DEGREE};
use crate::mapper::{self, MaterialMapper};
use crate::mesh::{
    LineBuilder, Projection, Repairs, TriangleBuilder, UvFrame, affine_inverse, affine_mul, bounds,
    gltf_uv_affine,
};
use crate::model::{ACI, ColorRef, Document, Rgb, binary_chunks, parse_color, rgb_unit};
use crate::ofml;

/// Curve and surface tessellation deviation: 0.1 mm.
const DEVIATION_M: f64 = 1e-4;
/// Vertices closer than this are one.
const WELD_M: f64 = 1e-6;
/// Smoothing angle for a MESH without vertex normals and without its own
/// EDWG_NORMAL_SMOOTHNESS (the OFML default).
const OFML_MESH_SMOOTHING: f64 = 53.0;
/// MESH extension record: AutoCAD's per-vertex texture coordinates
/// (reals 43/44/45 = u/v/w), see [`Converter::adsk_tex_coords`].
const ADSK_TEX_COORDS: &str = "ADSK_XREC_SUBDVERTEXTEXCOORDS";
/// Name prefix of the blocks OFML exports draw an article's 2D plan symbol
/// in (GFJ = OFML "Grafik"). They belong in plan views only, not in a 3D
/// model.
const PLAN_BLOCK: &str = "PEC_GFJ_GEO";

/// Layers for 2D plan symbols: OFML layer names "72_<program>_D2_..."
/// and "72_<program>_D2DETAIL_..." (the 3D ones carry no D2).
fn is_plan_layer(name: &str) -> bool {
    let name = name.to_ascii_uppercase();
    name.starts_with("72_") && name.contains("_D2")
}

const ACIS_ENTITIES: &[&str] = &[
    "3DSOLID",
    "BODY",
    "REGION",
    "SURFACE",
    "PLANESURFACE",
    "EXTRUDEDSURFACE",
    "LOFTEDSURFACE",
    "REVOLVEDSURFACE",
    "SWEPTSURFACE",
    "NURBSURFACE",
];
const ANNOTATION: &[&str] = &[
    "TEXT",
    "MTEXT",
    "ATTRIB",
    "ATTDEF",
    "DIMENSION_LINEAR",
    "DIMENSION_ALIGNED",
    "DIMENSION_ANG2LN",
    "DIMENSION_ANG3PT",
    "DIMENSION_RADIUS",
    "DIMENSION_DIAMETER",
    "DIMENSION_ORDINATE",
    "ARC_DIMENSION",
    "LEADER",
    "MULTILEADER",
    "TOLERANCE",
    "HATCH",
    "MPOLYGON",
    "POINT",
    "VIEWPORT",
    "LIGHT",
    "XLINE",
    "RAY",
    "WIPEOUT",
    "IMAGE",
    "OLE2FRAME",
    "TABLE",
    "ACAD_TABLE",
    "CAMERA",
    "SUN",
];
const STRUCTURAL: &[&str] = &[
    "SEQEND",
    "ENDBLK",
    "BLOCK",
    "VERTEX_2D",
    "VERTEX_3D",
    "VERTEX_MESH",
    "VERTEX_PFACE",
    "VERTEX_PFACE_FACE",
];

/// Properties an INSERT hands down to ByBlock / layer-0 content.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct Style {
    color: Rgb,
    layer: Option<Handle>,
    material: Option<Handle>,
    /// Scale (f64 bits) for texture projection: the OFML geometry scale
    /// from the nearest INSERT that states one, times the scales of the
    /// INSERTs below it (only in drawings with textures). Its size only: a
    /// mirrored instance keeps sharing the mesh (and shows the texture
    /// mirrored).
    geoscale: [u64; 3],
    /// The material mapper that goes with `material` (that of the INSERT
    /// the ByBlock material comes from, see [`Converter::face_frame`]);
    /// only in drawings with textures.
    mapper: Option<InheritedMapper>,
    /// Inside an OFML 2D plan symbol block (see [`PLAN_BLOCK`]).
    plan: bool,
}

/// A material mapper as [`Style`] hands it down: its projection and the map
/// into the mapper's unit frame (the top three rows of the 4x4 matrix, f64
/// bits). Applied to the coordinates of whichever block the faces are in,
/// as OFML does (see [`Converter::face_frame`]).
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
struct InheritedMapper {
    projection: i16,
    frame: [u64; 12],
}

impl InheritedMapper {
    /// None when its axes are degenerate.
    fn of(m: &MaterialMapper) -> Option<Self> {
        let frame = m.frame()?;
        Some(InheritedMapper {
            projection: m.projection,
            frame: std::array::from_fn(|k| frame[k].to_bits()),
        })
    }

    fn frame(&self) -> Mat {
        let mut m = [0.0; 16];
        for (k, bits) in self.frame.iter().enumerate() {
            m[k] = f64::from_bits(*bits);
        }
        m[15] = 1.0;
        m
    }
}

impl Default for Style {
    fn default() -> Self {
        Style {
            color: ACI[7],
            layer: None,
            material: None,
            geoscale: [1f64.to_bits(); 3],
            mapper: None,
            plan: false,
        }
    }
}

/// A drawing less than this many meters across, in units smaller than a
/// meter, is probably drawn in meters (see [`tiny_drawing`]).
const TINY_M: f64 = 0.01;

/// The units drawings are made in that are smaller than a meter, by
/// INSUNITS, as named in messages.
fn small_unit(insunits: i32) -> Option<&'static str> {
    match insunits {
        1 => Some("inches"),
        2 => Some("feet"),
        4 => Some("millimeters"),
        5 => Some("centimeters"),
        10 => Some("yards"),
        14 => Some("decimeters"),
        _ => None,
    }
}

/// A drawing whose INSUNITS is one of the [`small_unit`]s, converted
/// without `units`, that comes out less than 1 cm across (`size`: the
/// largest side of its bounding box, in meters): the warning for it, and
/// whether `auto_units` converts it again in meters. OFML geometry is often
/// stored in meters in files that say millimeters.
pub(crate) fn tiny_drawing(
    doc: &Document,
    opt: &Options,
    size: Option<f64>,
) -> Option<(String, bool)> {
    if opt.units.is_some() {
        return None;
    }
    let insunits = doc.insunits();
    let name = small_unit(insunits)?;
    if !size.is_some_and(|s| s < TINY_M) {
        return None;
    }
    Some(if opt.auto_units {
        (
            format!(
                "the drawing is less than 1 cm across in {name} (INSUNITS {insunits}); \
                 its coordinates are taken as meters (--units auto)"
            ),
            true,
        )
    } else {
        (
            format!(
                "the drawing is less than 1 cm across in {name} (INSUNITS {insunits}); \
                 if its coordinates are meters (as in many OFML drawings), use --units m \
                 or --units auto"
            ),
            false,
        )
    })
}

/// The largest side of a scene's bounding box (see [`GltfWriter::bounds`]).
pub(crate) fn scene_size(gltf: &GltfWriter) -> Option<f64> {
    let (lo, hi) = gltf.bounds()?;
    Some((0..3).map(|k| hi[k] - lo[k]).fold(0.0, f64::max))
}

#[derive(Clone, Debug)]
pub struct Options {
    /// Export wire geometry (lines, arcs, polylines).
    pub lines: bool,
    /// Export the wire geometry of OFML 2D plan symbols (see
    /// [`PLAN_BLOCK`] and [`is_plan_layer`]), which is left out otherwise.
    pub plan_symbols: bool,
    /// INSUNITS override.
    pub units: Option<i32>,
    /// Without `units`: take the coordinates as meters when the drawing
    /// comes out less than 1 cm across in its units (see [`tiny_drawing`]).
    pub auto_units: bool,
    /// Convert DWG's Z-up to glTF's Y-up.
    pub y_up: bool,
    /// Quantize and meshopt-compress the geometry, WebP textures (see
    /// [`crate::gltf::GltfWriter::compress`]).
    pub compress: bool,
    /// Write a file even when nothing could be converted (else that is
    /// [`crate::Error::Empty`]).
    pub allow_empty: bool,
    /// Per-drawing resource limits (smaller ones for tests).
    pub limits: Limits,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            lines: true,
            plan_symbols: false,
            units: None,
            auto_units: false,
            y_up: true,
            compress: false,
            allow_empty: false,
            limits: Limits::default(),
        }
    }
}

#[derive(Default, Debug)]
pub struct Stats {
    pub converted: BTreeMap<String, usize>,
    pub skipped: BTreeMap<String, usize>,
    /// Geometry that needed a fallback or a repair (still converted).
    pub repaired: BTreeMap<String, usize>,
    pub warnings: Vec<String>,
    pub instances: usize,
    pub triangles: usize,
    /// Line segments.
    pub lines: usize,
}

impl Stats {
    /// Add `other`'s counts; warnings are kept once.
    pub fn merge(&mut self, other: Stats) {
        for (mine, theirs) in [
            (&mut self.converted, other.converted),
            (&mut self.skipped, other.skipped),
            (&mut self.repaired, other.repaired),
        ] {
            for (k, v) in theirs {
                *mine.entry(k).or_default() += v;
            }
        }
        for w in other.warnings {
            if !self.warnings.contains(&w) {
                self.warnings.push(w);
            }
        }
        self.instances += other.instances;
        self.triangles += other.triangles;
        self.lines += other.lines;
    }

    /// Nothing to show: no triangles and no lines.
    pub fn is_empty(&self) -> bool {
        self.triangles == 0 && self.lines == 0
    }

    fn warn(&mut self, warning: String) {
        if !self.warnings.contains(&warning) {
            self.warnings.push(warning);
        }
    }

    fn skip(&mut self, what: impl Into<String>, n: usize) {
        *self.skipped.entry(what.into()).or_default() += n;
    }

    fn repair(&mut self, what: &str, n: usize) {
        if n > 0 {
            *self.repaired.entry(what.into()).or_default() += n;
        }
    }

    fn mesh_repairs(&mut self, r: Repairs) {
        self.repair("triangle turned to its stored normals", r.turned_to_normals);
        self.repair("triangle turned to match its neighbours", r.reoriented);
        self.repair("degenerate triangle dropped", r.degenerate);
    }
}

impl fmt::Display for Stats {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        let list = |m: &BTreeMap<String, usize>| {
            m.iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let conv = list(&self.converted);
        write!(
            f,
            "converted: {}\ntriangles: {}, line segments: {}, block instances: {}",
            if conv.is_empty() { "-" } else { &conv },
            self.triangles,
            self.lines,
            self.instances
        )?;
        if !self.repaired.is_empty() {
            write!(f, "\nrepaired: {}", list(&self.repaired))?;
        }
        if !self.skipped.is_empty() {
            write!(f, "\nskipped: {}", list(&self.skipped))?;
        }
        for w in &self.warnings {
            write!(f, "\nwarning: {w}")?;
        }
        Ok(())
    }
}

/// A block instance: node name, transform, block object index, inherited style, extras.
struct Child {
    name: String,
    matrix: Mat,
    block: usize,
    style: Style,
    extras: Value,
}

/// Material handle, and the color unless the material has its own.
type MaterialKey = (Option<Handle>, Option<Rgb>);

/// Tessellated content of one block for one inherited Style.
#[derive(Default)]
struct Geometry {
    tris: Vec<(MaterialKey, TriangleBuilder)>,
    lines: Vec<(Rgb, LineBuilder)>,
    children: Vec<Child>,
}

/// A converted block: its mesh and its block instances.
struct Built {
    mesh: Option<usize>,
    /// Where the mesh's positions are relative to (block coordinates), so
    /// float32 keeps its precision far from the block origin.
    origin: Vec3,
    children: Vec<Child>,
}

pub struct Converter<'a> {
    doc: &'a Document,
    opt: Options,
    unit: f64,
    /// Curve deviation and weld distance in the units of the block being
    /// converted: the world values divided by the largest scale the block
    /// is shown at.
    tol: f64,
    weld: f64,
    block_scale: HashMap<usize, f64>,
    /// Some MATERIAL of the drawing has a texture.
    textured: bool,
    pub stats: Stats,
    gltf: GltfWriter,
    geom_cache: HashMap<(Handle, Style), Rc<Built>>,
    /// Built meshes by (block, style), the projection scale left out when
    /// nothing in the mesh is projected.
    mesh_cache: HashMap<(Handle, Style), (Option<usize>, Vec3)>,
    materials: HashMap<MaterialKey, usize>,
    line_materials: HashMap<Rgb, usize>,
    textures: HashMap<Handle, usize>,
    /// The blocks being emitted, outermost first (to catch a block that
    /// inserts itself).
    ancestors: Vec<usize>,
    /// Against the limits (see [`crate::limits`]): instances emitted, block
    /// instances (MINSERT cells) created, triangles and line points.
    instances_seen: usize,
    children_made: usize,
    tris_used: Cell<usize>,
    line_points: usize,
    /// What a limit left out: instances, triangles (polygons), line pieces.
    instance_limit_hits: usize,
    tri_limit_hits: Cell<usize>,
    line_limit_hits: usize,
    /// Wire geometry left out as part of an OFML 2D plan symbol.
    plan_hits: usize,
    /// The projection frame of the faces of the entity being converted
    /// (from a material mapper; see [`Converter::face_frame`]).
    face_frame: Option<UvFrame>,
}

/// What became of an entity.
enum Outcome {
    Converted,
    /// Nothing to draw: "(no geometry)".
    NoGeometry,
    /// Already recorded as skipped, with its reason.
    Skipped,
}

/// Why an ACIS entity gave no mesh.
enum AcisLoss {
    NoData,
    Unreadable,
    NoTriangles,
    TriangleLimit,
}

impl<'a> Converter<'a> {
    pub fn new(doc: &'a Document, options: Options) -> Self {
        let unit = doc.unit_scale(options.units);
        Converter {
            doc,
            opt: options,
            unit,
            tol: DEVIATION_M / unit,
            weld: WELD_M / unit,
            block_scale: HashMap::new(),
            textured: doc.objects.iter().any(|o| {
                matches!(o.data, Data::Material(_))
                    && doc.material(Some(o.handle)).is_some_and(|m| m.has_maps())
            }),
            stats: Stats::default(),
            gltf: GltfWriter::new(&format!("dwg2gltf {}", env!("CARGO_PKG_VERSION"))),
            geom_cache: HashMap::new(),
            mesh_cache: HashMap::new(),
            materials: HashMap::new(),
            line_materials: HashMap::new(),
            textures: HashMap::new(),
            ancestors: vec![],
            instances_seen: 0,
            children_made: 0,
            tris_used: Cell::new(0),
            line_points: 0,
            instance_limit_hits: 0,
            tri_limit_hits: Cell::new(0),
            line_limit_hits: 0,
            plan_hits: 0,
            face_frame: None,
        }
    }

    // == scene ==============================================================
    /// Convert model space.
    pub fn run(self) -> Result<(GltfWriter, Stats), crate::Error> {
        let doc = self.doc;
        let ms = doc.model_space.ok_or(crate::Error::NoModelSpace)?;
        let opt = self.opt.clone();
        let (mut gltf, mut stats, empty) = self.run_root("ModelSpace", ms);
        if let Some((warning, meters)) = tiny_drawing(doc, &opt, scene_size(&gltf)) {
            if meters {
                let opt = Options {
                    units: Some(6),
                    ..opt
                };
                (gltf, stats, _) = Converter::new(doc, opt).run_root("ModelSpace", ms);
            }
            stats.warnings.push(warning);
        }
        if empty {
            let series = doc.series_blocks().len();
            if series > 0 {
                stats.warnings.push(format!(
                    "model space is empty, but {series} blocks are not inserted anywhere \
                     (a series DWG); use --seriesdwg for one file per block"
                ));
            }
        }
        let paper = paper_space_geometry(doc);
        if paper > 0 {
            stats.warnings.push(if empty {
                format!(
                    "model space is empty, but paper space has {paper} entities, \
                     which are not converted"
                )
            } else {
                format!("paper space has {paper} entities that are not converted")
            });
        }
        Ok((gltf, stats))
    }

    /// Convert one block as the root of its own scene, its base point at
    /// the origin; no scene when it has nothing to show.
    pub fn run_block(self, block: usize) -> (Option<GltfWriter>, Stats) {
        let name = match &self.doc.objects[block].data {
            Data::BlockHeader(b) => b.name.clone(),
            _ => "block".into(),
        };
        let (gltf, stats, empty) = self.run_root(&name, block);
        ((!empty).then_some(gltf), stats)
    }

    /// The scene below `root`, and whether it came out empty.
    fn run_root(mut self, name: &str, root: usize) -> (GltfWriter, Stats, bool) {
        self.block_scale = block_scales(self.doc, root);
        if self.opt.units.is_none() && self.doc.insunits() == 0 {
            self.stats.warnings.push(
                "the drawing has no units (INSUNITS 0) and is taken as meters; use --units to set them"
                    .into(),
            );
        }
        let s = self.unit;
        let root_m = if self.opt.y_up {
            // DWG is Z-up; glTF is Y-up
            [
                s, 0.0, 0.0, 0.0, 0.0, 0.0, s, 0.0, 0.0, -s, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0,
            ]
        } else {
            scale([s, s, s])
        };
        let root_m = match &self.doc.objects[root].data {
            // a block's base point is where it is inserted: the origin
            Data::BlockHeader(b) if root != self.doc.model_space.unwrap_or(usize::MAX) => {
                mat_mul(&root_m, &translate(sub(ZERO, b.base_pt)))
            }
            _ => root_m,
        };
        let node = self.emit(name, &root_m, root, Style::default(), &Value::Null, 0);
        if let Some(node) = node {
            self.gltf.add_root(node);
        }
        self.limit_warnings();
        if self.doc.decode_error != 0 {
            self.stats.warn(format!(
                "LibreDWG reported recoverable decode errors (0x{:x}): output may be incomplete",
                self.doc.decode_error
            ));
        }
        (self.gltf, self.stats, node.is_none())
    }

    fn limit_warnings(&mut self) {
        for (hits, what, limit) in [
            (
                self.instance_limit_hits,
                "block instances",
                self.opt.limits.instances,
            ),
            (
                self.tri_limit_hits.get(),
                "triangles",
                self.opt.limits.triangles,
            ),
            (
                self.line_limit_hits,
                "line points",
                self.opt.limits.line_points,
            ),
        ] {
            if hits > 0 {
                self.stats.warn(format!(
                    "the drawing needs more than {limit} {what} (the limit); \
                     the rest is left out"
                ));
            }
        }
    }

    fn emit(
        &mut self,
        name: &str,
        matrix: &Mat,
        block: usize,
        style: Style,
        extras: &Value,
        depth: usize,
    ) -> Option<usize> {
        if depth > MAX_NESTING {
            self.stats.skip("INSERT (nesting too deep)", 1);
            return None;
        }
        if depth > 0 {
            // AutoCAD refuses such blocks; each level would double the
            // instances. Normally caught in `insert` already.
            if self.ancestors.contains(&block) {
                self.stats.skip("INSERT (block inserts itself)", 1);
                return None;
            }
            if self.instances_seen >= self.opt.limits.instances {
                self.instance_limit_hits += 1;
                self.stats.skip("INSERT (instance limit reached)", 1);
                return None;
            }
            self.instances_seen += 1;
        }
        // pushed first: building the geometry drops INSERTs of the blocks here
        self.ancestors.push(block);
        let built = self.geometry(block, style);
        let children: Vec<usize> = built
            .children
            .iter()
            .filter_map(|c| self.emit(&c.name, &c.matrix, c.block, c.style, &c.extras, depth + 1))
            .collect();
        self.ancestors.pop();
        if built.mesh.is_none() && children.is_empty() {
            return None;
        }
        if depth > 0 {
            self.stats.instances += 1;
        }
        let extras = (!extras.is_null()).then(|| extras.clone());
        let mut mesh = built.mesh;
        let mut matrix = *matrix;
        let mut children = children;
        if built.origin != ZERO && mesh.is_some() {
            let offset = translate(built.origin);
            if children.is_empty() {
                matrix = mat_mul(&matrix, &offset);
            } else {
                // the block instances below use the block's own coordinates
                let child = self.gltf.add_node(name, Some(&offset), mesh, &[], None);
                children.insert(0, child);
                mesh = None;
            }
        }
        let matrix = (!is_identity(&matrix)).then_some(&matrix);
        Some(self.gltf.add_node(name, matrix, mesh, &children, extras))
    }

    fn geometry(&mut self, block: usize, style: Style) -> Rc<Built> {
        let key = (self.doc.objects[block].handle, style);
        if let Some(built) = self.geom_cache.get(&key) {
            return built.clone();
        }
        let k = self.block_scale.get(&block).copied().unwrap_or(1.0);
        (self.tol, self.weld) = (DEVIATION_M / self.unit / k, WELD_M / self.unit / k);
        let mut geo = Geometry::default();
        for i in self.doc.block_entities(block) {
            self.entity(i, style, &mut geo);
        }
        let name = match &self.doc.objects[block].data {
            Data::BlockHeader(b) => b.name.clone(),
            _ => "block".into(),
        };
        // the projection scale only matters to projected textures: else the
        // mesh is shared with the block's instances at other scales
        let projected = geo.tris.iter().any(|((m, _), b)| {
            b.has_unmapped() && self.doc.material(*m).is_some_and(|d| d.has_maps())
        });
        let mesh_key = (
            key.0,
            if projected {
                style
            } else {
                Style {
                    geoscale: Style::default().geoscale,
                    mapper: None,
                    ..style
                }
            },
        );
        let (mesh, origin) = match self.mesh_cache.get(&mesh_key) {
            Some(&shared) => shared,
            None => {
                let built = self.build_mesh(&name, &mut geo, style.geoscale.map(f64::from_bits));
                self.mesh_cache.insert(mesh_key, built);
                built
            }
        };
        let built = Rc::new(Built {
            mesh,
            origin,
            children: geo.children,
        });
        self.geom_cache.insert(key, built.clone());
        built
    }

    // == style resolution ====================================================
    /// Effective (layer handle, rgb, material handle) of an entity.
    fn resolve(&self, ent: &Entity, style: Style) -> (Handle, Rgb, Option<Handle>) {
        let layer_h = match style.layer {
            Some(inherited) if self.doc.is_layer_zero(ent.layer) => inherited,
            _ => ent.layer,
        };
        let layer = self.doc.layers.get(&layer_h);
        let color = match parse_color(ent.color, false) {
            ColorRef::ByLayer => layer.map_or(ACI[7], |l| l.color),
            ColorRef::ByBlock => style.color,
            ColorRef::Rgb(rgb) => rgb,
        };
        let (mflags, mut material) = self.doc.material_ref(ent);
        if mflags == 1 {
            material = style.material;
        } else if mflags == 0 {
            material = layer.and_then(|l| l.material);
        }
        (layer_h, color, material)
    }

    fn visible(&self, layer_h: Handle) -> bool {
        self.doc.layers.get(&layer_h).is_none_or(|l| l.visible)
    }

    fn tri_builder<'g>(
        &self,
        geo: &'g mut Geometry,
        color: Rgb,
        material: Option<Handle>,
    ) -> &'g mut TriangleBuilder {
        let mdef = self.doc.material(material);
        let key = (
            mdef.as_ref().and(material),
            if mdef.as_ref().is_some_and(|m| m.diffuse.is_some()) {
                None
            } else {
                Some(color)
            },
        );
        let i = match geo.tris.iter().position(|(k, _)| *k == key) {
            Some(i) => i,
            None => {
                geo.tris.push((key, TriangleBuilder::new(self.weld)));
                geo.tris.len() - 1
            }
        };
        let builder = &mut geo.tris[i].1;
        builder.set_frame(self.face_frame);
        builder
    }

    fn line_builder(geo: &mut Geometry, color: Rgb) -> &mut LineBuilder {
        let i = match geo.lines.iter().position(|(c, _)| *c == color) {
            Some(i) => i,
            None => {
                geo.lines.push((color, LineBuilder::default()));
                geo.lines.len() - 1
            }
        };
        &mut geo.lines[i].1
    }

    // == entities ===========================================================
    fn entity(&mut self, index: usize, style: Style, geo: &mut Geometry) {
        let doc = self.doc;
        let obj = &doc.objects[index];
        let Data::Entity(ent) = &obj.data else { return };
        let kind = obj.kind.as_str();
        if ent.invisible || STRUCTURAL.contains(&kind) {
            return;
        }
        let (layer_h, color, material) = self.resolve(ent, style);
        // an INSERT on an off (not frozen) layer still shows its content on
        // other layers; what it hands down to layer 0 is hidden with the layer
        let hidden = match (&ent.shape, self.doc.layers.get(&layer_h)) {
            (Shape::Insert { .. }, Some(l)) => l.frozen,
            _ => !self.visible(layer_h),
        };
        if hidden {
            self.stats.skip(format!("{kind} (hidden layer)"), 1);
            return;
        }
        if matches!(ent.shape, Shape::Spline { degree, .. } if degree > MAX_SPLINE_DEGREE) {
            self.stats.skip(format!("{kind} (degree too high)"), 1);
            return;
        }
        let plan = !self.opt.plan_symbols
            && (style.plan
                || self
                    .doc
                    .layers
                    .get(&layer_h)
                    .is_some_and(|l| is_plan_layer(&l.name)));
        let e = EntityRef {
            index,
            obj,
            ent,
            color,
            material,
            plan,
        };
        let (tri_hits, line_hits, plan_hits) = (
            self.tri_limit_hits.get(),
            self.line_limit_hits,
            self.plan_hits,
        );
        self.face_frame = match ent.shape {
            Shape::Insert { .. } => None,
            _ => self.face_frame(kind, ent, style, material),
        };
        let handled = match &ent.shape {
            Shape::Insert { .. } => {
                let inserted = self.insert(
                    &e,
                    Style {
                        color,
                        layer: Some(layer_h),
                        material,
                        geoscale: self.child_geoscale(obj, ent, style),
                        mapper: style.mapper,
                        plan: style.plan,
                    },
                    geo,
                );
                Some(if inserted {
                    Outcome::Converted
                } else {
                    Outcome::Skipped
                })
            }
            Shape::Face3d(..)
            | Shape::Solid { .. }
            | Shape::Polyline { .. }
            | Shape::Mesh { .. }
            | Shape::Line { .. }
            | Shape::Circle { .. }
            | Shape::Arc { .. }
            | Shape::Ellipse { .. }
            | Shape::LwPolyline { .. }
            | Shape::Spline { .. } => Some(if self.shape(&e, geo) {
                Outcome::Converted
            } else {
                Outcome::NoGeometry
            }),
            _ => None,
        };
        // a limit that refused some of the entity's geometry
        let limited = if self.tri_limit_hits.get() > tri_hits {
            Some("triangle limit reached")
        } else if self.line_limit_hits > line_hits {
            Some("line limit reached")
        } else if self.plan_hits > plan_hits {
            Some("2D plan symbol")
        } else {
            None
        };
        match (handled, limited) {
            (Some(Outcome::Skipped), _) => {}
            (Some(_), Some(why)) => self.stats.skip(format!("{kind} ({why})"), 1),
            (Some(Outcome::Converted), None) => {
                *self.stats.converted.entry(kind.into()).or_default() += 1
            }
            (Some(Outcome::NoGeometry), None) => {
                self.stats.skip(format!("{kind} (no geometry)"), 1)
            }
            (None, _) if ACIS_ENTITIES.contains(&kind) => match self.acis(&e, geo) {
                Ok(()) => *self.stats.converted.entry(kind.into()).or_default() += 1,
                Err(loss) => {
                    let why = match loss {
                        AcisLoss::NoData => "no ACIS data",
                        AcisLoss::Unreadable => "unreadable ACIS data",
                        AcisLoss::NoTriangles => "ACIS data gave no triangles",
                        AcisLoss::TriangleLimit => "triangle limit reached",
                    };
                    self.stats.skip(format!("{kind} ({why})"), 1);
                    if self.opt.lines && !e.plan {
                        self.acis_wires(&e, geo);
                    }
                }
            },
            (None, _) if ANNOTATION.contains(&kind) => {
                self.stats.skip(format!("{kind} (annotation)"), 1)
            }
            (None, _) => self.stats.skip(format!("{kind} (unsupported)"), 1),
        }
        self.face_frame = None;
    }

    /// The projection frame of an entity's faces from AutoCAD's material
    /// mapper, which goes with the material: with a ByBlock material the
    /// mapper of the INSERT the material comes from (else the entity's
    /// own), with any other material the entity's own. An identity mapper
    /// is the entity's coordinates, one repeat per drawing unit. An
    /// INSERT's mapper is applied as it is to the coordinates of the block
    /// the faces are in, also several INSERTs further down: it is not
    /// carried through the nested INSERTs (see
    /// docs/IMPLEMENTATION_NOTES.md). None: no mapper (the material's own
    /// projection in meters), or a cylindrical or spherical one, which is
    /// not followed (counted). Faces with stored UVs keep them.
    fn face_frame(
        &mut self,
        kind: &str,
        ent: &Entity,
        style: Style,
        material: Option<Handle>,
    ) -> Option<UvFrame> {
        let own = ent.material_mapper.as_ref().and_then(InheritedMapper::of);
        let chosen = if self.doc.material_ref(ent).0 == 1 {
            style.mapper.or(own)
        } else {
            own
        }?;
        let projection = match chosen.projection {
            mapper::PLANAR => Projection::Axis(2),
            mapper::BOX => Projection::Box,
            p => {
                if self.doc.material(material).is_some_and(|m| m.has_maps()) {
                    let what = if p == mapper::CYLINDER {
                        "cylindrical"
                    } else if p == mapper::SPHERE {
                        "spherical"
                    } else {
                        "unknown"
                    };
                    self.stats
                        .skip(format!("{kind} material mapper ({what}, not followed)"), 1);
                    self.stats.warn(format!(
                        "{what} material mappers are not followed; their faces are projected as without one"
                    ));
                }
                return None;
            }
        };
        Some(UvFrame {
            projection,
            to_frame: chosen.frame(),
        })
    }

    /// Geometry of a supported entity; false when it has none.
    fn shape(&mut self, e: &EntityRef, geo: &mut Geometry) -> bool {
        let (color, material) = (e.color, e.material);
        match &e.ent.shape {
            Shape::Face3d(corners) => {
                let pts = if corners[3] == corners[2] {
                    &corners[..3]
                } else {
                    &corners[..]
                };
                self.polygon(geo, color, material, pts, None, None)
            }
            Shape::Solid { corners, elevation } => self.solid(e, geo, corners, *elevation),
            Shape::Polyline { .. } => match e.obj.kind.as_str() {
                "POLYLINE_PFACE" => self.polyline_pface(e, geo),
                "POLYLINE_MESH" => self.polyline_mesh(e, geo),
                "POLYLINE_2D" => self.polyline_2d(e, geo),
                _ => self.polyline_3d(e, geo),
            },
            Shape::Mesh { vertices, faces } => self.mesh(e, geo, vertices, faces),
            Shape::Line { start, end } => self.curve(geo, e, vec![*start, *end], false),
            Shape::Circle { center, radius } => {
                if *radius <= 0.0 {
                    return false;
                }
                let mut pts = arc_points(*center, *radius, 0.0, TAU, self.tol);
                pts.pop();
                let pts = self.ocs_curve(e.ent, &pts);
                self.curve(geo, e, pts, true)
            }
            Shape::Arc {
                center,
                radius,
                start_angle,
                end_angle,
            } => {
                if *radius <= 0.0 {
                    return false;
                }
                let pts = arc_points(*center, *radius, *start_angle, *end_angle, self.tol);
                let pts = self.ocs_curve(e.ent, &pts);
                self.curve(geo, e, pts, false)
            }
            Shape::Ellipse {
                center,
                major,
                ratio,
                start_angle,
                end_angle,
            } => self.ellipse(e, geo, *center, *major, *ratio, *start_angle, *end_angle),
            Shape::LwPolyline {
                flag,
                elevation,
                points,
                bulges,
                const_width,
                widths,
            } => {
                if points.len() < 2 {
                    return false;
                }
                let pts: Vec<Vec3> = points.iter().map(|p| [p[0], p[1], *elevation]).collect();
                let closed = flag & 512 != 0;
                let widths: Vec<[f64; 2]> = (0..pts.len())
                    .map(|i| widths.get(i).copied().unwrap_or([*const_width; 2]))
                    .collect();
                self.polyline_2d_points(e, geo, &pts, bulges, &widths, closed)
            }
            Shape::Spline {
                flag,
                degree,
                ctrl,
                weights,
                knots,
                fit,
            } => {
                let degree = if *degree != 0 { *degree } else { 3 };
                let pts = if !ctrl.is_empty() && !knots.is_empty() {
                    bspline_points(degree, ctrl, knots, weights, (8 * ctrl.len()).max(16))
                } else {
                    catmull_rom(fit, 8)
                };
                let closed = flag & 1 != 0 && pts.len() > 2 && pts[0] != pts[pts.len() - 1];
                self.curve(geo, e, pts, closed)
            }
            _ => false,
        }
    }

    // -- blocks --------------------------------------------------------------
    /// Add the block instances of an INSERT / MINSERT to `geo`; false when
    /// it is skipped (the reason recorded).
    fn insert(&mut self, e: &EntityRef, style: Style, geo: &mut Geometry) -> bool {
        let kind = e.obj.kind.as_str();
        let Shape::Insert {
            block,
            ins_pt,
            scale: s,
            rotation,
            cols,
            rows,
            col_spacing,
            row_spacing,
        } = &e.ent.shape
        else {
            return false;
        };
        let Some(bi) = self
            .doc
            .index(*block)
            .filter(|&i| matches!(self.doc.objects[i].data, Data::BlockHeader(_)))
        else {
            self.stats.skip(format!("{kind} (missing block)"), 1);
            return false;
        };
        if s.contains(&0.0) {
            self.stats.skip(format!("{kind} (zero scale)"), 1);
            return false;
        }
        let Data::BlockHeader(b) = &self.doc.objects[bi].data else {
            unreachable!()
        };
        // the blocks being built (`emit`): converting the block the INSERT
        // is in, or one that contains that block; AutoCAD refuses these, and
        // every level would double the instances
        if self.ancestors.contains(&bi) {
            self.stats.skip(format!("{kind} (block inserts itself)"), 1);
            return false;
        }
        if b.xref.is_some() {
            // the content is in another drawing, which is not read
            self.stats.warn(format!(
                "INSERT of external reference {} (not resolved)",
                b.name
            ));
            self.stats.skip(format!("{kind} (xref)"), 1);
            return false;
        }
        let (cols, rows) = ((*cols).max(1), (*rows).max(1));
        let cells = cols as usize * rows as usize;
        if self.children_made + cells > self.opt.limits.instances {
            self.instance_limit_hits += 1;
            self.stats
                .skip(format!("{kind} (instance limit reached)"), 1);
            return false;
        }
        self.children_made += cells;
        let head = chain(&[
            ocs_matrix(e.ent.extrusion),
            translate(*ins_pt),
            rotate_z(*rotation),
        ]);
        let tail = chain(&[scale(*s), translate(mul(b.base_pt, -1.0))]);
        let extras = json!({"block": b.name, "handle": format!("{:X}", e.obj.handle)});
        // the material mapper goes with the material the INSERT hands down:
        // its own, or with a ByBlock material the one handed down to it;
        // it only matters to textures, and would split shared meshes
        let own = e
            .ent
            .material_mapper
            .as_ref()
            .filter(|_| self.textured)
            .and_then(InheritedMapper::of);
        let style = Style {
            plan: style.plan || b.name.starts_with(PLAN_BLOCK),
            mapper: if self.doc.material_ref(e.ent).0 == 1 {
                style.mapper.or(own)
            } else {
                own
            },
            ..style
        };
        for r in 0..rows {
            for c in 0..cols {
                let m = chain(&[
                    head,
                    translate([c as f64 * col_spacing, r as f64 * row_spacing, 0.0]),
                    tail,
                ]);
                geo.children.push(Child {
                    name: b.name.clone(),
                    matrix: m,
                    block: bi,
                    style,
                    extras: extras.clone(),
                });
            }
        }
        true
    }

    // -- faces ---------------------------------------------------------------
    fn polygon(
        &self,
        geo: &mut Geometry,
        color: Rgb,
        material: Option<Handle>,
        pts: &[Vec3],
        normals: Option<&[Vec3]>,
        uvs: Option<&[Vec2]>,
    ) -> bool {
        if pts.len() < 3 || !pts.iter().all(|&p| finite(p)) {
            return false;
        }
        // counted as a fan (holes are not passed here)
        let used = self.tris_used.get() + pts.len() - 2;
        if used > self.opt.limits.triangles {
            self.tri_limit_hits.set(self.tri_limit_hits.get() + 1);
            return false;
        }
        self.tris_used.set(used);
        self.tri_builder(geo, color, material)
            .add_polygon(pts, normals, uvs);
        true
    }

    fn polyline_pface(&mut self, e: &EntityRef, geo: &mut Geometry) -> bool {
        let mut verts = vec![];
        let mut faces = vec![];
        for i in self.doc.sub_entities(e.index, "VERTEX_PFACE") {
            let Data::Entity(v) = &self.doc.objects[i].data else {
                continue;
            };
            match &v.shape {
                Shape::PfaceFace(ind) => faces.push((ind, v.color)),
                Shape::Vertex { point, .. } => verts.push(*point),
                _ => verts.push([0.0; 3]),
            }
        }
        if faces.is_empty() {
            return false;
        }
        for (ind, fcolor) in faces {
            let used: Vec<usize> = ind
                .iter()
                .filter(|&&i| i != 0)
                .map(|&i| i.unsigned_abs() as usize - 1)
                .collect();
            if used.iter().any(|&i| i >= verts.len()) {
                self.stats
                    .skip("POLYLINE_PFACE face (invalid vertex number)", 1);
                continue;
            }
            let pts: Vec<Vec3> = used.iter().map(|&i| verts[i]).collect();
            let pts = dedup_ring(&pts);
            // face records may carry their own color
            let c = match parse_color(fcolor, false) {
                ColorRef::Rgb(rgb) => rgb,
                _ => e.color,
            };
            self.polygon(geo, c, e.material, &pts, None, None);
        }
        true
    }

    fn polyline_mesh(&mut self, e: &EntityRef, geo: &mut Geometry) -> bool {
        let Shape::Polyline { flag, m, n, .. } = e.ent.shape else {
            return false;
        };
        let verts = self.vertices(e.index, "VERTEX_MESH");
        let (m, n) = (m as usize, n as usize);
        if m < 2 || n < 2 || m * n > verts.len() {
            return false;
        }
        let mi = if flag & 1 != 0 { m } else { m - 1 };
        let ni = if flag & 32 != 0 { n } else { n - 1 };
        for i in 0..mi {
            for j in 0..ni {
                let (a, b) = (i * n + j, i * n + (j + 1) % n);
                let (c, d) = (((i + 1) % m) * n + (j + 1) % n, ((i + 1) % m) * n + j);
                self.polygon(
                    geo,
                    e.color,
                    e.material,
                    &[verts[a].0, verts[b].0, verts[c].0, verts[d].0],
                    None,
                    None,
                );
            }
        }
        true
    }

    /// (point, bulge, widths) of the vertices that are not spline frame
    /// control points.
    fn vertices(&self, parent: usize, kind_prefix: &str) -> Vec<(Vec3, f64, [f64; 2])> {
        self.doc
            .sub_entities(parent, kind_prefix)
            .into_iter()
            .filter_map(|i| match &self.doc.objects[i].data {
                Data::Entity(v) => match v.shape {
                    Shape::Vertex {
                        point,
                        bulge,
                        flag,
                        widths,
                    } => (flag & 16 == 0).then_some((point, bulge, widths)),
                    _ => Some(([0.0; 3], 0.0, [0.0; 2])),
                },
                _ => None,
            })
            .collect()
    }

    fn mesh(&mut self, e: &EntityRef, geo: &mut Geometry, verts: &[Vec3], flat: &[u32]) -> bool {
        // AcDbSubDMesh, drawn as its base cage (level 0)
        if verts.is_empty() || flat.is_empty() {
            return false;
        }
        let normals = self
            .xrecord_doubles(e.obj, ofml::VERTEX_NORMALS)
            .filter(|n| n.len() == verts.len());
        let uvs = self
            .tex_coords(e.obj, verts.len())
            .or_else(|| self.adsk_tex_coords(e.obj, verts.len()));
        // a MESH without normals is smoothed at its own angle, if it has one
        let smoothing = self
            .xrecord_real(e.obj, ofml::NORMAL_SMOOTHNESS, 50)
            .unwrap_or(OFML_MESH_SMOOTHING);
        self.tri_builder(geo, e.color, e.material)
            .set_smoothing(smoothing);
        let mut invalid = 0;
        let mut i = 0;
        while i < flat.len() {
            let count = flat[i] as usize;
            let idx: Vec<usize> = flat[i + 1..(i + 1 + count).min(flat.len())]
                .iter()
                .map(|&k| k as usize)
                .collect();
            i += count + 1;
            if idx.len() < count || idx.iter().any(|&k| k >= verts.len()) {
                invalid += 1;
                continue;
            }
            if count < 3 {
                continue;
            }
            let pts: Vec<Vec3> = idx.iter().map(|&k| verts[k]).collect();
            let face_normals: Option<Vec<Vec3>> = normals
                .as_ref()
                .map(|n| idx.iter().map(|&k| n[k]).collect());
            let face_uvs: Option<Vec<Vec2>> =
                uvs.as_ref().map(|t| idx.iter().map(|&k| t[k]).collect());
            self.polygon(
                geo,
                e.color,
                e.material,
                &pts,
                face_normals.as_deref(),
                face_uvs.as_deref(),
            );
        }
        self.tri_builder(geo, e.color, e.material)
            .set_smoothing(crate::mesh::SMOOTH_DEGREES);
        if invalid > 0 {
            self.stats.skip("MESH face (invalid vertex index)", invalid);
        }
        true
    }

    /// A real (group `code`) from the entity's extension record `key`.
    fn xrecord_real(&self, obj: &Object, key: &str, code: i16) -> Option<f64> {
        let Data::XRecord(items) = &self.doc.xrecord(obj, key)?.data else {
            return None;
        };
        items.iter().find_map(|(c, v)| match v {
            XValue::Real(x) if *c == code => Some(*x),
            _ => None,
        })
    }

    /// The texture projection scale an INSERT hands down: the OFML geometry
    /// scale recorded on it, else the inherited one times the INSERT's
    /// own scale when that enlarges the block. Textures are projected on the
    /// scaled geometry then (blocks in meters inserted x100 into a
    /// centimeter drawing show 3.3 repeats per meter, not 0.03). Sizes
    /// only, see [`Style::geoscale`].
    fn child_geoscale(&self, obj: &Object, ent: &Entity, style: Style) -> [u64; 3] {
        if let Some(s) = self.geoscale(obj) {
            return s.map(|c| c.abs().to_bits());
        }
        let Shape::Insert { scale: s, .. } = &ent.shape else {
            return style.geoscale;
        };
        if !self.textured {
            // it only matters for textures, and would split shared meshes
            return style.geoscale;
        }
        if s.iter().any(|c| c.abs() < 1.0) {
            // a shrunk block (blocks in mm inserted x0.001 into a meter
            // drawing) is as often textured in its block coordinates as
            // not; an empirical cut that changes neither, see
            // docs/IMPLEMENTATION_NOTES.md
            return style.geoscale;
        }
        std::array::from_fn(|k| {
            let c = f64::from_bits(style.geoscale[k]) * s[k].abs();
            if c.is_finite() && c > 0.0 {
                c.to_bits()
            } else {
                style.geoscale[k]
            }
        })
    }

    /// The OFML geometry scale recorded on an INSERT.
    fn geoscale(&self, obj: &Object) -> Option<[f64; 3]> {
        let Data::XRecord(items) = &self.doc.xrecord(obj, ofml::MAPPING_INFO)?.data else {
            return None;
        };
        items.iter().find_map(|(_, v)| match v {
            XValue::Text(t) => ofml::geoscale(t),
            _ => None,
        })
    }

    /// OFML per-vertex UVs: XRECORD EDWG_TEX_COORDS_2D holding u/v pairs of
    /// doubles; NaN reads as 0; count must match.
    fn tex_coords(&self, obj: &Object, count: usize) -> Option<Vec<Vec2>> {
        let data = binary_chunks(self.doc.xrecord(obj, ofml::TEX_COORDS)?);
        if data.len() % 16 != 0 || data.len() / 16 != count {
            return None;
        }
        let val = |c: &[u8]| {
            let v = f64::from_le_bytes(c.try_into().unwrap());
            if v.is_finite() { v } else { 0.0 }
        };
        Some(
            data.chunks_exact(16)
                .map(|c| [val(&c[..8]), val(&c[8..])])
                .collect(),
        )
    }

    /// AutoCAD's MESH texture coordinates, which many OFML meshes carry
    /// instead of EDWG_TEX_COORDS_2D: XRECORD
    /// ADSK_XREC_SUBDVERTEXTEXCOORDS with reals 43/44/45 = u/v/w per vertex
    /// (level 0). NaN reads as 0; the count must match.
    fn adsk_tex_coords(&self, obj: &Object, count: usize) -> Option<Vec<Vec2>> {
        let Data::XRecord(items) = &self.doc.xrecord(obj, ADSK_TEX_COORDS)?.data else {
            return None;
        };
        let (mut u, mut v) = (vec![], vec![]);
        for (code, value) in items {
            let XValue::Real(x) = value else { continue };
            let x = if x.is_finite() { *x } else { 0.0 };
            match code {
                43 => u.push(x),
                44 => v.push(x),
                _ => {}
            }
        }
        (u.len() == count && v.len() == count)
            .then(|| u.into_iter().zip(v).map(|(a, b)| [a, b]).collect())
    }

    /// Vertex normals: doubles with group codes 10..59, three per vertex.
    fn xrecord_doubles(&self, obj: &Object, key: &str) -> Option<Vec<Vec3>> {
        let Data::XRecord(items) = &self.doc.xrecord(obj, key)?.data else {
            return None;
        };
        let vals: Vec<f64> = items
            .iter()
            .filter_map(|(code, v)| match v {
                XValue::Real(x) if (10..60).contains(code) => Some(*x),
                _ => None,
            })
            .collect();
        if vals.is_empty() || vals.len() % 3 != 0 {
            return None;
        }
        Some(vals.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect())
    }

    // -- curves --------------------------------------------------------------
    fn curve(&mut self, geo: &mut Geometry, e: &EntityRef, pts: Vec<Vec3>, closed: bool) -> bool {
        let pts: Vec<Vec3> = pts.into_iter().filter(|&p| finite(p)).collect();
        if pts.len() < 2 {
            return false;
        }
        let thickness = e.ent.thickness;
        if thickness != 0.0 {
            // AutoCAD extrudes curves with thickness along their normal; a
            // circle becomes a closed cylinder, other curves get walls only
            let normal = ocs_axes(e.ent.extrusion)[2];
            let offset = mul(normal, thickness);
            if closed && e.obj.kind == "CIRCLE" {
                self.prism(geo, e.color, e.material, &pts, offset);
                return true;
            }
            // closed: walls facing outwards (around the extrusion direction)
            let mut ring = pts.clone();
            if closed {
                if dot(crate::geom::newell_normal(&pts), offset) < 0.0 {
                    ring.reverse();
                }
                ring.push(ring[0]);
            }
            for w in ring.windows(2) {
                let (p, q) = (w[0], w[1]);
                self.polygon(
                    geo,
                    e.color,
                    e.material,
                    &[p, q, add(q, offset), add(p, offset)],
                    None,
                    None,
                );
            }
            return true;
        }
        if !self.opt.lines {
            return false;
        }
        if e.plan {
            // OFML 2D plan symbol: for plan views only, never in 3D
            self.plan_hits += 1;
            return false;
        }
        if !self.line_budget(pts.len()) {
            return false;
        }
        Self::line_builder(geo, e.color).add_polyline(&pts, closed);
        true
    }

    /// Take `n` line points from the limit; false (and counted) when they
    /// would go over it.
    fn line_budget(&mut self, n: usize) -> bool {
        if self.line_points + n > self.opt.limits.line_points {
            self.line_limit_hits += 1;
            return false;
        }
        self.line_points += n;
        true
    }

    fn ocs_curve(&self, ent: &Entity, pts: &[Vec3]) -> Vec<Vec3> {
        let m = ocs_matrix(ent.extrusion);
        pts.iter().map(|&p| apply(&m, p)).collect()
    }

    #[allow(clippy::too_many_arguments)]
    fn ellipse(
        &mut self,
        e: &EntityRef,
        geo: &mut Geometry,
        center: Vec3,
        major: Vec3,
        ratio: f64,
        t0: f64,
        t1: f64,
    ) -> bool {
        // a stored 0 means the default, as in DXF
        let ratio = if ratio != 0.0 { ratio } else { 1.0 };
        let t1 = if t1 != 0.0 { t1 } else { TAU };
        let normal = ocs_axes(e.ent.extrusion)[2];
        let minor = mul(normalize(cross(normal, major)), length(major) * ratio);
        let Some(t1) = arc_end(t0, t1) else {
            return false;
        };
        // an ellipse deviates from its chords no more than the circle of its
        // major radius does at the same parameter step
        let n = arc_segments(length(major), t1 - t0, self.tol);
        let mut pts: Vec<Vec3> = (0..=n)
            .map(|i| {
                let t = t0 + (t1 - t0) * i as f64 / n as f64;
                add(center, add(mul(major, t.cos()), mul(minor, t.sin())))
            })
            .collect();
        let closed = (t1 - t0 - TAU).abs() < 1e-9;
        if closed {
            pts.pop();
        }
        self.curve(geo, e, pts, closed)
    }

    fn bulged(&self, pts: &[Vec3], bulges: &[f64], closed: bool) -> Vec<Vec3> {
        let mut out = vec![pts[0]];
        let count = if closed { pts.len() } else { pts.len() - 1 };
        for i in 0..count {
            let b = bulges.get(i).copied().unwrap_or(0.0);
            out.extend(bulge_points(pts[i], pts[(i + 1) % pts.len()], b, self.tol));
        }
        if closed {
            out.pop(); // last point repeats the first
        }
        out
    }

    fn polyline_2d(&mut self, e: &EntityRef, geo: &mut Geometry) -> bool {
        let Shape::Polyline {
            flag, elevation, ..
        } = e.ent.shape
        else {
            return false;
        };
        let Shape::Polyline {
            widths: default, ..
        } = e.ent.shape
        else {
            return false;
        };
        let verts = self.vertices(e.index, "VERTEX_2D");
        if verts.len() < 2 {
            return false;
        }
        let pts: Vec<Vec3> = verts
            .iter()
            .map(|(p, ..)| [p[0], p[1], elevation])
            .collect();
        let bulges: Vec<f64> = verts.iter().map(|v| v.1).collect();
        // vertices without widths take the polyline's
        let widths: Vec<[f64; 2]> = verts
            .iter()
            .map(|v| if v.2 == [0.0; 2] { default } else { v.2 })
            .collect();
        self.polyline_2d_points(e, geo, &pts, &bulges, &widths, flag & 1 != 0)
    }

    /// A 2D polyline (OCS points at their elevation): a curve, or where
    /// segments have a width, a filled band (a slab with thickness).
    fn polyline_2d_points(
        &mut self,
        e: &EntityRef,
        geo: &mut Geometry,
        pts: &[Vec3],
        bulges: &[f64],
        widths: &[[f64; 2]],
        closed: bool,
    ) -> bool {
        let count = if closed { pts.len() } else { pts.len() - 1 };
        if (0..count).all(|i| widths[i] == [0.0; 2]) {
            let out = self.bulged(pts, bulges, closed);
            let out = self.ocs_curve(e.ent, &out);
            return self.curve(geo, e, out, closed);
        }
        // per segment: its points (from the bulge) with their half widths
        let mut runs: Vec<Vec<(Vec3, f64)>> = vec![];
        let mut thin: Vec<Vec<Vec3>> = vec![];
        for i in 0..count {
            let (p, q) = (pts[i], pts[(i + 1) % pts.len()]);
            let mut seg = vec![p];
            seg.extend(bulge_points(
                p,
                q,
                bulges.get(i).copied().unwrap_or(0.0),
                self.tol,
            ));
            let [w0, w1] = widths[i].map(f64::abs);
            if w0 == 0.0 && w1 == 0.0 {
                thin.push(seg);
                continue;
            }
            // half width along the segment, by distance
            let mut lens = vec![0.0];
            for k in 1..seg.len() {
                lens.push(lens[k - 1] + length(sub(seg[k], seg[k - 1])));
            }
            let total = lens[lens.len() - 1].max(1e-300);
            let run: Vec<(Vec3, f64)> = seg
                .iter()
                .zip(&lens)
                .map(|(&p, &l)| (p, (w0 + (w1 - w0) * l / total) / 2.0))
                .collect();
            match runs.last_mut() {
                // continue the band through the vertex when the width does
                Some(last)
                    if last
                        .last()
                        .is_some_and(|&(lp, lw)| lp == run[0].0 && lw == run[0].1) =>
                {
                    last.extend_from_slice(&run[1..]);
                }
                _ => runs.push(run),
            }
        }
        // a closed band that goes all the way round joins up
        let ring = closed && runs.len() == 1 && thin.is_empty();
        for run in &runs {
            for quad in band(run, ring && run.len() > 2) {
                let quad = self.ocs_curve(e.ent, &quad);
                if e.ent.thickness != 0.0 {
                    let offset = mul(ocs_axes(e.ent.extrusion)[2], e.ent.thickness);
                    self.prism(geo, e.color, e.material, &quad, offset);
                } else {
                    self.polygon(geo, e.color, e.material, &quad, None, None);
                }
            }
        }
        for seg in thin {
            let seg = self.ocs_curve(e.ent, &seg);
            self.curve(geo, e, seg, false);
        }
        true
    }

    /// A closed prism: `base` (counter-clockwise around the extrusion
    /// direction), the same shape moved by `offset`, and the walls between,
    /// all facing outwards.
    fn prism(
        &self,
        geo: &mut Geometry,
        color: Rgb,
        material: Option<Handle>,
        base: &[Vec3],
        offset: Vec3,
    ) {
        let mut base = base.to_vec();
        // the top must face along the offset
        if dot(crate::geom::newell_normal(&base), offset) < 0.0 {
            base.reverse();
        }
        let top: Vec<Vec3> = base.iter().map(|&p| add(p, offset)).collect();
        let mut bottom = base.clone();
        bottom.reverse();
        self.polygon(geo, color, material, &bottom, None, None);
        self.polygon(geo, color, material, &top, None, None);
        for i in 0..base.len() {
            let (p, q) = (base[i], base[(i + 1) % base.len()]);
            self.polygon(
                geo,
                color,
                material,
                &[p, q, add(q, offset), add(p, offset)],
                None,
                None,
            );
        }
    }

    /// SOLID / TRACE: a filled triangle or quad in OCS (corner order 1-2-4-3,
    /// crossed if the corners were given around the perimeter), a prism with
    /// thickness. Faces point along the extrusion direction.
    fn solid(
        &mut self,
        e: &EntityRef,
        geo: &mut Geometry,
        corners: &[Vec3; 4],
        elevation: f64,
    ) -> bool {
        let [ax, ay, az] = ocs_axes(e.ent.extrusion);
        let at = |c: [f64; 2]| add(add(mul(ax, c[0]), mul(ay, c[1])), mul(az, elevation));
        let [c1, c2, c3, c4] = corners.map(|c| [c[0], c[1]]);
        let cross2 = |o: [f64; 2], a: [f64; 2], b: [f64; 2]| {
            (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
        };
        // outline 1-2-4-3; when edges 2-4 and 3-1 cross it is a bow tie
        let crossing = (cross2(c2, c4, c3) > 0.0) != (cross2(c2, c4, c1) > 0.0)
            && (cross2(c3, c1, c2) > 0.0) != (cross2(c3, c1, c4) > 0.0);
        let shapes: Vec<Vec<[f64; 2]>> = if crossing {
            let (d1, d2) = (cross2(c3, c1, c2), cross2(c3, c1, c4));
            let t = d1 / (d1 - d2);
            let x = [c2[0] + (c4[0] - c2[0]) * t, c2[1] + (c4[1] - c2[1]) * t];
            vec![vec![c1, c2, x], vec![x, c4, c3]]
        } else {
            vec![vec![c1, c2, c4, c3]]
        };
        let offset = mul(az, e.ent.thickness);
        let mut any = false;
        for shape in shapes {
            let mut pts = dedup_ring(&shape.iter().map(|&c| at(c)).collect::<Vec<_>>());
            if pts.len() < 3 {
                continue;
            }
            if dot(crate::geom::newell_normal(&pts), az) < 0.0 {
                pts.reverse();
            }
            if e.ent.thickness != 0.0 {
                self.prism(geo, e.color, e.material, &pts, offset);
            } else {
                self.polygon(geo, e.color, e.material, &pts, None, None);
            }
            any = true;
        }
        any
    }

    fn polyline_3d(&mut self, e: &EntityRef, geo: &mut Geometry) -> bool {
        let Shape::Polyline { flag, .. } = e.ent.shape else {
            return false;
        };
        let pts = self
            .vertices(e.index, "VERTEX_3D")
            .into_iter()
            .map(|v| v.0)
            .collect();
        self.curve(geo, e, pts, flag & 1 != 0)
    }

    // -- ACIS ------------------------------------------------------------------
    fn acis(&mut self, e: &EntityRef, geo: &mut Geometry) -> Result<(), AcisLoss> {
        let data = self.doc.acis_data(e.obj).ok_or(AcisLoss::NoData)?;
        let model = sab::parse(&data).map_err(|_| AcisLoss::Unreadable)?;
        let bodies = Brep::new(&model).bodies();
        let mut tess = Tessellator::new(self.tol);
        let left = self
            .opt
            .limits
            .triangles
            .saturating_sub(self.tris_used.get());
        let mut tris: Vec<[Vec3; 3]> = vec![];
        let mut normals: Vec<Option<[Vec3; 3]>> = vec![];
        for b in &bodies {
            tess.budget = left - tris.len();
            tris.extend(tess.body(b));
            normals.append(&mut tess.normals);
            if tess.over_budget {
                break;
            }
        }
        if tess.failed_faces > 0 {
            self.stats
                .skip("ACIS face (tessellation failed)", tess.failed_faces);
        }
        for (what, n) in [
            (
                "ACIS face filled from its boundary only",
                tess.approximated_faces,
            ),
            (
                "ACIS face triangulated with a fallback",
                tess.degraded_faces,
            ),
            ("ACIS face refined to the triangle limit", tess.capped_faces),
        ] {
            if n > 0 {
                *self.stats.repaired.entry(what.into()).or_default() += n;
            }
        }
        if tess.over_budget {
            self.tri_limit_hits.set(self.tri_limit_hits.get() + 1);
            return Err(AcisLoss::TriangleLimit);
        }
        if tris.is_empty() {
            return Err(AcisLoss::NoTriangles);
        }
        self.tris_used.set(self.tris_used.get() + tris.len());
        let builder = self.tri_builder(geo, e.color, e.material);
        builder.set_oriented(true);
        // the surfaces' own normals: smoothing the facets shades coarse or
        // sliver triangles unevenly
        for (t, n) in tris.iter().zip(&normals) {
            if t.iter().all(|&p| finite(p)) {
                builder.add_polygon(t, n.as_ref().map(|n| &n[..]), None);
            }
        }
        builder.set_oriented(false);
        Ok(())
    }

    /// Keep the isoline wireframe AutoCAD caches next to ACIS data.
    fn acis_wires(&mut self, e: &EntityRef, geo: &mut Geometry) {
        let Shape::Acis { wires, .. } = &e.ent.shape else {
            return;
        };
        for pts in wires {
            if pts.len() >= 2 && pts.iter().all(|&p| finite(p)) && self.line_budget(pts.len()) {
                Self::line_builder(geo, e.color).add_polyline(pts, false);
            }
        }
    }

    // == meshes and materials ===============================================
    /// The block's mesh, and the origin its positions are relative to.
    fn build_mesh(
        &mut self,
        name: &str,
        geo: &mut Geometry,
        geoscale: Vec3,
    ) -> (Option<usize>, Vec3) {
        let origin = self.mesh_origin(geo);
        let mut prims = vec![];
        for ((material, color), builder) in std::mem::take(&mut geo.tris) {
            if builder.is_empty() {
                continue;
            }
            let mdef = self.doc.material(material);
            let textured = mdef.as_ref().is_some_and(|m| m.has_maps());
            let data = match &mdef {
                Some(m) if textured => builder.build(
                    Some(m.projection),
                    mul(geoscale, self.unit),
                    Some(m.uv_matrix),
                    origin,
                ),
                _ => builder.build(None, [1.0; 3], None, origin),
            };
            self.stats.mesh_repairs(data.repairs);
            if data.indices.is_empty() {
                continue;
            }
            let mut attrs = json!({
                "POSITION": self.gltf.vec_accessor(&data.positions, true),
                "NORMAL": self.gltf.vec_accessor(&data.normals, false),
            });
            if let (Some(uvs), true) = (&data.uvs, textured) {
                attrs["TEXCOORD_0"] = json!(self.gltf.vec_accessor(uvs, false));
            }
            let indices = self
                .gltf
                .index_accessor(&data.indices, data.positions.len());
            let material = self.material(material, color);
            prims.push(json!({"attributes": attrs, "indices": indices, "material": material, "mode": TRIANGLES}));
            self.stats.triangles += data.indices.len() / 3;
        }
        for (color, builder) in std::mem::take(&mut geo.lines) {
            if builder.is_empty() {
                continue;
            }
            let positions: Vec<Vec3> = builder.positions.iter().map(|&p| sub(p, origin)).collect();
            let position = self.gltf.vec_accessor(&positions, true);
            let indices = self
                .gltf
                .index_accessor(&builder.indices, builder.positions.len());
            let material = self.line_material(color);
            prims.push(json!({"attributes": {"POSITION": position}, "indices": indices, "material": material, "mode": LINES}));
            self.stats.lines += builder.indices.len() / 2;
        }
        (
            (!prims.is_empty()).then(|| self.gltf.add_mesh(name, prims)),
            origin,
        )
    }

    /// The centre of the block's geometry when its coordinates are so large
    /// that float32 would round them by more than the weld distance (2^-23
    /// relative), else the block origin.
    fn mesh_origin(&self, geo: &Geometry) -> Vec3 {
        let mut pts = vec![];
        for (_, b) in &geo.tris {
            if let Some((lo, hi)) = b.bounds() {
                pts.extend([lo, hi]);
            }
        }
        for (_, b) in &geo.lines {
            if let Some((lo, hi)) = bounds(&b.positions) {
                pts.extend([lo, hi]);
            }
        }
        let Some((lo, hi)) = bounds(&pts) else {
            return ZERO;
        };
        let far = lo.iter().chain(&hi).fold(0.0f64, |m, c| m.max(c.abs()));
        if far * f64::powi(2.0, -23) <= self.weld {
            return ZERO;
        }
        std::array::from_fn(|k| (lo[k] + hi[k]) / 2.0)
    }

    fn material(&mut self, material: Option<Handle>, color: Option<Rgb>) -> usize {
        if let Some(&idx) = self.materials.get(&(material, color)) {
            return idx;
        }
        let white = ACI[7];
        let idx = match self.doc.material(material) {
            None => {
                let c = color.unwrap_or(white);
                self.gltf
                    .add_material(MaterialDesc::new(&format!("color_{}", hex(c)), rgb_unit(c)))
            }
            Some(mdef) => {
                let mut texture = None;
                if let (Some((data, mime)), Some(h)) = (&mdef.texture, material) {
                    let gltf = &mut self.gltf;
                    texture = Some(
                        *self
                            .textures
                            .entry(h)
                            .or_insert_with(|| gltf.add_image_texture(data, mime, &mdef.name)),
                    );
                }
                let mut rgb = rgb_unit(mdef.diffuse.or(color).unwrap_or(white));
                if texture.is_some() {
                    rgb = [1.0; 3]; // the texture carries the colour, as in OFML material archives
                }
                let mut desc = MaterialDesc::new(&mdef.name, rgb);
                desc.alpha = mdef.opacity;
                desc.metallic = mdef.metallic;
                desc.roughness = mdef.roughness;
                desc.texture = texture;
                if let Some(n) = &mdef.normal {
                    let name = format!("{}_normal", mdef.name);
                    let index = self.gltf.add_image_texture(&n.data, n.mime, &name);
                    desc.normal = Some(NormalTexture {
                        index,
                        transform: normal_transform(mdef.uv_matrix, n.uv_delta),
                    });
                }
                desc.extras = (!mdef.extras.is_empty()).then(|| Value::Object(mdef.extras.clone()));
                self.gltf.add_material(desc)
            }
        };
        self.materials.insert((material, color), idx);
        idx
    }

    fn line_material(&mut self, color: Rgb) -> usize {
        if let Some(&idx) = self.line_materials.get(&color) {
            return idx;
        }
        let name = format!("line_{}", hex(color));
        let mut desc = MaterialDesc::new(&name, rgb_unit(color));
        desc.unlit = true;
        let idx = self.gltf.add_material(desc);
        self.line_materials.insert(color, idx);
        idx
    }
}

/// The normal map's `KHR_texture_transform` on the base color's glTF UVs:
/// the base color has `uv_matrix` (as [`gltf_uv_affine`] applies it after
/// the v flip), the normal map `uv_matrix · uv_delta` the same way. None
/// when they agree, or when the difference shears (which
/// `KHR_texture_transform` cannot express).
fn normal_transform(uv_matrix: [f64; 6], uv_delta: [f64; 6]) -> Option<TextureTransform> {
    const IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];
    if uv_delta
        .iter()
        .zip(IDENTITY)
        .all(|(a, b)| (a - b).abs() < 1e-9)
    {
        return None;
    }
    let base = gltf_uv_affine(uv_matrix);
    let normal = gltf_uv_affine(affine_mul(uv_matrix, uv_delta));
    TextureTransform::from_affine(affine_mul(normal, affine_inverse(base)?))
}

/// An entity being converted, with its resolved color and material.
struct EntityRef<'a> {
    index: usize,
    obj: &'a Object,
    ent: &'a Entity,
    color: Rgb,
    material: Option<Handle>,
    /// Part of an OFML 2D plan symbol: its wire geometry is left out.
    plan: bool,
}

/// The largest scale (by absolute value, nested INSERTs multiplied) each
/// block is shown at, from model space down.
fn block_scales(doc: &Document, root: usize) -> HashMap<usize, f64> {
    let mut best: HashMap<usize, f64> = HashMap::from([(root, 1.0)]);
    let mut stack = vec![(root, 1.0, 0)];
    while let Some((block, k, depth)) = stack.pop() {
        if depth > MAX_NESTING {
            continue;
        }
        for i in doc.block_entities(block) {
            let Data::Entity(ent) = &doc.objects[i].data else {
                continue;
            };
            let Shape::Insert {
                block: b, scale: s, ..
            } = &ent.shape
            else {
                continue;
            };
            let Some(bi) = doc.index(*b) else { continue };
            let k = k * s.iter().fold(0.0f64, |m, c| m.max(c.abs()));
            if !(k.is_finite() && k > 0.0) || best.get(&bi).is_some_and(|&x| x >= k) {
                continue;
            }
            best.insert(bi, k);
            stack.push((bi, k, depth + 1));
        }
    }
    best
}

/// Entities in paper space that would be converted in model space
/// (annotation and viewports aside).
fn paper_space_geometry(doc: &Document) -> usize {
    doc.paper_space_entities()
        .into_iter()
        .filter(|&i| {
            let kind = doc.objects[i].kind.as_str();
            !(STRUCTURAL.contains(&kind) || ANNOTATION.contains(&kind))
        })
        .count()
}

/// Quads covering a polyline band: points with their half widths, mitred
/// where the direction changes (`closed`: the last point joins the first),
/// counter-clockwise in the OCS plane.
fn band(run: &[(Vec3, f64)], closed: bool) -> Vec<Vec<Vec3>> {
    let n = run.len();
    let side = |a: Vec3, b: Vec3| {
        let d = normalize(sub(b, a));
        [-d[1], d[0], 0.0]
    };
    let segs: Vec<Vec3> = (0..n - 1).map(|i| side(run[i].0, run[i + 1].0)).collect();
    // offset direction at each point: the mitre of the segments meeting there
    let miter = |i: usize| -> Vec3 {
        let (before, after) = match (i, closed) {
            (0, false) => (segs[0], segs[0]),
            (0, true) => (side(run[n - 2].0, run[n - 1].0), segs[0]),
            (i, _) if i == n - 1 => (segs[n - 2], if closed { segs[0] } else { segs[n - 2] }),
            (i, _) => (segs[i - 1], segs[i]),
        };
        let m = normalize(add(before, after));
        let m = if m == ZERO { after } else { m };
        // longer at sharp corners, but bounded
        mul(m, 1.0 / dot(m, after).max(0.25))
    };
    (0..n - 1)
        .filter(|&i| run[i].0 != run[i + 1].0)
        .map(|i| {
            let (p, q) = (run[i], run[i + 1]);
            let (mp, mq) = (miter(i), miter(i + 1));
            vec![
                sub(p.0, mul(mp, p.1)),
                sub(q.0, mul(mq, q.1)),
                add(q.0, mul(mq, q.1)),
                add(p.0, mul(mp, p.1)),
            ]
        })
        .collect()
}

/// Drop points that repeat their predecessor (the first compares with the last).
fn dedup_ring(pts: &[Vec3]) -> Vec<Vec3> {
    let n = pts.len();
    (0..n)
        .filter(|&i| pts[i] != pts[(i + n - 1) % n])
        .map(|i| pts[i])
        .collect()
}

fn hex(rgb: Rgb) -> String {
    rgb.iter().map(|c| format!("{c:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mesh::affine_apply;

    /// The normal map's KHR_texture_transform turns the base color's glTF
    /// UVs into the ones the normal map's own archive transform gives.
    #[test]
    fn normal_map_transform_on_base_color_uvs() {
        let (s, c) = 30f64.to_radians().sin_cos();
        let m = [2.0 * c, -3.0 * s, 0.25, 2.0 * s, 3.0 * c, -0.5];
        let d = [0.5, 0.0, 0.1, 0.0, 0.5, 0.3];
        let k = normal_transform(m, d).unwrap();
        let (ks, kc) = k.rotation.sin_cos();
        for uv in [[0.0, 0.0], [1.0, 0.0], [0.3, 0.7], [-2.0, 5.0]] {
            let flipped = [uv[0], 1.0 - uv[1]];
            let base = affine_apply(gltf_uv_affine(m), flipped);
            let want = affine_apply(gltf_uv_affine(affine_mul(m, d)), flipped);
            let sc = [k.scale[0] * base[0], k.scale[1] * base[1]];
            let got = [
                kc * sc[0] + ks * sc[1] + k.offset[0],
                -ks * sc[0] + kc * sc[1] + k.offset[1],
            ];
            assert!((got[0] - want[0]).abs() < 1e-9 && (got[1] - want[1]).abs() < 1e-9);
        }
        assert!(normal_transform(m, [1.0, 0.0, 0.0, 0.0, 1.0, 0.0]).is_none());
    }
}
