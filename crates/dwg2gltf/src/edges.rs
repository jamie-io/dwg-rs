// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Visible edges of a triangle mesh, for drawing outlines in a viewer:
//! boundary edges (one triangle), non-manifold edges (three or more) and
//! creases (two triangles meeting at more than an angle).
//!
//! Vertices are welded by position first, so seams where the mesh splits
//! vertices for normals, UVs or materials do not count as boundaries.
//! The angle between two triangles is measured independently of their
//! winding: triangles of different materials are oriented separately, and a
//! coplanar pair wound in opposite directions is still flat.

use std::collections::HashMap;

/// The crease angle [`visible_edges`] is usually called with, in degrees.
pub const DEFAULT_CREASE_DEG: f64 = 30.0;

/// Positions closer than this times the mesh's bounding box diagonal are
/// one vertex.
pub const WELD_RELATIVE: f64 = 1e-5;

/// The visible edges of the triangles `triangles` (three indices into
/// `positions` each): pairs of indices into `positions`, each edge once, in
/// the order first met. An edge is visible when exactly one triangle uses it
/// (boundary), three or more do (non-manifold), or the two triangles on it
/// meet at more than `crease_deg` degrees. With `normals` (one per
/// position), such a fold only counts where the vertex normals of the two
/// triangles differ along the edge: a curved surface tessellated more
/// coarsely than `crease_deg` but shaded smooth shows no facet edges.
/// Degenerate and repeated triangles (also repeated with the other winding)
/// are ignored. The index of an edge end is the first vertex at that
/// position.
pub fn visible_edges(
    positions: &[[f32; 3]],
    normals: Option<&[[f32; 3]]>,
    triangles: &[u32],
    crease_deg: f64,
) -> Vec<[u32; 2]> {
    let rep = weld(positions, triangles);
    let point = |i: u32| positions[i as usize].map(f64::from);
    let cos_limit = crease_deg.clamp(0.0, 180.0).to_radians().cos();

    struct Edge {
        ends: [u32; 2],
        faces: u32,
        /// The first face's normal and whether it runs from the lower to the
        /// higher index.
        normal: [f64; 3],
        forward: bool,
        /// The first face's vertices (unwelded) at `ends`.
        corners: [u32; 2],
        crease: bool,
    }
    let mut edges: Vec<Edge> = vec![];
    let mut by_ends: HashMap<(u32, u32), usize> = HashMap::new();
    let mut seen: std::collections::HashSet<[u32; 3]> = std::collections::HashSet::new();

    for t in triangles.chunks_exact(3) {
        let [a, b, c] = [rep[t[0] as usize], rep[t[1] as usize], rep[t[2] as usize]];
        // the unwelded vertex of this triangle at a welded one
        let corner = |w: u32| t[[a, b, c].iter().position(|&x| x == w).unwrap()];
        if a == b || b == c || a == c {
            continue;
        }
        let mut key = [a, b, c];
        key.sort_unstable();
        if !seen.insert(key) {
            continue;
        }
        let normal = {
            let (pa, pb, pc) = (point(a), point(b), point(c));
            let u = [pb[0] - pa[0], pb[1] - pa[1], pb[2] - pa[2]];
            let v = [pc[0] - pa[0], pc[1] - pa[1], pc[2] - pa[2]];
            let n = [
                u[1] * v[2] - u[2] * v[1],
                u[2] * v[0] - u[0] * v[2],
                u[0] * v[1] - u[1] * v[0],
            ];
            let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
            if len == 0.0 || !len.is_finite() {
                continue; // collinear corners
            }
            n.map(|c| c / len)
        };
        for (u, v) in [(a, b), (b, c), (c, a)] {
            let forward = u < v;
            let ends = if forward { (u, v) } else { (v, u) };
            let corners = [corner(ends.0), corner(ends.1)];
            match by_ends.get(&ends) {
                None => {
                    by_ends.insert(ends, edges.len());
                    edges.push(Edge {
                        ends: [ends.0, ends.1],
                        faces: 1,
                        normal,
                        forward,
                        corners,
                        crease: false,
                    });
                }
                Some(&i) => {
                    let e = &mut edges[i];
                    e.faces += 1;
                    if e.faces == 2 {
                        // consistently wound neighbours run along the edge
                        // in opposite directions
                        let sign = if e.forward == forward { -1.0 } else { 1.0 };
                        let cos = sign
                            * (e.normal[0] * normal[0]
                                + e.normal[1] * normal[1]
                                + e.normal[2] * normal[2]);
                        e.crease = cos < cos_limit
                            && !normals.is_some_and(|n| smooth(n, e.corners, corners, sign));
                    }
                }
            }
        }
    }
    edges
        .into_iter()
        .filter(|e| e.faces != 2 || e.crease)
        .map(|e| e.ends)
        .collect()
}

