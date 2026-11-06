// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Write a [`Scene`] as DWG.
//!
//! Each primitive becomes a MESH (r2010+) with the OFML UV, normal and
//! double-sided records; a mesh used by several nodes becomes a block with
//! one INSERT per node. Each material becomes a layer (its color), a
//! MATERIAL (set on the MESH) and an embedded OFML material archive (PBR
//! values and the texture), so OFML applications show textures and
//! AutoCAD/BricsCAD show colors.

use std::collections::{BTreeMap, HashSet};
use std::fmt;

use crate::dwg_writer::{Block, DwgBuilder, DwgVersion, MeshData, Style, WriteError};
use crate::geom::{
    IDENTITY, Mat, Vec2, Vec3, ZERO, add, apply, chain, cross, dot, length, mat_mul, newell_normal,
    normalize, ocs_axes, ocs_matrix, rotate_z, scale, translate,
};
use crate::input::{Material, Mesh, Scene};
use crate::model::unit_to_meters;
use crate::ofml::{ArchiveMaterial, material_archive};

#[derive(Debug, Clone)]
pub struct WriteOptions {
    pub version: DwgVersion,
    /// INSUNITS of the drawing; glTF meters are scaled to it.
    pub units: i32,
    /// INSUNITS-style unit of inputs that carry none (OBJ, STL); meters
    /// when not given.
    pub input_units: Option<i32>,
    /// The source is Z-up already (no Y-up to Z-up rotation).
    pub z_up: bool,
}

impl Default for WriteOptions {
    fn default() -> Self {
        WriteOptions {
            version: DwgVersion::R2018,
            units: 6,
            input_units: None,
            z_up: false,
        }
    }
}

#[derive(Debug, Default)]
pub struct WriteStats {
    pub meshes: usize,
    pub faces: usize,
    pub lines: usize,
    pub blocks: usize,
    pub inserts: usize,
    pub materials: usize,
    pub textures: usize,
    pub skipped: BTreeMap<String, usize>,
    pub warnings: Vec<String>,
}

impl fmt::Display for WriteStats {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(
            f,
            "written: MESH={} ({} faces), LINE={}, blocks={} (INSERT={})\nmaterials: {} ({} textured)",
            self.meshes,
            self.faces,
            self.lines,
            self.blocks,
            self.inserts,
            self.materials,
            self.textures
        )?;
        if !self.skipped.is_empty() {
            let list: Vec<String> = self
                .skipped
                .iter()
                .map(|(k, v)| format!("{k}={v}"))
                .collect();
            write!(f, "\nskipped: {}", list.join(", "))?;
        }
        for w in &self.warnings {
            write!(f, "\nwarning: {w}")?;
        }
        Ok(())
    }
}

/// Faces per MESH: LibreDWG's encoder silently writes an empty MESH when a
/// list grows past 16M values, so bigger primitives are split.
const MAX_MESH_FACES: usize = 1 << 20;

/// Encode `scene` as a DWG file image.
pub fn write(scene: &Scene, opt: &WriteOptions) -> Result<(Vec<u8>, WriteStats), WriteError> {
    let mut w = Writer {
        b: DwgBuilder::new(opt.version, opt.units)?,
        scene,
        stats: WriteStats {
            skipped: scene.skipped.clone(),
            ..WriteStats::default()
        },
        styles: vec![None; scene.materials.len()],
        layers: Names::new(&["0", "Defpoints"]),
        materials: Names::new(&["ByLayer", "ByBlock", "Global"]),
        blocks: Names::new(&[]),
        bounds: None,
    };
    let source_m = match (scene.meters_per_unit, opt.input_units) {
        (Some(m), _) => m,
        (None, Some(u)) => unit_to_meters(u),
        (None, None) => {
            w.stats.warnings.push(
                "the input has no units and is taken as meters; use --input-units to set them"
                    .into(),
            );
            1.0
        }
    };
    let k = source_m / unit_to_meters(opt.units);
    let root: Mat = if scene.y_up && !opt.z_up {
        // Y-up to DWG's Z-up: the inverse of the reader's root matrix
        [
            k, 0.0, 0.0, 0.0, 0.0, 0.0, -k, 0.0, 0.0, k, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0,
        ]
    } else {
        scale([k, k, k])
    };
    let mut uses = vec![vec![]; scene.meshes.len()];
    for node in &scene.nodes {
        uses[node.mesh].push(mat_mul(&root, &node.matrix));
    }
    let ms = w.b.model_space();
    for (mesh, worlds) in scene.meshes.iter().zip(&uses) {
        for m in worlds {
            w.extend_bounds(mesh, m);
        }
        let inserts: Vec<Option<InsertXf>> = if worlds.len() > 1 {
            worlds.iter().map(decompose).collect()
        } else {
            vec![]
        };
        if inserts.iter().flatten().count() > 1 {
            let name = w.blocks.unique(&sanitize(&mesh.name, "mesh"));
            let block = w.b.block(&name)?;
            w.primitives(block, mesh, &IDENTITY)?;
            w.b.end_block(block)?;
            w.stats.blocks += 1;
            let layer0 = Style {
                layer: w.b.layer_zero(),
                material: None,
            };
            for (x, m) in inserts.iter().zip(worlds) {
                match x {
                    Some(x) => {
                        w.b.insert(
                            ms,
                            &name,
                            x.ins_pt,
                            x.scale,
                            x.rotation,
                            x.extrusion,
                            layer0,
                        )?;
                        w.stats.inserts += 1;
                    }
                    None => w.primitives(ms, mesh, m)?,
                }
            }
        } else {
            for m in worlds {
                w.primitives(ms, mesh, m)?;
            }
        }
    }
    if let Some((min, max)) = w.bounds {
        w.b.set_extents(min, max);
    }
    let stats = w.stats;
    Ok((w.b.encode()?, stats))
}

