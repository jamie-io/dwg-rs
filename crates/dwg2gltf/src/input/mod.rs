// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Mesh formats that can be written to DWG (glTF/GLB, OBJ, STL), read into
//! one neutral [`Scene`].

pub mod gltf;
pub mod obj;
pub mod stl;

use std::collections::BTreeMap;
use std::fmt;
use std::path::Path;

use crate::geom::{Mat, Vec2, Vec3, ZERO, newell_normal, normalize};

/// File extensions [`load`] reads.
pub const EXTENSIONS: [&str; 4] = ["glb", "gltf", "obj", "stl"];

#[derive(Debug, Default)]
pub struct Scene {
    pub meshes: Vec<Mesh>,
    pub materials: Vec<Material>,
    /// Mesh instances with their world matrices (source coordinates).
    pub nodes: Vec<Node>,
    /// The source is Y-up (glTF, OBJ) rather than Z-up (STL, DWG).
    pub y_up: bool,
    /// Size of a source unit in meters, if the format defines one (glTF).
    pub meters_per_unit: Option<f64>,
    /// Whether primitives without a material are double-sided, if the
    /// format says (glTF's default material is single-sided).
    pub default_double_sided: Option<bool>,
    /// What was left out, and why.
    pub skipped: BTreeMap<String, usize>,
}

impl Scene {
    pub fn skip(&mut self, what: impl Into<String>, n: usize) {
        if n > 0 {
            *self.skipped.entry(what.into()).or_default() += n;
        }
    }
}

#[derive(Debug, Default)]
pub struct Mesh {
    pub name: String,
    pub primitives: Vec<Primitive>,
}

#[derive(Debug, Default)]
pub struct Primitive {
    pub positions: Vec<Vec3>,
    pub normals: Option<Vec<Vec3>>,
    /// glTF convention: v grows downwards from the image's top edge.
    pub uvs: Option<Vec<Vec2>>,
    /// Polygons as in MESH (DXF 90): vertex count, then that many indices.
    pub faces: Vec<u32>,
    /// Line segments (index pairs).
    pub lines: Vec<[u32; 2]>,
    pub material: Option<usize>,
}

#[derive(Debug)]
pub struct Material {
    pub name: String,
    /// sRGB 0..1
    pub color: [f64; 3],
    pub alpha: f64,
    pub metallic: f64,
    pub roughness: f64,
    pub double_sided: bool,
    pub texture: Option<Texture>,
}

#[derive(Debug)]
pub struct Texture {
    pub data: Vec<u8>,
    /// "png" or "jpg"
    pub ext: &'static str,
}

impl Texture {
    /// PNG and JPEG are the formats OFML material archives take; WebP
    /// becomes PNG (lossless).
    pub fn from_bytes(data: Vec<u8>) -> Option<Texture> {
        let ext = if data.starts_with(b"\x89PNG\r\n\x1a\n") {
            "png"
        } else if data.starts_with(&[0xFF, 0xD8, 0xFF]) {
            "jpg"
        } else if data.starts_with(b"RIFF") && data.get(8..12) == Some(b"WEBP") {
            let img = image::load_from_memory_with_format(&data, image::ImageFormat::WebP).ok()?;
            let mut png = vec![];
            img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
                .ok()?;
            return Some(Texture {
                data: png,
                ext: "png",
            });
        } else {
            return None;
        };
        Some(Texture { data, ext })
    }
}

#[derive(Debug)]
pub struct Node {
    pub name: String,
    /// Row-major, source coordinates.
    pub matrix: Mat,
    pub mesh: usize,
}

#[derive(Debug)]
pub struct InputError(pub String);

impl fmt::Display for InputError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl From<std::io::Error> for InputError {
    fn from(e: std::io::Error) -> Self {
        InputError(e.to_string())
    }
}

/// Read a glTF/GLB, OBJ or STL file.
pub fn load(path: &Path) -> Result<Scene, InputError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match ext.as_str() {
        "glb" | "gltf" => gltf::read(path),
        "obj" => obj::read(path),
        "stl" => stl::read(path),
        _ => Err(InputError(format!(
            "unsupported input format .{ext} (expected .glb, .gltf, .obj or .stl)"
        ))),
    }
}

/// Give a primitive without normals flat ones (what glTF prescribes, and
/// what STL's facets and OBJ faces outside smoothing groups mean): each face
/// gets its own vertices with the face's normal. The shared vertices stay
/// for the lines.
pub fn flat_normals(prim: &mut Primitive) {
    if prim.normals.is_some() || prim.faces.is_empty() {
        return;
    }
    let mut normals = vec![ZERO; prim.positions.len()];
    let mut faces = Vec::with_capacity(prim.faces.len());
    let mut i = 0;
    while i < prim.faces.len() {
        let n = prim.faces[i] as usize;
        let face = &prim.faces[i + 1..i + 1 + n];
        i += n + 1;
        let pts: Vec<Vec3> = face.iter().map(|&v| prim.positions[v as usize]).collect();
        let normal = normalize(newell_normal(&pts));
        faces.push(n as u32);
        for &v in face {
            faces.push(prim.positions.len() as u32);
            prim.positions.push(prim.positions[v as usize]);
            normals.push(normal);
            if let Some(uvs) = prim.uvs.as_mut() {
                uvs.push(uvs[v as usize]);
            }
        }
    }
    prim.faces = faces;
    prim.normals = Some(normals);
}

/// Faces with repeated or out-of-range vertices are dropped; returns how
/// many. MESH requires at least 3 distinct vertices per face.
pub fn clean_faces(faces: &[u32], num_vertices: usize) -> (Vec<u32>, usize) {
    let mut out = Vec::with_capacity(faces.len());
    let mut dropped = 0;
    let mut i = 0;
    while i < faces.len() {
        let n = faces[i] as usize;
        let face = &faces[(i + 1).min(faces.len())..(i + 1 + n).min(faces.len())];
        i += n + 1;
        let valid = n >= 3
            && face.len() == n
            && face.iter().all(|&v| (v as usize) < num_vertices)
            && (0..n).all(|a| (a + 1..n).all(|b| face[a] != face[b]));
        if valid {
            out.push(n as u32);
            out.extend_from_slice(face);
        } else {
            dropped += 1;
        }
    }
    (out, dropped)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn faces_are_cleaned() {
        let faces = [3, 0, 1, 2, 3, 0, 0, 1, 4, 0, 1, 2, 3, 3, 0, 1, 9, 2, 0];
        let (out, dropped) = clean_faces(&faces, 4);
        assert_eq!(out, vec![3, 0, 1, 2, 4, 0, 1, 2, 3]);
        assert_eq!(dropped, 3);
    }
}
