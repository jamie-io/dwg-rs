// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Typed access to a decoded drawing: handles, layers, blocks, colors,
//! units and materials.

use std::cell::{OnceCell, RefCell};
use std::collections::{BTreeSet, HashMap, HashSet};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::LazyLock;

use serde_json::{Map, Value, json};
use zip::ZipArchive;

use crate::acis::acds::acis_streams;
use crate::acis::sab::AcisData;
use crate::dwg::{Color, Data, Drawing, Entity, Handle, Object, Shape, XValue};
use crate::gltf::TextureTransform;
use crate::mesh::Projection;
use crate::ofml;

pub type Rgb = [u8; 3];

/// A resolved entity or layer color.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColorRef {
    ByLayer,
    ByBlock,
    Rgb(Rgb),
}

/// INSUNITS -> meters (AutoCAD UnitsValue enumeration).
pub(crate) fn unit_to_meters(units: i32) -> f64 {
    match units {
        1 => 0.0254,
        2 => 0.3048,
        3 => 1609.344,
        4 => 0.001,
        5 => 0.01,
        6 => 1.0,
        7 => 1000.0,
        8 => 2.54e-8,
        9 => 2.54e-5,
        10 => 0.9144,
        11 => 1e-10,
        12 => 1e-9,
        13 => 1e-6,
        14 => 0.1,
        15 => 10.0,
        16 => 100.0,
        17 => 1e9,
        18 => 1.495978707e11,
        19 => 9.4607304725808e15,
        20 => 3.085_677_581_491_367e16,
        21 => 1200.0 / 3937.0,
        22 => 100.0 / 3937.0,
        23 => 3600.0 / 3937.0,
        24 => 6336000.0 / 3937.0,
        _ => 1.0, // 0 = unitless
    }
}

/// `--units` names -> INSUNITS.
pub const UNIT_NAMES: [(&str, i32); 9] = [
    ("cm", 5),
    ("dm", 14),
    ("ft", 2),
    ("in", 1),
    ("km", 7),
    ("m", 6),
    ("mi", 3),
    ("mm", 4),
    ("yd", 10),
];

/// AutoCAD Color Index -> RGB.
///
/// ACI 7 is the foreground color: white on a dark background, black on a
/// light one (paper, plots, renderings on white). glTF has no background, so
/// it is black, which is how OFML viewers show it; a true color white
/// (255,255,255) stays white.
pub static ACI: LazyLock<[Rgb; 256]> = LazyLock::new(|| {
    let mut table = [[0u8; 3]; 256];
    let fixed: [Rgb; 9] = [
        [255, 0, 0],
        [255, 255, 0],
        [0, 255, 0],
        [0, 255, 255],
        [0, 0, 255],
        [255, 0, 255],
        [0, 0, 0],
        [65, 65, 65],
        [128, 128, 128],
    ];
    table[1..10].copy_from_slice(&fixed);
    let levels = [255.0, 204.0, 153.0, 127.0, 76.0];
    for (i, entry) in table.iter_mut().enumerate().take(250).skip(10) {
        let hue = ((i - 10) / 10 * 15) as f64 / 360.0;
        let step = (i - 10) % 10;
        let level = levels[step / 2];
        let full = hsv_to_rgb(hue);
        // odd entries are the pastel variant: halfway towards the level
        let chan = if step % 2 == 1 {
            full.map(|c| c + (1.0 - c) * 0.5)
        } else {
            full
        };
        *entry = chan.map(|c| (c * level) as u8);
    }
    for (i, g) in (250..256).zip([51, 80, 105, 130, 190, 255]) {
        table[i] = [g, g, g];
    }
    table
});

/// colorsys.hsv_to_rgb(h, 1, 1)
fn hsv_to_rgb(h: f64) -> [f64; 3] {
    let i = (h * 6.0).trunc();
    let f = h * 6.0 - i;
    let (q, t) = (1.0 - f, f);
    match (i as i64).rem_euclid(6) {
        0 => [1.0, t, 0.0],
        1 => [q, 1.0, 0.0],
        2 => [0.0, 1.0, t],
        3 => [0.0, q, 1.0],
        4 => [t, 0.0, 1.0],
        _ => [1.0, 0.0, q],
    }
}

pub fn rgb_unit(rgb: Rgb) -> [f64; 3] {
    rgb.map(|c| c as f64 / 255.0)
}

/// ByLayer, ByBlock or an RGB color. Layers resolve ByLayer/ByBlock to ACI 7.
pub fn parse_color(color: Color, is_layer: bool) -> ColorRef {
    let seven = ColorRef::Rgb(ACI[7]);
    if let Some(value) = color.rgb {
        let low = (value & 255) as usize;
        match value >> 24 {
            0xC2 => return ColorRef::Rgb([(value >> 16) as u8, (value >> 8) as u8, value as u8]),
            0xC3 => return ColorRef::Rgb(if low > 0 { ACI[low] } else { ACI[7] }),
            0xC0 if !is_layer => return ColorRef::ByLayer,
            0xC1 if !is_layer => return ColorRef::ByBlock,
            _ => {}
        }
    }
    let Some(index) = color.index else {
        // stored as ByBlock (DXF 62 = 0)
        return if is_layer { seven } else { ColorRef::ByBlock };
    };
    match index.unsigned_abs() {
        256 if !is_layer => ColorRef::ByLayer,
        0 if !is_layer => ColorRef::ByBlock,
        i @ 1..=255 => ColorRef::Rgb(ACI[i as usize]),
        _ => seven,
    }
}

pub struct Layer {
    pub name: String,
    pub color: Rgb,
    pub material: Option<Handle>,
    /// Neither off nor frozen.
    pub visible: bool,
    /// Frozen hides block references on the layer with all their content;
    /// off hides only what is drawn on it.
    pub frozen: bool,
}