struct Writer<'a> {
    b: DwgBuilder,
    scene: &'a Scene,
    stats: WriteStats,
    /// Layer and MATERIAL per scene material, created on first use.
    styles: Vec<Option<Style>>,
    layers: Names,
    materials: Names,
    blocks: Names,
    bounds: Option<(Vec3, Vec3)>,
}

impl Writer<'_> {
    fn style(&mut self, material: Option<usize>) -> Result<Style, WriteError> {
        let Some(i) = material.filter(|&i| i < self.scene.materials.len()) else {
            return Ok(Style {
                layer: self.b.layer_zero(),
                material: None,
            });
        };
        if let Some(s) = self.styles[i] {
            return Ok(s);
        }
        let m: &Material = &self.scene.materials[i];
        let rgb = m.color.map(|c| (c.clamp(0.0, 1.0) * 255.0).round() as u8);
        let base = sanitize(&m.name, "material");
        let layer = self.b.layer(&self.layers.unique(&base), rgb)?;
        let name = self.materials.unique(&base);
        let material = self.b.material(&name, rgb, m.alpha)?;
        let package = material_archive(&ArchiveMaterial {
            id: &name,
            diffuse: m.color,
            metallic: m.metallic,
            roughness: m.roughness,
            texture: m.texture.as_ref().map(|t| (t.data.as_slice(), t.ext)),
        });
        self.b.material_archive(&name, &package)?;
        self.stats.materials += 1;
        self.stats.textures += m.texture.is_some() as usize;
        let style = Style {
            layer,
            material: Some(material),
        };
        self.styles[i] = Some(style);
        Ok(style)
    }

    /// The mesh's primitives, transformed by `m`, into `block`.
    fn primitives(&mut self, block: Block, mesh: &Mesh, m: &Mat) -> Result<(), WriteError> {
        let normal_m = normal_matrix(m);
        let mirrored = det3(m) < 0.0;
        for prim in &mesh.primitives {
            let style = self.style(prim.material)?;
            let material = prim.material.and_then(|i| self.scene.materials.get(i));
            let positions: Vec<Vec3> = prim.positions.iter().map(|&p| apply(m, p)).collect();
            if !prim.faces.is_empty() {
                let mut faces = prim.faces.clone();
                if mirrored {
                    // keep the faces' outside where it was
                    let mut i = 0;
                    while i < faces.len() {
                        let n = faces[i] as usize;
                        faces[i + 1..i + 1 + n].reverse();
                        i += n + 1;
                    }
                }
                let normals: Option<Vec<Vec3>> = prim.normals.as_ref().map(|ns| {
                    let mut ns: Vec<Vec3> =
                        ns.iter().map(|&n| normalize(apply(&normal_m, n))).collect();
                    face_normal_fallback(&positions, &faces, &mut ns);
                    ns
                });
                // textures need UVs; DWG's v grows upwards
                let uvs: Option<Vec<_>> = prim
                    .uvs
                    .as_ref()
                    .filter(|_| material.is_some_and(|m| m.texture.is_some()))
                    .map(|uvs| uvs.iter().map(|uv| [uv[0], 1.0 - uv[1]]).collect());
                let double_sided = material
                    .map(|m| m.double_sided)
                    .or(self.scene.default_double_sided);
                for part in split_faces(&faces, MAX_MESH_FACES) {
                    // only the vertices the faces use
                    let (used, faces) = compact(&part, positions.len());
                    let pick3 = |v: &[Vec3]| used.iter().map(|&i| v[i]).collect::<Vec<_>>();
                    let pick2 = |v: &[Vec2]| used.iter().map(|&i| v[i]).collect::<Vec<_>>();
                    let (vertices, normals, uvs) = (
                        pick3(&positions),
                        normals.as_deref().map(pick3),
                        uvs.as_deref().map(pick2),
                    );
                    self.b.mesh(
                        block,
                        &MeshData {
                            vertices: &vertices,
                            faces: &faces,
                            normals: normals.as_deref(),
                            uvs: uvs.as_deref(),
                            double_sided,
                        },
                        style,
                    )?;
                    self.stats.meshes += 1;
                    self.stats.faces += face_count(&faces);
                }
            }
            for &[a, b] in &prim.lines {
                self.b
                    .line(block, positions[a as usize], positions[b as usize], style)?;
                self.stats.lines += 1;
            }
        }
        Ok(())
    }

    fn extend_bounds(&mut self, mesh: &Mesh, m: &Mat) {
        for p in mesh.primitives.iter().flat_map(|p| &p.positions) {
            let p = apply(m, *p);
            let (lo, hi) = self.bounds.get_or_insert((p, p));
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
    }
}

