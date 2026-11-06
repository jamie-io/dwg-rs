// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shared by the integration tests: a builder of small synthetic drawings
//! (`Doc`), reading converted files back, and a temporary folder. Other test
//! crates can include this file with `#[path = ".../tests/common/mod.rs"] mod common;`.

#![allow(dead_code)]

use std::path::{Path, PathBuf};

use dwg2gltf::dwg::{BlockHeader, Color, Data, Drawing, Entity, Handle, Layer, Object, Shape};
use dwg2gltf::{Converter, Document, Options, Stats};
use serde_json::{Map, Value};

pub const BYLAYER: Color = Color {
    index: Some(256),
    rgb: None,
};

/// Builds a minimal drawing: layers "0" and "RED", and a model space.
pub struct Doc {
    pub drawing: Drawing,
    pub next: Handle,
    pub layer0: Handle,
    pub red: Handle,
    pub model: Handle,
}

impl Doc {
    pub fn new(insunits: i32) -> Doc {
        let mut doc = Doc {
            drawing: Drawing {
                insunits,
                ..Drawing::default()
            },
            next: 0x100,
            layer0: 0,
            red: 0,
            model: 0,
        };
        doc.layer0 = doc.add("LAYER", Data::Layer(layer("0", 0xC3000007)));
        doc.red = doc.add("LAYER", Data::Layer(layer("RED", 0xC3000001)));
        doc.model = doc.block("*Model_Space", [0.0; 3]);
        doc
    }

    pub fn add(&mut self, kind: &str, data: Data) -> Handle {
        let handle = self.next;
        self.next += 1;
        self.drawing.objects.push(Object {
            handle,
            kind: kind.into(),
            owner: 0,
            xdic: 0,
            data,
        });
        handle
    }

    pub fn block(&mut self, name: &str, base_pt: [f64; 3]) -> Handle {
        self.add(
            "BLOCK_HEADER",
            Data::BlockHeader(BlockHeader {
                name: name.into(),
                base_pt,
                entities: Some(vec![]),
                xref: None,
            }),
        )
    }

    pub fn get(&mut self, h: Handle) -> &mut Object {
        self.drawing
            .objects
            .iter_mut()
            .find(|o| o.handle == h)
            .unwrap()
    }

    /// Add an entity on layer 0 with ByLayer color to a block.
    pub fn put(&mut self, block: Handle, kind: &str, shape: Shape) -> Handle {
        let ent = entity(self.layer0, shape);
        self.put_entity(block, kind, ent)
    }

    pub fn put_entity(&mut self, block: Handle, kind: &str, ent: Entity) -> Handle {
        let h = self.add(kind, Data::Entity(Box::new(ent)));
        match &mut self.get(block).data {
            Data::BlockHeader(b) => b.entities.as_mut().unwrap().push(h),
            _ => unreachable!(),
        }
        h
    }

    pub fn entity_mut(&mut self, h: Handle) -> &mut Entity {
        match &mut self.get(h).data {
            Data::Entity(e) => e,
            _ => unreachable!(),
        }
    }

    pub fn run(self, options: Options) -> (Map<String, Value>, Stats) {
        let doc = Document::new(self.drawing, None);
        let (gltf, stats) = Converter::new(&doc, options).run().unwrap();
        (gltf.document().clone(), stats)
    }
}

pub fn layer(name: &str, rgb: u32) -> Layer {
    Layer {
        name: name.into(),
        color: Color {
            index: Some(256),
            rgb: Some(rgb),
        },
        material: 0,
        off: false,
        frozen: false,
    }
}

pub fn entity(layer: Handle, shape: Shape) -> Entity {
    Entity {
        entmode: 0,
        invisible: false,
        layer,
        color: BYLAYER,
        material_flags: 0,
        material_refs: [0, 0],
        has_ds_data: false,
        thickness: 0.0,
        extrusion: Some([0.0, 0.0, 1.0]),
        material_mapper: None,
        shape,
    }
}

/// A temporary folder of this test process and thread.
pub fn tempdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dwg2gltf-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// World positions, face and line counts, and texture coordinates of a
/// scene read back by the glTF reader.
pub struct Readback {
    pub points: Vec<[f64; 3]>,
    pub faces: usize,
    pub lines: usize,
    pub uvs: Vec<[f64; 2]>,
    pub area: f64,
}