/// What the DWG says about one MATERIAL object, merged with its embedded
/// OFML material archive when present.
#[derive(Debug, Clone)]
pub struct MaterialDef {
    pub name: String,
    pub diffuse: Option<Rgb>,
    /// Alpha 0..1: `1 - tra` of the archive when it has `tra`, else the DWG
    /// MATERIAL's opacity.
    pub opacity: f64,
    pub metallic: f64,
    pub roughness: f64,
    pub texture: Option<(Vec<u8>, &'static str)>,
    /// 2x3 affine applied to texture coordinates: the DWG
    /// diffusemap.transmatrix, as is. For OFML materials it already holds the
    /// OFML `scale`, `offset` and `rotate` of the archive (M = S * T * R(rotate),
    /// or S * R * T when both offset and rotate are set; rotate in degrees),
    /// so the archive's values are not applied again.
    pub uv_matrix: [f64; 6],
    /// How to generate UVs for faces without stored ones.
    pub projection: Projection,
    /// Whether the material comes with an OFML material archive, whose
    /// projection is in meters; a plain AutoCAD material's transmatrix is in
    /// drawing units.
    pub ofml: bool,
    /// The OMATS block mapping parameters (`auto <triple> [<token>]`), from
    /// the archive or the modifiers in the material name (`...; auto xzx yz;
    /// rotate 0 0 90`), which win. Not used by the converter yet.
    pub auto_mapping: Option<AutoMapping>,
    /// The archive's normal map (`bumps`), if any.
    pub normal: Option<NormalMap>,
    pub extras: Map<String, Value>,
}

/// OMATS 2.2 §3.2 block mapping: `auto <triple> [<token>]`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AutoMapping {
    /// Direction of the U vector on the front, right and top sides of the
    /// block (`b'x'`, `b'y'` or `b'z'`); back, left and bottom are analogous.
    pub u_axes: [u8; 3],
    /// The word after the triple as these files have it (`yz`, `z`, `xz`; the
    /// mirror/inversion part of the mapping, not defined in OMATS 2.2).
    pub mirror: Option<String>,
}

impl AutoMapping {
    /// From the arguments after `auto`.
    pub fn parse(args: &[&str]) -> Option<Self> {
        let triple = args.first()?.to_ascii_lowercase();
        let u_axes: [u8; 3] = triple.as_bytes().try_into().ok()?;
        if !u_axes.iter().all(|c| matches!(c, b'x' | b'y' | b'z')) {
            return None;
        }
        Some(AutoMapping {
            u_axes,
            mirror: args.get(1).map(|w| w.to_string()),
        })
    }
}

/// A tangent-space normal map: OMATS `bumps <ext> <stem>`. The image is
/// passed through as the package holds it.
#[derive(Debug, Clone)]
pub struct NormalMap {
    pub data: Vec<u8>,
    pub mime: &'static str,
    /// The normal map's own UV transform relative to the base color's, on
    /// the UVs before `uv_matrix` (2x3 like it): `B⁻¹ · N`, where B and N
    /// are the archive's transforms built from `scale`/`offset`/`rotate` and
    /// from `nscale`/`noffset`/`nrotate` (each falling back to the former).
    /// Identity unless the archive has `n*` keys that differ.
    pub uv_delta: [f64; 6],
    /// N itself: the normal map's transform when there is no base color
    /// texture (the archive's transform is folded into the DWG's diffuse
    /// map only, so a bump-only material has an identity transmatrix).
    pub archive_uv: [f64; 6],
}

pub const UV_IDENTITY: [f64; 6] = [1.0, 0.0, 0.0, 0.0, 1.0, 0.0];

impl MaterialDef {
    fn new(name: String) -> Self {
        MaterialDef {
            name,
            diffuse: None,
            opacity: 1.0,
            metallic: 0.0,
            roughness: 0.7,
            texture: None,
            uv_matrix: UV_IDENTITY,
            projection: Projection::Box,
            ofml: false,
            auto_mapping: None,
            normal: None,
            extras: Map::new(),
        }
    }

    fn set_texture(&mut self, data: Vec<u8>, name: &str) {
        if let Some(mime) = image_mime(name) {
            self.texture = Some((data, mime));
        }
    }

    /// Whether faces with this material need texture coordinates (a base
    /// color texture or a normal map).
    pub fn has_maps(&self) -> bool {
        self.texture.is_some() || self.normal.is_some()
    }

    /// The glTF `KHR_texture_transform` of the normal map, for texture
    /// coordinates built with `uv_matrix` (and then `v = 1 − v`); None when
    /// it uses them as they are, or when the transform has a shear that
    /// `KHR_texture_transform` cannot express.
    pub fn normal_transform(&self) -> Option<TextureTransform> {
        let normal = self.normal.as_ref()?;
        if uv_close(normal.uv_delta, UV_IDENTITY) {
            return None;
        }
        // glTF uv = F(M uv0) with F(u, v) = (u, 1 - v); the normal map wants
        // F(M D uv0) = (F M D M⁻¹ F) glTF uv
        const FLIP: [f64; 6] = [1.0, 0.0, 0.0, 0.0, -1.0, 1.0];
        let m = self.uv_matrix;
        let k = uv_mul(
            FLIP,
            uv_mul(m, uv_mul(normal.uv_delta, uv_mul(uv_inverse(m)?, FLIP))),
        );
        TextureTransform::from_affine(k)
    }
}

/// A normal map without a base color texture: the transmatrix is mostly
/// left at identity then (there is no diffuse map to carry the transform),
/// so the UVs take the archive's transform directly. A transmatrix that is
/// set (the exporter folded the archive's transform after all, or another
/// tool wrote it) stays, with the normal map relative to it as usual.
fn normal_map_alone(mat: &mut MaterialDef) {
    if mat.texture.is_none() && uv_close(mat.uv_matrix, UV_IDENTITY) {
        if let Some(normal) = mat.normal.as_mut() {
            mat.uv_matrix = normal.archive_uv;
            normal.uv_delta = UV_IDENTITY;
        }
    }
}

/// The MIME type of a texture file glTF can hold, by its name.
fn image_mime(name: &str) -> Option<&'static str> {
    let ext = Path::new(name)
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "png" => Some("image/png"),
        "jpg" | "jpeg" => Some("image/jpeg"),
        _ => None,
    }
}