/// Zero normals (none in the source, or collapsed by the transform) take
/// the normal of the faces around the vertex.
fn face_normal_fallback(positions: &[Vec3], faces: &[u32], normals: &mut [Vec3]) {
    if normals.iter().all(|&n| n != ZERO) {
        return;
    }
    let mut acc = vec![ZERO; positions.len()];
    let mut i = 0;
    while i < faces.len() {
        let n = faces[i] as usize;
        let face = &faces[i + 1..i + 1 + n];
        i += n + 1;
        let pts: Vec<Vec3> = face.iter().map(|&v| positions[v as usize]).collect();
        let fnorm = newell_normal(&pts);
        for &v in face {
            acc[v as usize] = add(acc[v as usize], fnorm);
        }
    }
    for (n, a) in normals.iter_mut().zip(acc) {
        if *n == ZERO {
            *n = normalize(a);
        }
    }
}

/// Face lists of at most `max` faces each.
fn split_faces(faces: &[u32], max: usize) -> Vec<Vec<u32>> {
    let mut parts = vec![vec![]];
    let (mut i, mut count) = (0, 0);
    while i < faces.len() {
        let n = faces[i] as usize;
        if count == max {
            parts.push(vec![]);
            count = 0;
        }
        parts
            .last_mut()
            .unwrap()
            .extend_from_slice(&faces[i..i + 1 + n]);
        count += 1;
        i += n + 1;
    }
    parts
}

/// The vertices a face list uses (in first-use order) and the faces
/// renumbered to them.
fn compact(faces: &[u32], num_vertices: usize) -> (Vec<usize>, Vec<u32>) {
    let mut map = vec![u32::MAX; num_vertices];
    let mut used = vec![];
    let mut out = Vec::with_capacity(faces.len());
    let mut i = 0;
    while i < faces.len() {
        let n = faces[i] as usize;
        out.push(n as u32);
        for &v in &faces[i + 1..i + 1 + n] {
            if map[v as usize] == u32::MAX {
                map[v as usize] = used.len() as u32;
                used.push(v as usize);
            }
            out.push(map[v as usize]);
        }
        i += n + 1;
    }
    (used, out)
}

fn face_count(faces: &[u32]) -> usize {
    let (mut n, mut i) = (0, 0);
    while i < faces.len() {
        i += faces[i] as usize + 1;
        n += 1;
    }
    n
}

fn det3(m: &Mat) -> f64 {
    let c = |j: usize| [m[j], m[4 + j], m[8 + j]];
    dot(cross(c(0), c(1)), c(2))
}

/// Inverse transpose of the 3x3 part (normals stay perpendicular).
fn normal_matrix(m: &Mat) -> Mat {
    let c = |j: usize| [m[j], m[4 + j], m[8 + j]];
    let (a, b, cc) = (c(0), c(1), c(2));
    let d = dot(cross(a, b), cc);
    if d.abs() < 1e-300 {
        return IDENTITY;
    }
    // rows of the inverse transpose are the cross products / det
    let (r0, r1, r2) = (cross(b, cc), cross(cc, a), cross(a, b));
    [
        r0[0] / d,
        r1[0] / d,
        r2[0] / d,
        0.0, //
        r0[1] / d,
        r1[1] / d,
        r2[1] / d,
        0.0, //
        r0[2] / d,
        r1[2] / d,
        r2[2] / d,
        0.0, //
        0.0,
        0.0,
        0.0,
        1.0,
    ]
}

/// An INSERT's parameters, as the reader turns them back into a matrix:
/// OCS(extrusion) * T(ins_pt) * Rz(rotation) * S(scale).
#[derive(Debug, PartialEq)]
struct InsertXf {
    ins_pt: Vec3,
    scale: Vec3,
    rotation: f64,
    extrusion: Vec3,
}