pub fn read_back(path: &Path) -> Readback {
    let scene = dwg2gltf::input::load(path).unwrap();
    let mut r = Readback {
        points: vec![],
        faces: 0,
        lines: 0,
        uvs: vec![],
        area: 0.0,
    };
    for node in &scene.nodes {
        for p in &scene.meshes[node.mesh].primitives {
            let world: Vec<[f64; 3]> = p
                .positions
                .iter()
                .map(|&v| dwg2gltf::geom::apply(&node.matrix, v))
                .collect();
            r.points.extend(&world);
            let mut i = 0;
            while i < p.faces.len() {
                r.faces += 1;
                let n = p.faces[i] as usize;
                let pts: Vec<[f64; 3]> = p.faces[i + 1..i + 1 + n]
                    .iter()
                    .map(|&v| world[v as usize])
                    .collect();
                r.area += dwg2gltf::geom::length(dwg2gltf::geom::newell_normal(&pts)) / 2.0;
                i += n + 1;
            }
            r.lines += p.lines.len();
            r.uvs.extend(p.uvs.iter().flatten());
        }
    }
    r
}

pub fn bounds(points: &[[f64; 3]]) -> ([f64; 3], [f64; 3]) {
    points.iter().fold(
        ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]),
        |(lo, hi), p| {
            (
                std::array::from_fn(|k| lo[k].min(p[k])),
                std::array::from_fn(|k| hi[k].max(p[k])),
            )
        },
    )
}

/// A triangle's corners and their normals.
pub type Corners = ([[f64; 3]; 3], [[f64; 3]; 3]);

/// A converted file's triangles in world space (glTF coordinates), wound
/// counter-clockwise around their front (mirrored nodes flipped back), with
/// their vertex normals.
pub fn world_triangles(path: &Path) -> Vec<Corners> {
    use dwg2gltf::geom::{apply, cross, dot, normalize};
    let scene = dwg2gltf::input::load(path).unwrap();
    let mut out = vec![];
    for node in &scene.nodes {
        let m = node.matrix;
        let col = |j: usize| [m[j], m[4 + j], m[8 + j]];
        let (a, b, c) = (col(0), col(1), col(2));
        let det = dot(cross(a, b), c);
        // inverse transpose of the 3x3 part: rows are the cross products / det
        let (r0, r1, r2) = (cross(b, c), cross(c, a), cross(a, b));
        let normal = |n: [f64; 3]| {
            normalize([
                (r0[0] * n[0] + r1[0] * n[1] + r2[0] * n[2]) / det,
                (r0[1] * n[0] + r1[1] * n[1] + r2[1] * n[2]) / det,
                (r0[2] * n[0] + r1[2] * n[1] + r2[2] * n[2]) / det,
            ])
        };
        for p in &scene.meshes[node.mesh].primitives {
            let Some(ns) = &p.normals else { continue };
            let mut i = 0;
            while i < p.faces.len() {
                let n = p.faces[i] as usize;
                let f = &p.faces[i + 1..i + 1 + n];
                i += n + 1;
                let mut t = [f[0], f[1], f[2]];
                if det < 0.0 {
                    t.swap(1, 2);
                }
                out.push((
                    t.map(|v| apply(&m, p.positions[v as usize])),
                    t.map(|v| normal(ns[v as usize])),
                ));
            }
        }
    }
    out
}