/// a · b for 2x3 affine maps (row-major [a b c; d e f]).
fn uv_mul(a: [f64; 6], b: [f64; 6]) -> [f64; 6] {
    [
        a[0] * b[0] + a[1] * b[3],
        a[0] * b[1] + a[1] * b[4],
        a[0] * b[2] + a[1] * b[5] + a[2],
        a[3] * b[0] + a[4] * b[3],
        a[3] * b[1] + a[4] * b[4],
        a[3] * b[2] + a[4] * b[5] + a[5],
    ]
}

fn uv_inverse(m: [f64; 6]) -> Option<[f64; 6]> {
    let det = m[0] * m[4] - m[1] * m[3];
    if det.abs() < 1e-12 || !det.is_finite() {
        return None;
    }
    let (a, b, d, e) = (m[4] / det, -m[1] / det, -m[3] / det, m[0] / det);
    Some([a, b, -(a * m[2] + b * m[5]), d, e, -(d * m[2] + e * m[5])])
}

fn uv_close(a: [f64; 6], b: [f64; 6]) -> bool {
    a.iter().zip(&b).all(|(x, y)| (x - y).abs() <= 1e-9)
}

/// OFML texture transform S · T · R(θ) (OMATS 2.2 §3: rotate, then offset,
/// then scale; θ in degrees, counter-clockwise with v up). The DWG
/// transmatrix agrees except when both offset and rotate are set: there it
/// is S · R(θ) · T. Only the normal map uses this, relative to the same
/// build of the base transform, so the order matters only for a normal map
/// without base color that has both (rare).
fn ofml_uv_matrix(scale: [f64; 2], offset: [f64; 2], degrees: f64) -> [f64; 6] {
    let (s, c) = degrees.to_radians().sin_cos();
    let [sx, sy] = scale;
    [
        sx * c,
        -sx * s,
        sx * offset[0],
        sy * s,
        sy * c,
        sy * offset[1],
    ]
}

/// Finds the image file a DWG MATERIAL names (as stored, often an absolute
/// path on the author's machine): its bytes, or None.
pub type TextureFiles = Box<dyn Fn(&str) -> Option<Vec<u8>>>;

/// The file as stored, else the file of that name next to `source` (the
/// drawing).
pub fn texture_files_near(source: Option<&Path>) -> TextureFiles {
    let dir = source.and_then(Path::parent).map(Path::to_path_buf);
    Box::new(move |filename| {
        let mut paths = vec![PathBuf::from(filename)];
        if let Some(dir) = &dir {
            paths.push(dir.join(basename(filename)));
        }
        paths
            .into_iter()
            .filter(|p| p.is_file())
            .find_map(|p| std::fs::read(p).ok())
    })
}

pub struct Document {
    pub objects: Vec<Object>,
    texture_files: TextureFiles,
    insunits: i32,
    by_handle: HashMap<Handle, usize>,
    owned: OnceCell<HashMap<Handle, Vec<usize>>>,
    /// Entities by their owner handle, in file order.
    by_owner: OnceCell<HashMap<Handle, Vec<usize>>>,
    pub layers: HashMap<Handle, Layer>,
    pub model_space: Option<usize>,
    /// LibreDWG's recoverable decode errors (see [`Drawing::decode_error`]).
    pub decode_error: i32,
    materials: RefCell<HashMap<Handle, Option<Rc<MaterialDef>>>>,
    /// Embedded OFML material archives (zip files) by material name.
    archives: HashMap<String, Vec<u8>>,
    /// Embedded texture images by lower-case file name.
    textures: HashMap<String, Vec<u8>>,
    acds: HashMap<Handle, Vec<u8>>,
}

impl Document {
    /// `source` is the drawing's path: material images are looked for next
    /// to it (see [`texture_files_near`]).
    pub fn new(drawing: Drawing, source: Option<&Path>) -> Self {
        Self::with_texture_files(drawing, texture_files_near(source))
    }

    /// A document whose material images come from `texture_files`.
    pub fn with_texture_files(drawing: Drawing, texture_files: TextureFiles) -> Self {
        let Drawing {
            insunits,
            objects,
            acds,
            decode_error,
        } = drawing;
        let by_handle = objects
            .iter()
            .enumerate()
            .filter(|(_, o)| o.handle != 0)
            .map(|(i, o)| (o.handle, i))
            .collect();
        let mut doc = Document {
            objects,
            texture_files,
            insunits,
            by_handle,
            owned: OnceCell::new(),
            by_owner: OnceCell::new(),
            layers: HashMap::new(),
            model_space: None,
            decode_error,
            materials: RefCell::default(),
            archives: HashMap::new(),
            textures: HashMap::new(),
            acds: acis_streams(&acds),
        };
        doc.layers = doc.read_layers();
        doc.model_space = doc.objects.iter().position(
            |o| matches!(&o.data, Data::BlockHeader(b) if b.name.to_uppercase() == "*MODEL_SPACE"),
        );
        doc.archives = doc
            .read_edwg_dict(ofml::MATERIAL_DICT)
            .into_iter()
            .filter(|(_, blob)| {
                blob.starts_with(b"PK") && ZipArchive::new(Cursor::new(blob.as_slice())).is_ok()
            })
            .collect();
        doc.textures = doc
            .read_edwg_dict(ofml::TEXTURE_DICT)
            .into_iter()
            .filter(|(name, _)| image_mime(name).is_some())
            .map(|(name, blob)| (basename(&name).to_lowercase(), blob))
            .collect();
        doc
    }

    // -- handles ---------------------------------------------------------
    pub fn get(&self, handle: Handle) -> Option<&Object> {
        if handle == 0 {
            return None;
        }
        self.by_handle.get(&handle).map(|&i| &self.objects[i])
    }

    /// The position of an object in [`Document::objects`].
    pub fn index(&self, handle: Handle) -> Option<usize> {
        if handle == 0 {
            None
        } else {
            self.by_handle.get(&handle).copied()
        }
    }