/// Whether the vertex normals of two triangles agree at both ends of their
/// common edge (`a` and `b`: each triangle's vertices there; `sign` -1 when
/// the second triangle is wound against the first, whose normals are then
/// taken reversed).
fn smooth(normals: &[[f32; 3]], a: [u32; 2], b: [u32; 2], sign: f64) -> bool {
    /// About 2.6 degrees.
    const SAME: f64 = 0.999;
    (0..2).all(|k| {
        let (p, q) = (normals[a[k] as usize], normals[b[k] as usize]);
        let dot: f64 = (0..3).map(|i| p[i] as f64 * q[i] as f64).sum();
        let len = (0..3).map(|i| (p[i] as f64).powi(2)).sum::<f64>().sqrt()
            * (0..3).map(|i| (q[i] as f64).powi(2)).sum::<f64>().sqrt();
        len > 0.0 && sign * dot >= SAME * len
    })
}

/// For each position, the first position (in index order) within the weld
/// distance of it, among those `triangles` uses; positions no triangle uses
/// map to themselves.
fn weld(positions: &[[f32; 3]], triangles: &[u32]) -> Vec<u32> {
    let mut rep: Vec<u32> = (0..positions.len() as u32).collect();
    let mut used = vec![false; positions.len()];
    for &i in triangles {
        used[i as usize] = true;
    }
    let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
    for (p, _) in positions.iter().zip(&used).filter(|(_, u)| **u) {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k] as f64);
            hi[k] = hi[k].max(p[k] as f64);
        }
    }
    let diag = (0..3).map(|k| (hi[k] - lo[k]).powi(2)).sum::<f64>().sqrt();
    let eps = diag * WELD_RELATIVE;
    if !(eps > 0.0 && eps.is_finite()) {
        // a single point, or no triangles: exact positions
        let mut first: HashMap<[u32; 3], u32> = HashMap::new();
        for (i, p) in positions.iter().enumerate().filter(|(i, _)| used[*i]) {
            rep[i] = *first.entry(p.map(f32::to_bits)).or_insert(i as u32);
        }
        return rep;
    }
    let cell = |p: [f64; 3]| -> [i64; 3] {
        std::array::from_fn(|k| ((p[k] - lo[k]) / eps).floor() as i64)
    };
    let mut grid: HashMap<[i64; 3], Vec<u32>> = HashMap::new();
    for (i, p) in positions.iter().enumerate() {
        if !used[i] {
            continue;
        }
        let p = p.map(f64::from);
        let c = cell(p);
        let mut found = None;
        'search: for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let Some(list) = grid.get(&[c[0] + dx, c[1] + dy, c[2] + dz]) else {
                        continue;
                    };
                    for &j in list {
                        let q = positions[j as usize];
                        let d2: f64 = (0..3).map(|k| (p[k] - q[k] as f64).powi(2)).sum();
                        if d2 <= eps * eps {
                            found = Some(j);
                            break 'search;
                        }
                    }
                }
            }
        }
        match found {
            Some(j) => rep[i] = j,
            None => grid.entry(c).or_default().push(i as u32),
        }
    }
    rep
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(positions: &[[f32; 3]], tris: &[u32]) -> usize {
        visible_edges(positions, None, tris, DEFAULT_CREASE_DEG).len()
    }

    /// A unit cube with 8 shared corners, outward winding.
    fn cube() -> (Vec<[f32; 3]>, Vec<u32>) {
        let p: Vec<[f32; 3]> = (0..8)
            .map(|i| [(i & 1) as f32, (i >> 1 & 1) as f32, (i >> 2 & 1) as f32])
            .collect();
        let quads = [
            [0, 2, 3, 1], // z = 0
            [4, 5, 7, 6], // z = 1
            [0, 1, 5, 4], // y = 0
            [2, 6, 7, 3], // y = 1
            [0, 4, 6, 2], // x = 0
            [1, 3, 7, 5], // x = 1
        ];
        let tris = quads
            .iter()
            .flat_map(|q| [q[0], q[1], q[2], q[0], q[2], q[3]])
            .collect();
        (p, tris)
    }

    #[test]
    fn cube_has_twelve_edges() {
        let (p, t) = cube();
        let edges = visible_edges(&p, None, &t, DEFAULT_CREASE_DEG);
        assert_eq!(edges.len(), 12);
        // no face diagonal: the ends of every edge differ in one coordinate
        for [a, b] in edges {
            let (a, b) = (p[a as usize], p[b as usize]);
            assert_eq!((0..3).filter(|&k| a[k] != b[k]).count(), 1);
        }
    }

    #[test]
    fn cube_with_split_vertices_and_mixed_winding() {
        // every face with its own four vertices (as for flat normals), and
        // half the faces wound inwards (separately oriented materials)
        let (p, t) = cube();
        let mut positions = vec![];
        let mut tris = vec![];
        for (f, face) in t.chunks(6).enumerate() {
            let base = positions.len() as u32;
            let corners = [face[0], face[1], face[2], face[5]];
            positions.extend(corners.iter().map(|&i| p[i as usize]));
            let mut quad = [base, base + 1, base + 2, base, base + 2, base + 3];
            if f % 2 == 1 {
                quad.swap(1, 2);
                quad.swap(4, 5);
            }
            tris.extend(quad);
        }
        assert_eq!(count(&positions, &tris), 12);
    }

    #[test]
    fn flat_quad_has_no_diagonal() {
        let p = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let edges = visible_edges(&p, None, &[0, 1, 2, 0, 2, 3], DEFAULT_CREASE_DEG);
        assert_eq!(edges.len(), 4);
        assert!(!edges.contains(&[0, 2]));
        // the same with the second triangle wound the other way
        assert_eq!(count(&p, &[0, 1, 2, 0, 3, 2]), 4);
    }

    #[test]
    fn coplanar_split_with_duplicate_vertices() {
        // the two triangles do not share vertex indices, only positions
        // (within the weld distance)
        let p = [
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 0.0, 1e-7],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        assert_eq!(count(&p, &[0, 1, 2, 3, 4, 5]), 4);
    }

    #[test]
    fn cylinder_shows_its_rims_only() {
        const N: u32 = 32;
        let mut p = vec![];
        // N + 1 columns: the seam column repeats the first (split for UVs)
        for i in 0..=N {
            let a = std::f32::consts::TAU * (i % N) as f32 / N as f32;
            p.push([a.cos(), a.sin(), 0.0]);
            p.push([a.cos(), a.sin(), 2.0]);
        }
        let mut t = vec![];
        for i in 0..N {
            let (b0, t0, b1, t1) = (2 * i, 2 * i + 1, 2 * i + 2, 2 * i + 3);
            t.extend([b0, b1, t1, b0, t1, t0]);
        }
        let (bottom, top) = (p.len() as u32, p.len() as u32 + 1);
        p.push([0.0, 0.0, 0.0]);
        p.push([0.0, 0.0, 2.0]);
        for i in 0..N {
            t.extend([bottom, 2 * i + 2, 2 * i]);
            t.extend([top, 2 * i + 1, 2 * i + 3]);
        }
        let edges = visible_edges(&p, None, &t, DEFAULT_CREASE_DEG);
        assert_eq!(edges.len(), 2 * N as usize);
        // all on the rims: no seam, no side or cap spokes
        for [a, b] in edges {
            assert_eq!(p[a as usize][2], p[b as usize][2]);
            assert!(a != bottom && a != top && b != bottom && b != top);
        }
        // a coarse prism has its side edges as creases
        assert!(visible_edges(&p, None, &t, 5.0).len() > 2 * N as usize);
    }

    /// An 8-sided prism (45 degrees between side faces) with caps.
    fn prism(smooth_sides: bool) -> (Vec<[f32; 3]>, Vec<[f32; 3]>, Vec<u32>) {
        const N: u32 = 8;
        let (mut p, mut n, mut t) = (vec![], vec![], vec![]);
        let at = |i: u32| {
            let a = std::f32::consts::TAU * (i % N) as f32 / N as f32;
            (a.cos(), a.sin())
        };
        for i in 0..N {
            // each side face with its own four vertices
            let base = p.len() as u32;
            for (k, z) in [(i, 0.0), (i + 1, 0.0), (i + 1, 1.0), (i, 1.0)] {
                let (x, y) = at(k);
                p.push([x, y, z]);
                n.push(if smooth_sides {
                    [x, y, 0.0]
                } else {
                    let (mx, my) = at(i);
                    let (nx, ny) = at(i + 1);
                    let (sx, sy) = (mx + nx, my + ny);
                    let l = sx.hypot(sy);
                    [sx / l, sy / l, 0.0]
                });
            }
            t.extend([base, base + 1, base + 2, base, base + 2, base + 3]);
        }
        for (z, nz) in [(0.0, -1.0), (1.0, 1.0)] {
            let centre = p.len() as u32;
            p.push([0.0, 0.0, z]);
            n.push([0.0, 0.0, nz]);
            for i in 0..N {
                let (x, y) = at(i);
                p.push([x, y, z]);
                n.push([0.0, 0.0, nz]);
            }
            for i in 0..N {
                let (a, b) = (centre + 1 + i, centre + 1 + (i + 1) % N);
                t.extend(if nz < 0.0 {
                    [centre, b, a]
                } else {
                    [centre, a, b]
                });
            }
        }
        (p, n, t)
    }

    #[test]
    fn smooth_normals_hide_facets() {
        // shaded smooth: only the rims, although the sides fold by 45 degrees
        let (p, n, t) = prism(true);
        assert_eq!(
            visible_edges(&p, Some(&n), &t, DEFAULT_CREASE_DEG).len(),
            16
        );
        // flat shaded, or without normals: the side edges too
        let (p, n, t) = prism(false);
        assert_eq!(
            visible_edges(&p, Some(&n), &t, DEFAULT_CREASE_DEG).len(),
            24
        );
        assert_eq!(visible_edges(&p, None, &t, DEFAULT_CREASE_DEG).len(), 24);
        // smooth normals do not hide a boundary
        let (p, n, t) = prism(true);
        assert_eq!(
            visible_edges(&p, Some(&n), &t[..48], DEFAULT_CREASE_DEG).len(),
            16
        );
    }

    #[test]
    fn sheet_with_hole() {
        // a 3x3 square with a 1x1 hole, as 8 triangles between the outlines
        let p = [
            [0.0, 0.0, 0.0],
            [3.0, 0.0, 0.0],
            [3.0, 3.0, 0.0],
            [0.0, 3.0, 0.0],
            [1.0, 1.0, 0.0],
            [2.0, 1.0, 0.0],
            [2.0, 2.0, 0.0],
            [1.0, 2.0, 0.0],
        ];
        let mut t = vec![];
        for k in 0..4u32 {
            let (o0, o1, i0, i1) = (k, (k + 1) % 4, 4 + k, 4 + (k + 1) % 4);
            t.extend([o0, o1, i1, o0, i1, i0]);
        }
        let edges = visible_edges(&p, None, &t, DEFAULT_CREASE_DEG);
        assert_eq!(edges.len(), 8);
        for [a, b] in edges {
            // outline to outline, hole to hole
            assert_eq!(a < 4, b < 4);
        }
    }

    #[test]
    fn non_manifold_and_repeated_triangles() {
        // three flat fins on one edge: the shared edge is non-manifold
        let p = [
            [0.0, 0.0, 0.0],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
        ];
        let edges = visible_edges(&p, None, &[0, 1, 2, 0, 1, 3, 0, 1, 4], DEFAULT_CREASE_DEG);
        assert!(edges.contains(&[0, 1]));
        assert_eq!(edges.len(), 7);
        // a triangle repeated (as a back face) is one triangle
        assert_eq!(count(&p, &[0, 1, 2, 0, 2, 1]), 3);
        // degenerate input gives nothing
        assert_eq!(count(&p, &[0, 0, 1]), 0);
        assert_eq!(count(&[], &[]), 0);
    }
}
