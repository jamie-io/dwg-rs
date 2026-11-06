// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Wavefront OBJ reader with MTL materials. One mesh per object/group, one
//! primitive per material; faces keep their polygons. Vertices without a
//! normal get one from their faces: averaged within a smoothing group
//! (`s 1`), flat outside one (`s off`, the default).

use std::collections::HashMap;
use std::path::Path;

use super::{InputError, Material, Mesh, Node, Primitive, Scene, Texture, clean_faces};
use crate::geom::{IDENTITY, Vec2, Vec3, ZERO, add, newell_normal, normalize};

pub fn read(path: &Path) -> Result<Scene, InputError> {
    let text = std::fs::read(path)?;
    parse(&String::from_utf8_lossy(&text), path.parent())
}

/// `dir` resolves mtllib files.
pub fn parse(text: &str, dir: Option<&Path>) -> Result<Scene, InputError> {
    let mut b = Builder {
        scene: Scene {
            y_up: true,
            ..Scene::default()
        },
        ..Builder::default()
    };
    for (lineno, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        let (key, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
        let rest = rest.trim();
        let nums = || -> Vec<f64> {
            rest.split_whitespace()
                .map_while(|t| t.parse::<f64>().ok())
                .collect()
        };
        let bad = || InputError(format!("line {}: invalid {key}", lineno + 1));
        match key {
            "v" => {
                let v = nums();
                if v.len() < 3 {
                    return Err(bad());
                }
                b.v.push([v[0], v[1], v[2]]);
            }
            "vt" => {
                let v = nums();
                if v.is_empty() {
                    return Err(bad());
                }
                b.vt.push([v[0], v.get(1).copied().unwrap_or(0.0)]);
            }
            "vn" => {
                let v = nums();
                if v.len() < 3 {
                    return Err(bad());
                }
                b.vn.push([v[0], v[1], v[2]]);
            }
            "f" => {
                let refs = rest
                    .split_whitespace()
                    .map(|t| b.vertex_ref(t))
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(bad)?;
                b.face(&refs);
            }
            "l" => {
                let refs = rest
                    .split_whitespace()
                    .map(|t| b.vertex_ref(t))
                    .collect::<Option<Vec<_>>>()
                    .ok_or_else(bad)?;
                b.polyline(&refs);
            }
            "o" | "g" => b.set_mesh(if rest.is_empty() { "default" } else { rest }),
            "s" => {
                b.smooth = match rest {
                    "" | "off" | "0" => None,
                    group => Some(group.parse().unwrap_or(1)),
                }
            }
            "usemtl" => b.material = Some(b.material_index(rest)),
            "mtllib" => {
                for name in mtl_names(rest) {
                    let path =
                        dir.map_or_else(|| Path::new(&name).to_path_buf(), |d| d.join(&name));
                    match std::fs::read(&path) {
                        Ok(data) => b.mtl(&String::from_utf8_lossy(&data), path.parent()),
                        Err(_) => b
                            .scene
                            .skip(format!("material library {name} (not found)"), 1),
                    }
                }
            }
            _ => {}
        }
    }
    Ok(b.finish())
}

/// mtllib takes several file names; a single one may contain spaces.
fn mtl_names(rest: &str) -> Vec<String> {
    let parts: Vec<&str> = rest.split_whitespace().collect();
    if parts.len() > 1
        && parts
            .iter()
            .all(|p| p.to_ascii_lowercase().ends_with(".mtl"))
    {
        parts.into_iter().map(str::to_string).collect()
    } else {
        vec![rest.to_string()]
    }
}

/// (position, uv, normal) indices into the file's lists.
type VertexRef = (usize, Option<usize>, Option<usize>);

/// Where a vertex's normal comes from.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
enum NormalFrom {
    /// The file's normal (`vn`).
    Given,
    /// Faces of this smoothing group around the position.
    Group(u32),
    /// Its one face (flat).
    Face(usize),
}

/// A face for normals the file does not give: mesh, primitive, face
/// number, its vertices, its smoothing group.
type FaceRecord = (usize, usize, usize, Vec<u32>, Option<u32>);

/// A primitive (index in its mesh) and its vertices by reference.
type PrimitiveSlot = (usize, HashMap<(VertexRef, NormalFrom), u32>);

#[derive(Default)]
struct Builder {
    scene: Scene,
    v: Vec<Vec3>,
    vt: Vec<Vec2>,
    vn: Vec<Vec3>,
    /// Current mesh (index into scene.meshes, created on first use).
    mesh: Option<usize>,
    mesh_name: Option<String>,
    material: Option<usize>,
    /// (mesh, material) -> primitive, with its vertex map.
    prims: HashMap<(usize, Option<usize>), PrimitiveSlot>,
    /// Current smoothing group (None: off).
    smooth: Option<u32>,
    /// Faces so far, for normals the file does not give.
    faces: usize,
    /// (mesh, primitive) -> per vertex: where its normal comes from, and the
    /// file position it stands for
    sources: HashMap<(usize, usize), Vec<(NormalFrom, usize)>>,
    face_list: Vec<FaceRecord>,
    /// Primitives where some vertices have texture coordinates and some not.
    no_uvs: Vec<(usize, usize)>,
}

impl Builder {
    /// 1-based or negative (relative) index.
    fn index(i: &str, len: usize) -> Option<usize> {
        let i: i64 = i.parse().ok()?;
        let i = if i < 0 { len as i64 + i } else { i - 1 };
        (0..len as i64).contains(&i).then_some(i as usize)
    }

    fn vertex_ref(&self, token: &str) -> Option<VertexRef> {
        let mut parts = token.split('/');
        let v = Self::index(parts.next()?, self.v.len())?;
        let vt = match parts.next() {
            Some("") | None => None,
            Some(t) => Some(Self::index(t, self.vt.len())?),
        };
        let vn = match parts.next() {
            Some("") | None => None,
            Some(t) => Some(Self::index(t, self.vn.len())?),
        };
        Some((v, vt, vn))
    }

    fn set_mesh(&mut self, name: &str) {
        self.mesh_name = Some(name.to_string());
        self.mesh = self.scene.meshes.iter().position(|m| m.name == name);
    }

    fn material_index(&mut self, name: &str) -> usize {
        if let Some(i) = self.scene.materials.iter().position(|m| m.name == name) {
            return i;
        }
        self.scene.materials.push(default_material(name));
        self.scene.materials.len() - 1
    }

    /// The current primitive (mesh index, primitive index).
    fn primitive(&mut self) -> (usize, usize) {
        let mesh = match self.mesh {
            Some(m) => m,
            None => {
                let name = self.mesh_name.clone().unwrap_or_else(|| "default".into());
                self.scene.meshes.push(Mesh {
                    name,
                    primitives: vec![],
                });
                let m = self.scene.meshes.len() - 1;
                self.mesh = Some(m);
                m
            }
        };
        let material = self.material;
        let prims = &mut self.scene.meshes[mesh].primitives;
        let (p, _) = self.prims.entry((mesh, material)).or_insert_with(|| {
            prims.push(Primitive {
                material,
                normals: Some(vec![]),
                uvs: Some(vec![]),
                ..Primitive::default()
            });
            (prims.len() - 1, HashMap::new())
        });
        (mesh, *p)
    }

    fn vertex(&mut self, (mesh, p): (usize, usize), r: VertexRef, from: NormalFrom) -> u32 {
        let map = &mut self.prims.get_mut(&(mesh, self.material)).unwrap().1;
        if let Some(&i) = map.get(&(r, from)) {
            return i;
        }
        let prim = &mut self.scene.meshes[mesh].primitives[p];
        let i = prim.positions.len() as u32;
        map.insert((r, from), i);
        prim.positions.push(self.v[r.0]);
        // filled in `finish` where the file has none
        prim.normals
            .get_or_insert_with(Vec::new)
            .push(r.2.map_or(ZERO, |n| self.vn[n]));
        self.sources.entry((mesh, p)).or_default().push((from, r.0));
        match r.1 {
            // OBJ's v grows upwards, the scene's (glTF's) downwards
            Some(t) => prim
                .uvs
                .get_or_insert_with(Vec::new)
                .push([self.vt[t][0], 1.0 - self.vt[t][1]]),
            None => {
                prim.uvs.get_or_insert_with(Vec::new).push([0.0, 0.0]);
                // lines need none
                if from != NormalFrom::Given || r.2.is_some() {
                    self.no_uvs.push((mesh, p));
                }
            }
        }
        i
    }

    fn face(&mut self, refs: &[VertexRef]) {
        let at = self.primitive();
        let face = self.faces;
        self.faces += 1;
        let ids: Vec<u32> = refs
            .iter()
            .map(|&r| {
                let from = match (r.2, self.smooth) {
                    (Some(_), _) => NormalFrom::Given,
                    (None, Some(g)) => NormalFrom::Group(g),
                    (None, None) => NormalFrom::Face(face),
                };
                self.vertex(at, r, from)
            })
            .collect();
        self.face_list
            .push((at.0, at.1, face, ids.clone(), self.smooth));
        let faces = &mut self.scene.meshes[at.0].primitives[at.1].faces;
        faces.push(ids.len() as u32);
        faces.extend(ids);
    }

    fn polyline(&mut self, refs: &[VertexRef]) {
        let at = self.primitive();
        let ids: Vec<u32> = refs
            .iter()
            .map(|&(v, vt, _)| self.vertex(at, (v, vt, None), NormalFrom::Given))
            .collect();
        let lines = &mut self.scene.meshes[at.0].primitives[at.1].lines;
        lines.extend(ids.windows(2).map(|w| [w[0], w[1]]));
    }

    fn mtl(&mut self, text: &str, dir: Option<&Path>) {
        let mut cur: Option<usize> = None;
        let mut has_d = false;
        for line in text.lines() {
            let line = line.split('#').next().unwrap_or("").trim();
            let (key, rest) = line.split_once(char::is_whitespace).unwrap_or((line, ""));
            let rest = rest.trim();
            let num = |i: usize| {
                rest.split_whitespace()
                    .nth(i)
                    .and_then(|t| t.parse::<f64>().ok())
            };
            if key == "newmtl" {
                cur = Some(self.material_index(rest));
                has_d = false;
                continue;
            }
            let Some(m) = cur.map(|i| &mut self.scene.materials[i]) else {
                continue;
            };
            match key {
                "Kd" => {
                    if let (Some(r), Some(g), Some(b)) = (num(0), num(1), num(2)) {
                        m.color = [r, g, b].map(|c| c.clamp(0.0, 1.0));
                    }
                }
                "d" => {
                    if let Some(d) = num(0) {
                        m.alpha = d.clamp(0.0, 1.0);
                        has_d = true;
                    }
                }
                "Tr" if !has_d => {
                    if let Some(t) = num(0) {
                        m.alpha = (1.0 - t).clamp(0.0, 1.0);
                    }
                }
                // Blinn-Phong exponent to roughness
                "Ns" => {
                    if let Some(ns) = num(0) {
                        m.roughness = (2.0 / (ns.max(0.0) + 2.0)).sqrt();
                    }
                }
                "Pr" => m.roughness = num(0).unwrap_or(m.roughness).clamp(0.0, 1.0),
                "Pm" => m.metallic = num(0).unwrap_or(m.metallic).clamp(0.0, 1.0),
                "map_Kd" => {
                    let name = texture_file(rest);
                    let path =
                        dir.map_or_else(|| Path::new(&name).to_path_buf(), |d| d.join(&name));
                    match std::fs::read(&path) {
                        Ok(data) => match Texture::from_bytes(data) {
                            Some(t) => m.texture = Some(t),
                            None => self.scene.skip("texture (not PNG, JPEG or WebP)", 1),
                        },
                        Err(_) => self.scene.skip(format!("texture {name} (not found)"), 1),
                    }
                }
                _ => {}
            }
        }
    }

    fn finish(mut self) -> Scene {
        // normals the file does not give: per face, and per smoothing group
        // around a position (area weighted)
        let mut by_face: HashMap<usize, Vec3> = HashMap::new();
        let mut by_group: HashMap<(usize, usize, u32, usize), Vec3> = HashMap::new();
        for (mesh, p, face, ids, group) in &self.face_list {
            let prim = &self.scene.meshes[*mesh].primitives[*p];
            let pts: Vec<Vec3> = ids.iter().map(|&i| prim.positions[i as usize]).collect();
            let n = newell_normal(&pts);
            by_face.insert(*face, n);
            if let Some(g) = group {
                let sources = &self.sources[&(*mesh, *p)];
                for &i in ids {
                    let pos = sources[i as usize].1;
                    let acc = by_group.entry((*mesh, *p, *g, pos)).or_insert(ZERO);
                    *acc = add(*acc, n);
                }
            }
        }
        for ((mesh, p), sources) in &self.sources {
            let normals = self.scene.meshes[*mesh].primitives[*p]
                .normals
                .as_mut()
                .unwrap();
            for (i, &(from, pos)) in sources.iter().enumerate() {
                match from {
                    NormalFrom::Given => {}
                    NormalFrom::Group(g) => {
                        normals[i] =
                            normalize(by_group.get(&(*mesh, *p, g, pos)).copied().unwrap_or(ZERO))
                    }
                    NormalFrom::Face(f) => normals[i] = normalize(by_face[&f]),
                }
            }
        }
        // texture coordinates: none where no vertex has them; where only
        // some lack them, those get (0, 0)
        let mut missing: HashMap<(usize, usize), usize> = HashMap::new();
        for at in &self.no_uvs {
            *missing.entry(*at).or_default() += 1;
        }
        let mut partial = 0;
        for ((mesh, p), n) in missing {
            let prim = &mut self.scene.meshes[mesh].primitives[p];
            if n == prim.positions.len() {
                prim.uvs = None;
            } else {
                partial += n;
            }
        }
        self.scene.skip(
            "texture coordinates of vertices without vt (set to 0, 0)",
            partial,
        );
        let mut dropped = 0;
        for mesh in &mut self.scene.meshes {
            for prim in &mut mesh.primitives {
                let (faces, n) = clean_faces(&prim.faces, prim.positions.len());
                prim.faces = faces;
                dropped += n;
                prim.lines.retain(|&[a, b]| a != b);
            }
        }
        self.scene.skip("degenerate faces", dropped);
        for (i, m) in self.scene.meshes.iter().enumerate() {
            self.scene.nodes.push(Node {
                name: m.name.clone(),
                matrix: IDENTITY,
                mesh: i,
            });
        }
        self.scene
    }
}

fn default_material(name: &str) -> Material {
    Material {
        name: name.to_string(),
        color: [0.8; 3],
        alpha: 1.0,
        metallic: 0.0,
        roughness: 0.7,
        // OBJ has no notion of face culling
        double_sided: true,
        texture: None,
    }
}

/// The file name of a map_Kd statement, without its options.
fn texture_file(rest: &str) -> String {
    let mut tokens = rest.split_whitespace().peekable();
    let mut name = vec![];
    while let Some(t) = tokens.next() {
        if t.starts_with('-') && name.is_empty() {
            // option arguments: numbers or on/off
            while tokens.peek().is_some_and(|a| {
                a.parse::<f64>().is_ok() || matches!(*a, "on" | "off") || a.len() == 1
            }) {
                tokens.next();
            }
        } else {
            name.push(t);
        }
    }
    name.join(" ")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quad_and_triangle_with_materials() {
        let obj = "\
            # cube corner
            v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nv 0 0 1
            vt 0 0\nvt 1 0\nvt 1 1\nvt 0 1
            o part
            usemtl red
            f 1/1 2/2 3/3 4/4
            usemtl blue
            f -5/1 -4/2 -1/3
            l 1 5
        ";
        let scene = parse(obj, None).unwrap();
        assert_eq!(scene.meshes.len(), 1);
        let prims = &scene.meshes[0].primitives;
        assert_eq!(prims.len(), 2);
        assert_eq!(prims[0].faces, vec![4, 0, 1, 2, 3]);
        assert_eq!(prims[0].uvs.as_ref().unwrap()[2], [1.0, 0.0]);
        // no vn and no smoothing group: flat
        assert!(
            prims[0]
                .normals
                .as_ref()
                .unwrap()
                .iter()
                .all(|&n| n == [0.0, 0.0, 1.0])
        );
        assert_eq!(prims[1].faces, vec![3, 0, 1, 2]);
        assert_eq!(prims[1].positions[2], [0.0, 0.0, 1.0]);
        // the line's vertices have no uvs; the faces keep theirs
        assert_eq!(prims[1].lines, vec![[3, 4]]);
        assert_eq!(prims[1].uvs.as_ref().unwrap()[1], [1.0, 1.0]);
        assert!(scene.skipped.is_empty());
        assert_eq!(scene.materials[prims[1].material.unwrap()].name, "blue");
    }

    #[test]
    fn smoothing_groups_decide_normals() {
        // two faces folded 90 degrees along the x axis
        let obj = "v 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nv 1 0 -1\nv 0 0 -1
            s 1\nf 1 2 3 4\nf 1 6 5 2
            s off\nf 1 2 3 4\nf 1 6 5 2";
        let scene = parse(obj, None).unwrap();
        let prim = &scene.meshes[0].primitives[0];
        let n = prim.normals.as_ref().unwrap();
        let normal_of = |face: usize, corner: usize| n[prim.faces[face * 5 + 1 + corner] as usize];
        // smoothed: the shared corner (vertex 1) averages both faces
        let h = std::f64::consts::FRAC_1_SQRT_2;
        let s = normal_of(0, 0);
        assert!((s[1] - -h).abs() < 1e-12 && (s[2] - h).abs() < 1e-12);
        assert_eq!(normal_of(1, 0), s);
        // flat: each face its own normal
        assert_eq!(normal_of(2, 0), [0.0, 0.0, 1.0]);
        assert_eq!(normal_of(3, 0), [0.0, -1.0, 0.0]);
    }

    #[test]
    fn missing_texture_coordinates_are_reported() {
        let obj = "v 0 0 0\nv 1 0 0\nv 0 1 0\nvt 0.5 0.5\nf 1/1 2 3";
        let scene = parse(obj, None).unwrap();
        let prim = &scene.meshes[0].primitives[0];
        assert_eq!(prim.uvs.as_ref().unwrap()[0], [0.5, 0.5]);
        assert_eq!(scene.skipped.values().sum::<usize>(), 2);
    }

    #[test]
    fn map_kd_options() {
        assert_eq!(
            texture_file("-s 1 1 1 -clamp on wood grain.png"),
            "wood grain.png"
        );
        assert_eq!(texture_file("tex.jpg"), "tex.jpg");
    }
}