    // -- units -------------------------------------------------------------
    /// The drawing's INSUNITS (0: unitless).
    pub fn insunits(&self) -> i32 {
        self.insunits
    }

    pub fn unit_scale(&self, units: Option<i32>) -> f64 {
        unit_to_meters(units.unwrap_or(self.insunits))
    }

    // -- blocks ------------------------------------------------------------
    /// The entities of a BLOCK_HEADER, as object indices.
    pub fn block_entities(&self, block: usize) -> Vec<usize> {
        match &self.objects[block].data {
            Data::BlockHeader(b) if b.entities.is_some() => b
                .entities
                .as_ref()
                .unwrap()
                .iter()
                .filter_map(|&h| self.index(h))
                .filter(|&i| matches!(self.objects[i].data, Data::Entity(_)))
                .collect(),
            _ => self
                .owned_by()
                .get(&self.objects[block].handle)
                .cloned()
                .unwrap_or_default(),
        }
    }

    /// Whether a block holds anything besides its BLOCK/ENDBLK markers.
    pub fn block_has_content(&self, block: usize) -> bool {
        self.block_entities(block)
            .into_iter()
            .any(|i| !matches!(self.objects[i].kind.as_str(), "BLOCK" | "ENDBLK" | "SEQEND"))
    }

    /// A series DWG (block library): nothing in model space, and named
    /// blocks that nothing inserts.
    pub fn is_series(&self) -> bool {
        !self
            .model_space
            .is_some_and(|ms| self.block_has_content(ms))
            && !self.series_blocks().is_empty()
    }

