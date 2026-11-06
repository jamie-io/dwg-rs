// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! STL reader (binary and ASCII): one mesh, identical corners welded,
//! flat faces (normals from the corners' winding).

use std::collections::HashMap;
use std::path::Path;

use super::{InputError, Mesh, Node, Primitive, Scene, clean_faces, flat_normals};
use crate::geom::{IDENTITY, Vec3};

pub fn read(path: &Path) -> Result<Scene, InputError> {
    let data = std::fs::read(path)?;
    let name = path
        .file_stem()
        .map_or_else(|| "stl".into(), |s| s.to_string_lossy().into_owned());
    parse(&data, &name)
}

pub fn parse(data: &[u8], name: &str) -> Result<Scene, InputError> {
    let triangles = match binary_count(data) {
        Some(n) => binary(data, n),
        None => ascii(&String::from_utf8_lossy(data))?,
    };
    if triangles.is_empty() {
        return Err(InputError("no triangles in the STL file".into()));
    }
    let mut prim = Primitive::default();
    let mut ids: HashMap<[u64; 3], u32> = HashMap::new();
    for t in &triangles {
        prim.faces.push(3);
        for p in t {
            let key = p.map(f64::to_bits);
            let i = *ids.entry(key).or_insert_with(|| {
                prim.positions.push(*p);
                prim.positions.len() as u32 - 1
            });
            prim.faces.push(i);
        }
    }
    let (faces, dropped) = clean_faces(&prim.faces, prim.positions.len());
    prim.faces = faces;
    flat_normals(&mut prim);
    let mut scene = Scene::default();
    scene.skip("degenerate faces", dropped);
    scene.meshes.push(Mesh {
        name: name.to_string(),
        primitives: vec![prim],
    });
    scene.nodes.push(Node {
        name: name.to_string(),
        matrix: IDENTITY,
        mesh: 0,
    });
    Ok(scene)
}

/// The triangle count of a binary STL (80-byte header, count, 50 bytes per
/// triangle), or None for ASCII. Binary headers may start with "solid" too
/// and files may carry trailing bytes, so the content decides: text that
/// starts with "solid" (after a byte order mark or blank lines) and has a
/// "facet" early on, without NUL bytes, is ASCII.
fn binary_count(data: &[u8]) -> Option<usize> {
    let count = (data.len() >= 84)
        .then(|| u32::from_le_bytes(data[80..84].try_into().unwrap()) as usize)
        .filter(|&n| {
            n.checked_mul(50)
                .and_then(|s| s.checked_add(84))
                .is_some_and(|s| s <= data.len())
        });
    let exact = count.is_some_and(|n| 84 + 50 * n == data.len());
    let head = &data[..data.len().min(1024)];
    let text = String::from_utf8_lossy(head);
    let ascii = text
        .trim_start_matches('\u{feff}')
        .trim_start()
        .starts_with("solid")
        && text.contains("facet")
        && !data[..data.len().min(512)].contains(&0);
    if exact || !ascii { count } else { None }
}

fn binary(data: &[u8], count: usize) -> Vec<[Vec3; 3]> {
    data[84..84 + 50 * count]
        .chunks_exact(50)
        .map(|rec| {
            let f = |i: usize| f32::from_le_bytes(rec[i * 4..i * 4 + 4].try_into().unwrap()) as f64;
            // 12 bytes of facet normal first; recomputed from the corners
            [3, 6, 9].map(|k| [f(k), f(k + 1), f(k + 2)])
        })
        .collect()
}

fn ascii(text: &str) -> Result<Vec<[Vec3; 3]>, InputError> {
    let mut corners = vec![];
    for (lineno, line) in text.lines().enumerate() {
        let mut parts = line.split_whitespace();
        if parts.next() != Some("vertex") {
            continue;
        }
        let v: Vec<f64> = parts.filter_map(|t| t.parse().ok()).collect();
        if v.len() != 3 {
            return Err(InputError(format!("line {}: invalid vertex", lineno + 1)));
        }
        corners.push([v[0], v[1], v[2]]);
    }
    if corners.len() % 3 != 0 {
        return Err(InputError("facet with other than 3 vertices".into()));
    }
    Ok(corners
        .chunks_exact(3)
        .map(|c| [c[0], c[1], c[2]])
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ascii_tetra_corner_welded() {
        let text = "solid t
            facet normal 0 0 -1
             outer loop
              vertex 0 0 0
              vertex 0 1 0
              vertex 1 0 0
             endloop
            endfacet
            facet normal 0 -1 0
             outer loop
              vertex 0 0 0
              vertex 1 0 0
              vertex 0 0 1
             endloop
            endfacet
            endsolid t";
        let scene = parse(text.as_bytes(), "t").unwrap();
        let prim = &scene.meshes[0].primitives[0];
        // welded corners, then one vertex per face corner for flat normals
        assert_eq!(prim.positions.len(), 4 + 6);
        assert_eq!(prim.faces, vec![3, 4, 5, 6, 3, 7, 8, 9]);
        let n = prim.normals.as_ref().unwrap();
        assert_eq!(n[4], [0.0, 0.0, -1.0]);
        assert_eq!(n[7], [0.0, -1.0, 0.0]);
        assert!(!scene.y_up);
    }

    #[test]
    fn ascii_with_bom_and_blank_line() {
        let text = "\u{feff}\n\nsolid t\nfacet normal 0 0 1\nouter loop\nvertex 0 0 0\nvertex 1 0 0\nvertex 0 1 0\nendloop\nendfacet\nendsolid t\n";
        let scene = parse(text.as_bytes(), "t").unwrap();
        let prim = &scene.meshes[0].primitives[0];
        assert_eq!(prim.faces.len(), 4);
        assert_eq!(
            prim.normals.as_ref().unwrap()[prim.faces[1] as usize],
            [0.0, 0.0, 1.0]
        );
    }

    #[test]
    fn binary_with_solid_header_and_trailing_bytes() {
        let mut data = b"solid exported by a CAD program".to_vec();
        data.resize(80, 0);
        data.extend(1u32.to_le_bytes());
        for f in [
            0.0f32, 0.0, 1.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 2.0, 0.0,
        ] {
            data.extend(f.to_le_bytes());
        }
        data.extend([0, 0]);
        data.extend(b"trailing junk");
        let scene = parse(&data, "b").unwrap();
        assert_eq!(scene.meshes[0].primitives[0].faces.len(), 4);
    }

    #[test]
    fn empty_is_an_error() {
        assert!(parse(b"solid nothing\nendsolid nothing\n", "e").is_err());
    }

    #[test]
    fn binary_starting_with_solid() {
        let mut data = b"solid but binary".to_vec();
        data.resize(80, 0);
        data.extend(1u32.to_le_bytes());
        for f in [
            0.0f32, 0.0, 1.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 2.0, 0.0,
        ] {
            data.extend(f.to_le_bytes());
        }
        data.extend([0, 0]);
        let scene = parse(&data, "b").unwrap();
        let prim = &scene.meshes[0].primitives[0];
        assert_eq!(prim.positions[prim.faces[3] as usize], [0.0, 2.0, 0.0]);
    }
}