impl InsertXf {
    fn matrix(&self) -> Mat {
        chain(&[
            ocs_matrix(Some(self.extrusion)),
            translate(self.ins_pt),
            rotate_z(self.rotation),
            scale(self.scale),
        ])
    }
}

/// `m` as an INSERT, unless it shears (non-uniform scale under rotation).
fn decompose(m: &Mat) -> Option<InsertXf> {
    let col = |j: usize| [m[j], m[4 + j], m[8 + j]];
    let cols = [col(0), col(1), col(2)];
    let mut s = cols.map(length);
    if s.iter().any(|&x| x < 1e-12) {
        return None;
    }
    let mut r = [0, 1, 2].map(|j| [cols[j][0] / s[j], cols[j][1] / s[j], cols[j][2] / s[j]]);
    if [(0, 1), (0, 2), (1, 2)]
        .iter()
        .any(|&(a, b)| dot(r[a], r[b]).abs() > 1e-7)
    {
        return None;
    }
    if dot(cross(r[0], r[1]), r[2]) < 0.0 {
        // mirrored: flip the extrusion
        s[2] = -s[2];
        r[2] = [-r[2][0], -r[2][1], -r[2][2]];
    }
    let [ax, ay, _] = ocs_axes(Some(r[2]));
    let t = [m[3], m[7], m[11]];
    let x = InsertXf {
        ins_pt: [dot(t, ax), dot(t, ay), dot(t, r[2])],
        scale: s,
        rotation: dot(r[0], ay).atan2(dot(r[0], ax)),
        extrusion: r[2],
    };
    let size = m.iter().fold(1.0f64, |a, v| a.max(v.abs()));
    let back = x.matrix();
    back.iter()
        .zip(m)
        .all(|(a, b)| (a - b).abs() <= 1e-7 * size)
        .then_some(x)
}

/// A symbol table name: control characters dropped, `<>/\":;?*|,=` and
/// backquote replaced, at most 240 characters (room for a "_2" suffix).
fn sanitize(name: &str, fallback: &str) -> String {
    let s: String = name
        .chars()
        .filter(|c| !c.is_control())
        .map(|c| {
            if "<>/\\\":;?*|,=`".contains(c) {
                '_'
            } else {
                c
            }
        })
        .take(240)
        .collect();
    let s = s.trim();
    if s.is_empty() {
        fallback.to_string()
    } else {
        s.to_string()
    }
}

/// Unique names in one symbol table (case-insensitive, as in AutoCAD).
struct Names(HashSet<String>);

impl Names {
    fn new(reserved: &[&str]) -> Self {
        Names(reserved.iter().map(|n| n.to_lowercase()).collect())
    }

    fn unique(&mut self, base: &str) -> String {
        let mut name = base.to_string();
        let mut n = 1;
        while !self.0.insert(name.to_lowercase()) {
            n += 1;
            name = format!("{base}_{n}");
        }
        name
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::{IDENTITY, rotate_z};

    #[test]
    fn insert_round_trip() {
        // rotation about a tilted axis, non-uniform scale, mirror
        let tilt = chain(&[
            ocs_matrix(Some(normalize([0.3, -0.5, 0.8]))),
            translate([1.0, 2.0, 3.0]),
            rotate_z(0.7),
            scale([2.0, 0.5, -1.5]),
        ]);
        for m in [IDENTITY, tilt, scale([0.001, 0.001, 0.001])] {
            let x = decompose(&m).expect("decomposable");
            let back = x.matrix();
            assert!(
                back.iter().zip(&m).all(|(a, b)| (a - b).abs() < 1e-9),
                "{x:?}"
            );
        }
        // shear
        let shear = mat_mul(&scale([1.0, 3.0, 1.0]), &rotate_z(0.5));
        assert!(decompose(&shear).is_none());
    }

    #[test]
    fn names() {
        let mut n = Names::new(&["ByLayer"]);
        assert_eq!(n.unique("bylayer"), "bylayer_2");
        assert_eq!(n.unique("Wood"), "Wood");
        assert_eq!(n.unique("WOOD"), "WOOD_2");
        assert_eq!(sanitize(" a/b:c ", "x"), "a_b_c");
        assert_eq!(sanitize("\n", "x"), "x");
    }

    #[test]
    fn normals_under_scale() {
        let m = scale([2.0, 1.0, 1.0]);
        let n = normalize(apply(&normal_matrix(&m), normalize([1.0, 1.0, 0.0])));
        // the plane x + y = c stretched along x has normal ~ (1, 2, 0)
        let e = normalize([1.0, 2.0, 0.0]);
        assert!(dot(n, e) > 1.0 - 1e-12);
    }
}