    /// The articles of a series DWG: named (not anonymous `*...`) blocks
    /// with content that no INSERT uses, in file order.
    pub fn series_blocks(&self) -> Vec<usize> {
        let inserted: HashSet<Handle> = self
            .objects
            .iter()
            .filter_map(|o| match &o.data {
                Data::Entity(e) => match &e.shape {
                    Shape::Insert { block, .. } => Some(*block),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        self.objects
            .iter()
            .enumerate()
            .filter(|(i, o)| match &o.data {
                Data::BlockHeader(b) => {
                    !b.name.starts_with('*')
                        && !inserted.contains(&o.handle)
                        && self.block_has_content(*i)
                }
                _ => false,
            })
            .map(|(i, _)| i)
            .collect()
    }

    /// The entities of all paper space layouts (`*Paper_Space*` blocks,
    /// and in pre-R2004 files the entities marked as paper space).
    pub fn paper_space_entities(&self) -> Vec<usize> {
        let mut found: BTreeSet<usize> = self
            .objects
            .iter()
            .enumerate()
            .filter(|(_, o)| matches!(&o.data, Data::Entity(e) if e.entmode == 1))
            .map(|(i, _)| i)
            .collect();
        for (i, o) in self.objects.iter().enumerate() {
            if let Data::BlockHeader(b) = &o.data {
                if b.name.to_uppercase().starts_with("*PAPER_SPACE") {
                    found.extend(self.block_entities(i));
                }
            }
        }
        found.into_iter().collect()
    }

    /// Fallback for files whose block headers carry no entity list.
    fn owned_by(&self) -> &HashMap<Handle, Vec<usize>> {
        self.owned.get_or_init(|| {
            let model = self.model_space.map(|i| self.objects[i].handle);
            let mut owned: HashMap<Handle, Vec<usize>> = HashMap::new();
            for (i, obj) in self.objects.iter().enumerate() {
                let Data::Entity(ent) = &obj.data else {
                    continue;
                };
                let owner = match ent.entmode {
                    2 => model,
                    0 => Some(obj.owner),
                    _ => None,
                };
                if let Some(owner) = owner {
                    owned.entry(owner).or_default().push(i);
                }
            }
            owned
        })
    }

    /// Vertices of a POLYLINE_*: explicit refs, else owner, else handle range.
    pub fn sub_entities(&self, parent: usize, kind_prefix: &str) -> Vec<usize> {
        let p = &self.objects[parent];
        let Data::Entity(ent) = &p.data else {
            return vec![];
        };
        let Shape::Polyline {
            vertices,
            first,
            last,
            ..
        } = &ent.shape
        else {
            return vec![];
        };
        if !vertices.is_empty() {
            return vertices.iter().filter_map(|&h| self.index(h)).collect();
        }
        let is_kind =
            |o: &Object| matches!(o.data, Data::Entity(_)) && o.kind.starts_with(kind_prefix);
        let found: Vec<usize> = self
            .owned_entities()
            .get(&p.handle)
            .into_iter()
            .flatten()
            .copied()
            .filter(|&i| is_kind(&self.objects[i]))
            .collect();
        if !found.is_empty() {
            return found;
        }
        let (Some(first), Some(last)) = (self.index(*first), self.index(*last)) else {
            return vec![];
        };
        (first..=last)
            .filter(|&i| is_kind(&self.objects[i]))
            .collect()
    }

    /// The entities of each owner, built once: looking them up per polyline
    /// would scan the whole drawing for every one.
    fn owned_entities(&self) -> &HashMap<Handle, Vec<usize>> {
        self.by_owner.get_or_init(|| {
            let mut by_owner: HashMap<Handle, Vec<usize>> = HashMap::new();
            for (i, obj) in self.objects.iter().enumerate() {
                if matches!(obj.data, Data::Entity(_)) {
                    by_owner.entry(obj.owner).or_default().push(i);
                }
            }
            by_owner
        })
    }

    // -- layers ------------------------------------------------------------
    fn read_layers(&self) -> HashMap<Handle, Layer> {
        let mut layers = HashMap::new();
        for obj in &self.objects {
            if let Data::Layer(l) = &obj.data {
                let ColorRef::Rgb(color) = parse_color(l.color, true) else {
                    unreachable!("layer colors are RGB")
                };
                layers.insert(
                    obj.handle,
                    Layer {
                        name: l.name.clone(),
                        color,
                        material: (l.material != 0).then_some(l.material),
                        visible: !(l.off || l.frozen),
                        frozen: l.frozen,
                    },
                );
            }
        }
        layers
    }

    pub fn is_layer_zero(&self, handle: Handle) -> bool {
        self.layers.get(&handle).is_some_and(|l| l.name == "0")
    }

    // -- materials ---------------------------------------------------------
    /// (flags, MATERIAL handle) of an entity; flags 0 ByLayer, 1 ByBlock, 3 explicit.
    pub fn material_ref(&self, ent: &Entity) -> (u8, Option<Handle>) {
        for &h in &ent.material_refs {
            if let Some(Object {
                data: Data::Material(m),
                handle,
                ..
            }) = self.get(h)
            {
                return match m.name.as_str() {
                    "ByLayer" => (0, None),
                    "ByBlock" => (1, None),
                    _ => (3, Some(*handle)),
                };
            }
        }
        (
            if ent.material_flags <= 1 {
                ent.material_flags
            } else {
                0
            },
            None,
        )
    }

    /// The XRECORDs (310 chunks) of the named object dictionary's entry `key`
    /// (under PXCAD_DICT), by name. OFML exports store each material as a zip
    /// (material.mat + textures) under EDWG_MATERIAL_DICT, keyed by the
    /// MATERIAL object's name; some store the images on their own under
    /// EDWG_TEXTURE_DICT, keyed by the file name the MATERIAL refers to.
    fn read_edwg_dict(&self, key: &str) -> Vec<(String, Vec<u8>)> {
        let mut blobs = vec![];
        for obj in &self.objects {
            let Data::Dictionary(items) = &obj.data else {
                continue;
            };
            if obj.kind != "DICTIONARY" {
                continue;
            }
            let Some(dict) = lookup(items, key).and_then(|h| self.get(h)) else {
                continue;
            };
            let Data::Dictionary(entries) = &dict.data else {
                continue;
            };
            for (name, h) in entries {
                let blob = self.get(*h).map(binary_chunks).unwrap_or_default();
                if !blob.is_empty() {
                    blobs.push((name.clone(), blob));
                }
            }
        }
        blobs
    }

    pub fn material(&self, handle: Option<Handle>) -> Option<Rc<MaterialDef>> {
        let handle = handle.filter(|&h| h != 0)?;
        if let Some(m) = self.materials.borrow().get(&handle) {
            return m.clone();
        }
        let result = match self.get(handle) {
            Some(
                obj @ Object {
                    data: Data::Material(m),
                    ..
                },
            ) if !matches!(m.name.as_str(), "ByLayer" | "ByBlock" | "Global") => {
                Some(Rc::new(self.material_from(obj)))
            }
            _ => None,
        };
        self.materials.borrow_mut().insert(handle, result.clone());
        result
    }

    fn material_from(&self, obj: &Object) -> MaterialDef {
        let Data::Material(m) = &obj.data else {
            unreachable!()
        };
        let mut mat = MaterialDef::new(m.name.clone());
        if let Some(v) = m.diffuse {
            mat.diffuse = Some([(v >> 16) as u8, (v >> 8) as u8, v as u8]);
        }
        mat.opacity = m.opacity.clamp(0.0, 1.0);
        if let Some(t) = m.transmatrix {
            mat.uv_matrix = [t[0], t[1], t[3], t[4], t[5], t[7]];
        }
        // there is one archive per base material; derived materials (same
        // look, other mapping) point to their base via EDWG_BASE_MATERIAL.
        let mut package = self.archives.get(&m.name);
        if package.is_none() {
            let base = self.xrecord(obj, ofml::BASE_MATERIAL);
            if let Some(Data::XRecord(items)) = base.map(|o| &o.data) {
                for (code, value) in items {
                    let XValue::Handle(h) = value else { continue };
                    if !matches!(code, 330 | 340 | 350 | 360) {
                        continue;
                    }
                    if let Some(Data::Material(target)) = self.get(*h).map(|o| &o.data) {
                        if let Some(p) = self.archives.get(&target.name) {
                            package = Some(p);
                            break;
                        }
                    }
                }
            }
        }
        if let Some(package) = package {
            apply_archive(&mut mat, package, &m.filename);
            mat.ofml = true;
        }
        if mat.texture.is_none() && !m.filename.is_empty() {
            self.load_texture_file(&mut mat, &m.filename);
        }
        normal_map_alone(&mut mat);
        // material modifiers in the name: "<base>; auto xzx yz; rotate 0 0 90"
        for modifier in m.name.split(';').skip(1) {
            let words: Vec<&str> = modifier.split_whitespace().collect();
            if let Some((&"auto", args)) = words.split_first() {
                if let Some(auto) = AutoMapping::parse(args) {
                    mat.auto_mapping = Some(auto);
                }
            }
        }
        mat
    }

    fn load_texture_file(&self, mat: &mut MaterialDef, filename: &str) {
        let name = basename(filename);
        if image_mime(&name).is_none() {
            return;
        }
        if let Some(data) = self.textures.get(&name.to_lowercase()) {
            mat.set_texture(data.clone(), &name);
        } else if let Some(data) = (self.texture_files)(filename) {
            mat.set_texture(data, &name);
        }
    }

    // -- ACIS ----------------------------------------------------------------
    /// SAB bytes or SAT text of a 3DSOLID/BODY/REGION/SURFACE, if present.
    pub fn acis_data(&self, obj: &Object) -> Option<AcisData> {
        let Data::Entity(ent) = &obj.data else {
            return None;
        };
        if ent.has_ds_data {
            // R2013+: data-storage section (LibreDWG's inline copy can be garbled)
            if let Some(stored) = self.acds.get(&obj.handle).filter(|s| !s.is_empty()) {
                return Some(AcisData::Sab(stored.clone()));
            }
        }
        match &ent.shape {
            Shape::Acis {
                data: Some(AcisData::Sab(b)),
                ..
            } if b.starts_with(b"ACIS BinaryFile") => Some(AcisData::Sab(b.clone())),
            Shape::Acis {
                data: Some(AcisData::Sat(text)),
                ..
            } => {
                // SAT text starts with its version number
                let first = text.split(['\r', '\n']).next().unwrap_or("");
                let head: String = first.chars().take(4).collect();
                let head = head.trim();
                (!head.is_empty() && head.chars().all(|c| c.is_ascii_digit()))
                    .then(|| AcisData::Sat(text.clone()))
            }
            _ => None,
        }
    }

    // -- extension dictionaries -------------------------------------------
    pub fn xrecord(&self, obj: &Object, key: &str) -> Option<&Object> {
        let Data::Dictionary(items) = &self.get(obj.xdic)?.data else {
            return None;
        };
        self.get(lookup(items, key)?)
    }
}

/// A dictionary entry (the last one, as for JSON objects with duplicate keys).
fn lookup(items: &[(String, Handle)], key: &str) -> Option<Handle> {
    items.iter().rev().find(|(k, _)| k == key).map(|&(_, h)| h)
}

/// The file name of a DWG path (`\` or `/` separated).
pub fn basename(path: &str) -> String {
    path.replace('\\', "/")
        .rsplit('/')
        .next()
        .unwrap_or("")
        .to_string()
}

/// Concatenate an XRECORD's binary (group code 310) chunks.
pub fn binary_chunks(record: &Object) -> Vec<u8> {
    let Data::XRecord(items) = &record.data else {
        return vec![];
    };
    items
        .iter()
        .filter_map(|(code, v)| match v {
            XValue::Binary(b) if *code == 310 => Some(b.as_slice()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .concat()
}

fn apply_archive(mat: &mut MaterialDef, package: &[u8], filename: &str) {
    let Ok(mut zip) = ZipArchive::new(Cursor::new(package)) else {
        return;
    };
    let names: Vec<String> = zip.file_names().map(String::from).collect();
    // an entry by name; ignoring case when there is no exact match
    let mut read = |name: &str| -> Option<Vec<u8>> {
        let entry = if names.iter().any(|n| n == name) {
            name
        } else {
            names.iter().find(|n| n.eq_ignore_ascii_case(name))?
        };
        let mut file = zip.by_name(entry).ok()?;
        let mut data = vec![];
        file.read_to_end(&mut data).ok()?;
        Some(data)
    };
    // The spec is material.mat, or in packages made by other tools the one
    // `<stem>.mat` (diffuse.mat, chrom.mat, silva_70_blue.mat, ...).
    let spec_name = if names.iter().any(|n| n == "material.mat") {
        Some("material.mat")
    } else {
        names
            .iter()
            .filter(|n| n.to_ascii_lowercase().ends_with(".mat"))
            .min()
            .map(String::as_str)
    };
    let spec = spec_name
        .and_then(&mut read)
        .map(|d| String::from_utf8_lossy(&d).into_owned())
        .unwrap_or_default();
    let mut tex_name = None;
    let mut bumps = None;
    // OFML texture transforms by key: scale, offset, rotate and the normal
    // map's nscale, noffset, nrotate (each a triple)
    let mut transforms: HashMap<&str, [f64; 3]> = HashMap::new();
    for line in spec.lines() {
        let parts: Vec<&str> = line.split_whitespace().collect();
        let Some((&key, args)) = parts.split_first() else {
            continue;
        };
        let num = |i: usize| args.get(i).and_then(|a| a.parse::<f64>().ok());
        match key {
            "scale" | "offset" | "rotate" | "nscale" | "noffset" | "nrotate" => {
                if let (Some(a), Some(b), Some(c)) = (num(0), num(1), num(2)) {
                    if [a, b, c].iter().all(|v| v.is_finite()) {
                        transforms.insert(key, [a, b, c]);
                    }
                }
            }
            "bumps" if args.len() >= 2 => bumps = Some(format!("{}.{}", args[1], args[0])),
            "dif" if args.len() >= 3 => {
                if let (Some(r), Some(g), Some(b)) = (num(0), num(1), num(2)) {
                    mat.diffuse = Some([r, g, b].map(|c| (c.clamp(0.0, 1.0) * 255.0 + 0.5) as u8));
                }
            }
            "metallic" => mat.metallic = num(0).unwrap_or(mat.metallic),
            "roughness" => mat.roughness = num(0).unwrap_or(mat.roughness),
            // OMATS `tra S`: transparency 0..1, alpha = 1 - S. It wins over
            // the DWG opacity, which is left at 100 % for many glass
            // materials
            "tra" => {
                if let Some(t) = num(0).filter(|t| t.is_finite()) {
                    mat.opacity = 1.0 - t.clamp(0.0, 1.0);
                }
            }
            "tex" if args.len() >= 3 && args[0] == "image" => {
                tex_name = Some(format!("{}.{}", args[2], args[1]))
            }
            "auto" => {
                mat.projection = Projection::Box;
                mat.auto_mapping = AutoMapping::parse(args).or(mat.auto_mapping.take());
            }
            "prjx" => mat.projection = Projection::Axis(0),
            "prjy" => mat.projection = Projection::Axis(1),
            "prjz" => mat.projection = Projection::Axis(2),
            _ => {}
        }
    }
    // scale/offset/rotate: the exporter has already folded them into
    // diffusemap.transmatrix (M = S * T * R), which `uv_matrix` holds;
    // applying them again would rotate twice. They only serve to place the
    // normal map relative to the base color.
    if let Some(name) = bumps {
        let ofml = |prefix: &str| {
            let get = |key: &str, default: [f64; 3]| {
                transforms
                    .get(format!("{prefix}{key}").as_str())
                    .or_else(|| transforms.get(key))
                    .copied()
                    .unwrap_or(default)
            };
            let (s, o, r) = (
                get("scale", [1.0; 3]),
                get("offset", [0.0; 3]),
                get("rotate", [0.0; 3]),
            );
            ofml_uv_matrix([s[0], s[1]], [o[0], o[1]], r[2])
        };
        let (base, normal) = (ofml(""), ofml("n"));
        let mime = image_mime(&name);
        if let (Some(data), Some(mime), Some(inverse)) = (read(&name), mime, uv_inverse(base)) {
            mat.normal = Some(NormalMap {
                data,
                mime,
                uv_delta: uv_mul(inverse, normal),
                archive_uv: normal,
            });
        }
    }
    if let Some(id) = read("id.txt") {
        mat.extras.insert(
            "ofml_material".into(),
            json!(String::from_utf8_lossy(&id).trim()),
        );
    }
    let file_name = (!filename.is_empty()).then(|| basename(filename));
    // the exported image is named `<material name>_<image in the package>`
    let unprefixed = file_name.as_deref().and_then(|f| {
        f.strip_prefix(mat.name.as_str())
            .and_then(|rest| rest.strip_prefix('_'))
            .map(String::from)
    });
    for cand in [tex_name, file_name, unprefixed].into_iter().flatten() {
        if let Some(data) = read(&cand) {
            mat.set_texture(data, &cand);
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A material archive (zip) with the given files.
    fn package(files: &[(&str, &[u8])]) -> Vec<u8> {
        use std::io::Write;
        let mut zip = zip::ZipWriter::new(Cursor::new(Vec::new()));
        for (name, data) in files {
            zip.start_file(*name, zip::write::SimpleFileOptions::default())
                .unwrap();
            zip.write_all(data).unwrap();
        }
        zip.finish().unwrap().into_inner()
    }

    fn close(a: [f64; 6], b: [f64; 6]) -> bool {
        a.iter().zip(&b).all(|(x, y)| (x - y).abs() < 1e-12)
    }

    #[test]
    fn archive_rotate_is_not_folded_twice() {
        // scale 22 22, rotate 90 -> transmatrix S * R(90)
        let tm = [0.0, -22.0, 0.0, 22.0, 0.0, 0.0];
        let spec = b"type common\ntex image jpg wood\nscale 22.0000 22.0000 1.0000\nrotate 0.0000 0.0000 90.0000\nimport\n";
        let zip = package(&[("material.mat", spec), ("wood.jpg", b"jpg")]);
        let mut mat = MaterialDef::new("m".into());
        mat.uv_matrix = tm;
        apply_archive(&mut mat, &zip, "");
        assert!(close(mat.uv_matrix, tm), "{:?}", mat.uv_matrix);
        assert!(mat.texture.is_some());
    }

    #[test]
    fn derived_material_keeps_its_own_transmatrix() {
        // a derived material ("...; rotate 0 0 90") uses the base's archive,
        // whose rotate must not land on top of the derived transmatrix
        let base = b"type common\ntex image jpg wood\nrotate 0.0000 0.0000 180.0000\nimport\n";
        let zip = package(&[("material.mat", base), ("wood.jpg", b"jpg")]);
        let tm = [0.0, -1.0, 0.0, 1.0, 0.0, 0.0];
        let mut mat = MaterialDef::new("base.wood; rotate 0 0 90".into());
        mat.uv_matrix = tm;
        apply_archive(&mut mat, &zip, "");
        assert!(close(mat.uv_matrix, tm), "{:?}", mat.uv_matrix);
    }

    #[test]
    fn archive_spec_with_another_name() {
        let spec = b"dif 0.5 0.5 0.5\r\ntex image jpg silva_70_blue\r\nscale 9 9 9\r\n";
        let zip = package(&[
            ("id.txt", b"x"),
            ("preview.jpeg", b"preview"),
            ("silva_70_blue.jpg", b"blue"),
            ("silva_70_blue.mat", spec),
        ]);
        let mut mat = MaterialDef::new("acad65899f31".into());
        apply_archive(&mut mat, &zip, "");
        assert_eq!(mat.diffuse, Some([128, 128, 128]));
        assert_eq!(
            mat.texture.as_ref().map(|t| t.0.as_slice()),
            Some(&b"blue"[..])
        );
    }

    #[test]
    fn archive_image_from_the_material_filename() {
        // no spec names the image; the MATERIAL's file name is
        // `<material>_<image>`
        let zip = package(&[("chrom.mat", b"type metal\n"), ("Beech.JPG", b"beech")]);
        let mut mat = MaterialDef::new("acad141ec6fd".into());
        apply_archive(&mut mat, &zip, "C:\\tmp\\acad141ec6fd_beech.jpg");
        assert_eq!(
            mat.texture.as_ref().map(|t| t.0.as_slice()),
            Some(&b"beech"[..])
        );
    }

    #[test]
    fn archive_tra_is_transparency() {
        let zip = package(&[("material.mat", b"type glass\ntra 0.87\nrefraction 1.5\n")]);
        let mut mat = MaterialDef::new("plexiglass".into());
        mat.opacity = 1.0; // the DWG MATERIAL says opaque
        apply_archive(&mut mat, &zip, "");
        assert!((mat.opacity - 0.13).abs() < 1e-12, "{}", mat.opacity);
        // without `tra` the DWG opacity stays
        let zip = package(&[("material.mat", b"type common\ntransparency 0.5\n")]);
        let mut mat = MaterialDef::new("m".into());
        mat.opacity = 0.4;
        apply_archive(&mut mat, &zip, "");
        assert_eq!(mat.opacity, 0.4);
    }

    #[test]
    fn archive_bumps_follow_the_base_color() {
        // no n* keys: the normal map uses the base color's UVs as they are
        let spec = b"tex image jpg wood\nbumps png wood_nrm\nscale 2 2 1\nrotate 0 0 90\nimport\n";
        let zip = package(&[
            ("material.mat", spec),
            ("wood.jpg", b"jpg"),
            ("wood_nrm.png", b"png"),
        ]);
        let mut mat = MaterialDef::new("m".into());
        mat.uv_matrix = [0.0, -2.0, 0.0, 2.0, 0.0, 0.0]; // S * R(90)
        apply_archive(&mut mat, &zip, "");
        normal_map_alone(&mut mat);
        let normal = mat.normal.as_ref().expect("normal map");
        assert_eq!(
            (normal.data.as_slice(), normal.mime),
            (&b"png"[..], "image/png")
        );
        assert!(close(normal.uv_delta, UV_IDENTITY), "{:?}", normal.uv_delta);
        assert_eq!(mat.normal_transform(), None);
        assert!(mat.has_maps());
    }

    #[test]
    fn archive_nscale_gives_the_normal_map_its_own_transform() {
        let spec = b"tex image jpg wood\nbumps jpg nrm\nscale 2 2 1\nnscale 8 4 1\nimport\n";
        let zip = package(&[
            ("material.mat", spec),
            ("wood.jpg", b"a"),
            ("nrm.jpg", b"b"),
        ]);
        let mut mat = MaterialDef::new("m".into());
        mat.uv_matrix = [2.0, 0.0, 0.0, 0.0, 2.0, 0.0];
        apply_archive(&mut mat, &zip, "");
        assert!(close(
            mat.normal.as_ref().unwrap().uv_delta,
            [4.0, 0.0, 0.0, 0.0, 2.0, 0.0]
        ));
        // on glTF UVs (built with uv_matrix, v flipped): scale 4 x 2, and
        // the flip about v = 1 moves the origin
        let t = mat.normal_transform().expect("a transform");
        assert!((t.scale[0] - 4.0).abs() < 1e-12 && (t.scale[1] - 2.0).abs() < 1e-12);
        assert!(t.rotation.abs() < 1e-12);
        assert!(
            t.offset[0].abs() < 1e-12 && (t.offset[1] + 1.0).abs() < 1e-12,
            "{t:?}"
        );
    }

    #[test]
    fn archive_bumps_alone_take_the_archive_transform() {
        // no `tex image`: the transmatrix is left at identity
        let spec = b"bumps png noise\nscale 20 20 1\nimport\n";
        let zip = package(&[("material.mat", spec), ("noise.png", b"png")]);
        let mut mat = MaterialDef::new("m".into());
        apply_archive(&mut mat, &zip, "");
        normal_map_alone(&mut mat);
        assert!(close(mat.uv_matrix, [20.0, 0.0, 0.0, 0.0, 20.0, 0.0]));
        assert_eq!(mat.normal_transform(), None);
        assert!(mat.texture.is_none() && mat.has_maps());
    }

    #[test]
    fn archive_bumps_alone_keep_a_folded_transmatrix() {
        let spec = b"bumps png noise\nscale 100 100 1\nnscale 50 50 1\n";
        let zip = package(&[("ed.mat", spec), ("noise.png", b"png")]);
        let mut mat = MaterialDef::new("m".into());
        mat.uv_matrix = [100.0, 0.0, 0.0, 0.0, 100.0, 0.0]; // folded by the exporter
        apply_archive(&mut mat, &zip, "");
        normal_map_alone(&mut mat);
        assert!(close(mat.uv_matrix, [100.0, 0.0, 0.0, 0.0, 100.0, 0.0]));
        let t = mat.normal_transform().expect("nscale differs from scale");
        assert!((t.scale[0] - 0.5).abs() < 1e-12 && (t.scale[1] - 0.5).abs() < 1e-12);
    }

    #[test]
    fn normal_map_rotation_matches_khr_texture_transform() {
        // nrotate 90 on an identity base: KHR rotation +90 degrees, as for
        // an archive's `rotate 0 0 90`
        let spec = b"tex image png c\nbumps png n\nnrotate 0 0 90\n";
        let zip = package(&[("material.mat", spec), ("c.png", b"a"), ("n.png", b"b")]);
        let mut mat = MaterialDef::new("m".into());
        apply_archive(&mut mat, &zip, "");
        let t = mat.normal_transform().unwrap();
        assert!(
            (t.rotation - std::f64::consts::FRAC_PI_2).abs() < 1e-12,
            "{t:?}"
        );
        assert!((t.scale[0] - 1.0).abs() < 1e-12 && (t.scale[1] - 1.0).abs() < 1e-12);
    }

    #[test]
    fn auto_mapping_is_parsed() {
        let zip = package(&[
            ("material.mat", b"tex image jpg w\nauto yyx\n"),
            ("w.jpg", b"w"),
        ]);
        let mut mat = MaterialDef::new("m".into());
        apply_archive(&mut mat, &zip, "");
        assert_eq!(
            mat.auto_mapping,
            Some(AutoMapping {
                u_axes: *b"yyx",
                mirror: None
            })
        );
        assert_eq!(
            AutoMapping::parse(&["xzx", "yz"]),
            Some(AutoMapping {
                u_axes: *b"xzx",
                mirror: Some("yz".into())
            })
        );
        assert_eq!(AutoMapping::parse(&["xq"]), None);
        assert_eq!(AutoMapping::parse(&[]), None);
    }

    fn color(index: Option<i32>, rgb: Option<u32>) -> Color {
        Color { index, rgb }
    }

    #[test]
    fn colors() {
        assert_eq!(
            parse_color(color(Some(256), None), false),
            ColorRef::ByLayer
        );
        assert_eq!(parse_color(color(None, None), false), ColorRef::ByBlock);
        assert_eq!(
            parse_color(color(Some(254), Some(0xC2E4E4E4)), false),
            ColorRef::Rgb([0xE4, 0xE4, 0xE4])
        );
        assert_eq!(
            parse_color(color(Some(256), Some(0xC3000001)), true),
            ColorRef::Rgb([255, 0, 0])
        );
        assert_eq!(
            parse_color(color(Some(5), None), false),
            ColorRef::Rgb(ACI[5])
        );
        assert_eq!(
            parse_color(color(Some(256), None), true),
            ColorRef::Rgb(ACI[7])
        );
        // ACI 7 is the foreground color, black; true color white is not
        assert_eq!(
            parse_color(color(Some(7), None), false),
            ColorRef::Rgb([0, 0, 0])
        );
        assert_eq!(
            parse_color(color(None, Some(0xC2FF_FFFF)), false),
            ColorRef::Rgb([255, 255, 255])
        );
        assert_eq!(ACI[11], [255, 127, 127]);
        assert_eq!(ACI[20], [255, 63, 0]);
    }
}