/// (triangles whose normals point against their winding, closed shells
/// that are inside out, signed volume of the closed shells). A closed shell
/// with a negative volume is not inside out when it encloses (next to)
/// nothing, like two triangles back to back, or when it lies inside the
/// rest of the scene: a cavity's wall faces into the cavity.
pub fn orientation_faults(path: &Path) -> (usize, usize, f64) {
    use dwg2gltf::geom::{cross, dot, sub};
    use std::collections::HashMap;
    let tris = world_triangles(path);
    let flipped = tris
        .iter()
        .filter(|(t, n)| {
            let g = cross(sub(t[1], t[0]), sub(t[2], t[0]));
            n.iter().map(|&n| dot(n, g)).sum::<f64>() < 0.0
        })
        .count();
    // shells: triangles joined across edges used exactly twice, in opposite
    // directions; closed when every edge is
    let key = |p: [f64; 3]| p.map(|c| (c as f32).to_bits());
    let mut edges: HashMap<([u32; 3], [u32; 3]), Vec<usize>> = HashMap::new();
    for (i, (t, _)) in tris.iter().enumerate() {
        for k in 0..3 {
            edges
                .entry((key(t[k]), key(t[(k + 1) % 3])))
                .or_default()
                .push(i);
        }
    }
    let mut parent: Vec<usize> = (0..tris.len()).collect();
    fn find(parent: &mut [usize], i: usize) -> usize {
        let mut r = i;
        while parent[r] != r {
            r = parent[r];
        }
        parent[i] = r;
        r
    }
    let mut open = vec![false; tris.len()];
    for ((a, b), uses) in &edges {
        let back = edges.get(&(*b, *a));
        match (uses.as_slice(), back.map(Vec::as_slice)) {
            ([i], Some([j])) => {
                let (ri, rj) = (find(&mut parent, *i), find(&mut parent, *j));
                parent[ri] = rj;
            }
            _ => uses.iter().for_each(|&i| open[i] = true),
        }
    }
    // shell: (signed volume, closed, triangles)
    let mut shells: HashMap<usize, (f64, bool, Vec<usize>)> = HashMap::new();
    for (i, (t, _)) in tris.iter().enumerate() {
        let r = find(&mut parent, i);
        let e = shells.entry(r).or_insert((0.0, true, vec![]));
        e.0 += dot(t[0], cross(t[1], t[2])) / 6.0;
        e.1 &= !open[i];
        e.2.push(i);
    }
    let closed: Vec<&(f64, bool, Vec<usize>)> = shells.values().filter(|s| s.1).collect();
    let inside_out = closed
        .iter()
        .filter(|(volume, _, members)| {
            if *volume >= 0.0 {
                return false;
            }
            let (lo, hi) = bounds(&members.iter().flat_map(|&i| tris[i].0).collect::<Vec<_>>());
            let size = (0..3).map(|k| hi[k] - lo[k]).fold(0.0, f64::max);
            // nothing enclosed: zero volume up to rounding
            if -volume <= 1e-9 * size * size * size {
                return false;
            }
            let centre: [f64; 3] = std::array::from_fn(|k| (lo[k] + hi[k]) / 2.0);
            !inside_the_rest(&tris, members, centre)
        })
        .count();
    (flipped, inside_out, closed.iter().map(|s| s.0).sum())
}

/// Whether `point` lies inside the triangles other than `members`: most of
/// 15 rays in skew directions (none along an axis, so they do not run along
/// box edges) cross them an odd number of times. The rest of the scene need
/// not be closed itself.
fn inside_the_rest(tris: &[Corners], members: &[usize], point: [f64; 3]) -> bool {
    use dwg2gltf::geom::{cross, dot, sub};
    let mine: std::collections::HashSet<usize> = members.iter().copied().collect();
    let crosses = |d: [f64; 3], t: &[[f64; 3]; 3]| {
        // Moeller-Trumbore
        let (e1, e2) = (sub(t[1], t[0]), sub(t[2], t[0]));
        let p = cross(d, e2);
        let det = dot(e1, p);
        if det.abs() < 1e-18 {
            return false;
        }
        let s = sub(point, t[0]);
        let u = dot(s, p) / det;
        let q = cross(s, e1);
        let v = dot(d, q) / det;
        (0.0..=1.0).contains(&u) && v >= 0.0 && u + v <= 1.0 && dot(e2, q) / det > 1e-12
    };
    let odd = (0..15)
        .filter(|&k| {
            // a spiral over the sphere, nudged off the axes
            let a = 2.399963 * k as f64;
            let z = 1.0 - 2.0 * (k as f64 + 0.5) / 15.0;
            let r = (1.0 - z * z).sqrt();
            let d = [r * a.cos() + 1e-3, r * a.sin() + 2e-3, z + 3e-3];
            let hits = tris
                .iter()
                .enumerate()
                .filter(|(i, (t, _))| !mine.contains(i) && crosses(d, t))
                .count();
            hits % 2 == 1
        })
        .count();
    odd >= 8
}
