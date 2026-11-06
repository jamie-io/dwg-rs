// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Accumulate triangles and line segments per material, then emit indexed
//! vertex streams with normals and texture coordinates.

use std::collections::HashMap;

use crate::geom::{
    Mat, Vec2, Vec3, ZERO, add, apply, cross, dominant_axis, dot, length, mul, normalize, sub,
    triangulate,
};
use crate::mapper::inverse3;

/// Faces whose normals differ by more than this stay hard-edged (unless an
/// entity asks for another angle, see [`TriangleBuilder::set_smoothing`]).
/// Normals are area-weighted and smoothed only across edges below this
/// threshold.
pub const SMOOTH_DEGREES: f64 = 30.0;

/// An open piece without known orientation counts as wrapping around its
/// centre (and is turned to face away from it) from this |volume| / area^1.5:
/// a 1 m x 0.3 m x 18 mm board is about 0.01, a slightly bent sheet 0.
const OPEN_PIECE_WRAPS: f64 = 1e-3;

/// How faces without stored UVs get texture coordinates.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Projection {
    /// OFML "auto" (OMATS 3.2 block mapping): per face along its dominant
    /// axis; U along Y on the X sides, along X on the Y and Z sides (DWG
    /// axes), mirrored on the back, left and bottom sides.
    // TODO: honour the U-axis triple of "auto <front><right><top>" (e.g.
    // "auto yyz", in MaterialDef::auto_mapping). The default above is
    // "xzx"; the drawings seen so far with other triples have stored UVs,
    // so there is no projected case to check the per-side signs against
    // yet.
    Box,
    /// OFML prjx / prjy / prjz: planar along one axis.
    Axis(usize),
}

/// A projection in a frame of its own instead of the block coordinates:
/// AutoCAD's material mapper (see [`crate::mapper`]). Positions are mapped
/// by `to_frame` (no unit or geometry scale: the frame is normalized), and
/// projected there as `projection` says, with the frame's directions
/// (`Axis(2)`: planar, U and V along the frame's X and Y).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct UvFrame {
    pub projection: Projection,
    /// Block coordinates to frame coordinates (row-major, affine).
    pub to_frame: Mat,
}

impl UvFrame {
    /// The frame's map for normals (the inverse transpose of the linear
    /// part), None when singular.
    fn normal_map(&self) -> Option<[[f64; 3]; 3]> {
        let m = &self.to_frame;
        let inv = inverse3([[m[0], m[1], m[2]], [m[4], m[5], m[6]], [m[8], m[9], m[10]]])?;
        Some(std::array::from_fn(|r| std::array::from_fn(|c| inv[c][r])))
    }
}

/// U and V of `p` (coordinates as projected, already scaled) on a face
/// with unit normal `n` (same coordinates), along `axis`.
fn project(p: Vec3, n: Vec3, axis: usize, projection: Projection) -> Vec2 {
    let (a, b) = [(1, 2), (0, 2), (0, 1)][axis];
    // OMATS block mapping: seen from outside, U runs the same way round
    // the block, so it is mirrored on the back (+Y), left (-X) and bottom
    // (-Z) sides
    let mirror = projection == Projection::Box && (n[axis] < 0.0) != (axis == 1);
    [if mirror { -p[a] } else { p[a] }, p[b]]
}

/// Output of [`TriangleBuilder::build`].
pub struct MeshData {
    /// Relative to the `origin` given to `build`.
    pub positions: Vec<Vec3>,
    pub normals: Vec<Vec3>,
    pub uvs: Option<Vec<Vec2>>,
    pub indices: Vec<u32>,
    pub repairs: Repairs,
}

/// What [`TriangleBuilder::build`] had to change.
#[derive(Default, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Repairs {
    /// Triangles turned to face the way their stored vertex normals point.
    pub turned_to_normals: usize,
    /// Triangles turned to agree with their neighbours (or, for closed
    /// shells, to face outwards).
    pub reoriented: usize,
    /// Triangles without area once welded or stored as float32.
    pub degenerate: usize,
}

type FrameKey = (Projection, [u64; 16]);

/// What makes frames equal (`==`): their projection and the bits of their
/// numbers, with -0.0 as 0.0. None for a frame with a NaN, which equals no
/// frame.
fn frame_key(f: &UvFrame) -> Option<FrameKey> {
    if f.to_frame.iter().any(|x| x.is_nan()) {
        return None;
    }
    let bits = f.to_frame.map(|x| if x == 0.0 { 0 } else { x.to_bits() });
    Some((f.projection, bits))
}

pub struct TriangleBuilder {
    weld: f64,
    /// grid cell -> vertices in it
    cells: HashMap<[i128; 3], Vec<u32>>,
    positions: Vec<Vec3>,
    tris: Vec<[u32; 3]>,
    normals: Vec<Option<[Vec3; 3]>>,
    uvs: Vec<Option<[Vec2; 3]>>,
    /// per triangle: its own projection frame, an index into `frames`
    frame: Vec<Option<u32>>,
    frames: Vec<UvFrame>,
    /// `frames` by [`frame_key`]: drawings give every entity a frame of its
    /// own, too many to compare each new one with
    frame_index: HashMap<FrameKey, u32>,
    frame_now: Option<u32>,
    /// cosine of the smoothing angle, per triangle
    smooth: Vec<f64>,
    smooth_cos: f64,
    /// per triangle: its winding is known to face outwards (from a B-rep)
    oriented: Vec<bool>,
    oriented_now: bool,
}

impl TriangleBuilder {
    pub fn new(weld: f64) -> Self {
        TriangleBuilder {
            weld,
            cells: HashMap::new(),
            positions: vec![],
            tris: vec![],
            normals: vec![],
            uvs: vec![],
            frame: vec![],
            frames: vec![],
            frame_index: HashMap::new(),
            frame_now: None,
            smooth: vec![],
            smooth_cos: SMOOTH_DEGREES.to_radians().cos(),
            oriented: vec![],
            oriented_now: false,
        }
    }

    pub fn is_empty(&self) -> bool {
        self.tris.is_empty()
    }

    /// Some triangle has no stored UVs (and would be projected).
    pub fn has_unmapped(&self) -> bool {
        self.uvs.iter().any(Option::is_none)
    }

    /// The smoothing angle for the triangles added from now on.
    pub fn set_smoothing(&mut self, degrees: f64) {
        self.smooth_cos = degrees.to_radians().cos();
    }

    /// The projection frame of the triangles added from now on (None: the
    /// projection given to [`TriangleBuilder::build`]).
    pub fn set_frame(&mut self, frame: Option<UvFrame>) {
        self.frame_now = frame.map(|f| {
            let key = frame_key(&f);
            if let Some(&i) = key.as_ref().and_then(|k| self.frame_index.get(k)) {
                return i;
            }
            self.frames.push(f);
            let i = self.frames.len() as u32 - 1;
            if let Some(key) = key {
                self.frame_index.insert(key, i);
            }
            i
        });
    }

    /// Whether the triangles added from now on face the right way already
    /// (tessellated B-rep faces): they decide for open pieces they belong to
    /// and are only turned with a closed shell that is inside out.
    pub fn set_oriented(&mut self, oriented: bool) {
        self.oriented_now = oriented;
    }

    /// The vertex at `p`: an existing one within the weld distance (also
    /// across grid cell borders), else a new one.
    fn point(&mut self, p: Vec3) -> u32 {
        let w = self.weld;
        let key = p.map(|c| (c / w).round_ties_even() as i128);
        let mut best: Option<(u32, f64)> = None;
        for dx in -1..=1 {
            for dy in -1..=1 {
                for dz in -1..=1 {
                    let cell = [key[0] + dx, key[1] + dy, key[2] + dz];
                    for &i in self.cells.get(&cell).into_iter().flatten() {
                        let d = length(sub(self.positions[i as usize], p));
                        if d <= w && best.is_none_or(|(_, bd)| d < bd) {
                            best = Some((i, d));
                        }
                    }
                }
            }
        }
        if let Some((i, _)) = best {
            return i;
        }
        let i = self.positions.len() as u32;
        self.positions.push(p);
        self.cells.entry(key).or_default().push(i);
        i
    }

    pub fn add_polygon(&mut self, pts: &[Vec3], normals: Option<&[Vec3]>, uvs: Option<&[Vec2]>) {
        if pts.len() < 3 {
            return;
        }
        for [a, b, c] in triangulate(pts).tris {
            let ids = [self.point(pts[a]), self.point(pts[b]), self.point(pts[c])];
            if ids[0] == ids[1] || ids[1] == ids[2] || ids[0] == ids[2] {
                continue;
            }
            let (ab, ac) = (sub(pts[b], pts[a]), sub(pts[c], pts[a]));
            if length(cross(ab, ac)) <= 1e-12 * length(ab) * length(ac) {
                continue; // collinear: no usable normal
            }
            self.tris.push(ids);
            self.normals.push(normals.map(|n| [n[a], n[b], n[c]]));
            self.uvs.push(uvs.map(|t| [t[a], t[b], t[c]]));
            self.frame.push(self.frame_now);
            self.smooth.push(self.smooth_cos);
            self.oriented.push(self.oriented_now);
        }
    }

    /// Bounds of the vertices.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        bounds(&self.positions)
    }

    /// Indexed vertex streams, positions relative to `origin`.
    ///
    /// `projection`: when set, faces without stored UVs are projected using
    /// coordinates in meters (drawing units * `uv_scale`, per axis: the
    /// OFML geometry scale is mapped as part of the shape), or in their own
    /// frame when they have one ([`TriangleBuilder::set_frame`]; only when
    /// `projection` is set, i.e. the material has maps). `uv_matrix`: 2x3
    /// material transform applied to all UVs after glTF's v flip (as a
    /// KHR_texture_transform: same scale and offset, rotation conjugated).
    pub fn build(
        mut self,
        projection: Option<Projection>,
        uv_scale: Vec3,
        uv_matrix: Option<[f64; 6]>,
        origin: Vec3,
    ) -> MeshData {
        let mut repairs = Repairs::default();
        let face_of = |t: &[u32; 3]| face_normal(&self.positions, t);
        let pos = &self.positions;
        // welding can collapse slivers (e.g. at sphere poles), and so can
        // rounding to float32: drop them, and those float32 would turn over
        let f32_of = |i: u32| sub(pos[i as usize], origin).map(|c| c as f32 as f64);
        let keep: Vec<usize> = (0..self.tris.len())
            .filter(|&t| {
                let tri = self.tris[t];
                let n = face_of(&tri);
                let [a, b, c] = tri.map(f32_of);
                let n32 = cross(sub(b, a), sub(c, a));
                length(n) > 0.0 && dot(n, n32) > 0.0
            })
            .collect();
        if keep.len() < self.tris.len() {
            repairs.degenerate = self.tris.len() - keep.len();
            self.tris = keep.iter().map(|&t| self.tris[t]).collect();
            self.normals = keep.iter().map(|&t| self.normals[t]).collect();
            self.uvs = keep.iter().map(|&t| self.uvs[t]).collect();
            self.frame = keep.iter().map(|&t| self.frame[t]).collect();
            self.smooth = keep.iter().map(|&t| self.smooth[t]).collect();
            self.oriented = keep.iter().map(|&t| self.oriented[t]).collect();
        }
        // stored vertex normals say which side is the front
        let against: Vec<usize> = (0..self.tris.len())
            .filter(|&t| {
                self.normals[t].is_some_and(|given| {
                    let n = face_normal(&self.positions, &self.tris[t]);
                    given.iter().map(|&g| dot(normalize(g), n)).sum::<f64>() < 0.0
                })
            })
            .collect();
        for &t in &against {
            self.turn(t);
        }
        repairs.turned_to_normals = against.len();
        let (pieces, inverted) = self.orient();
        for &t in pieces.iter().chain(&inverted) {
            self.turn(t);
        }
        for &t in &inverted {
            if let Some(n) = self.normals[t].as_mut() {
                *n = n.map(|v| v.map(|c| -c));
            }
        }
        repairs.reoriented = pieces.len() + inverted.len();

        let face_of = |t: &[u32; 3]| face_normal(&self.positions, t);
        let pos = &self.positions;
        let face_n: Vec<Vec3> = self.tris.iter().map(face_of).collect();
        let unit_n: Vec<Vec3> = face_n.iter().map(|&n| normalize(n)).collect();
        let mut incident: Vec<Vec<usize>> = vec![vec![]; pos.len()];
        for (t, tri) in self.tris.iter().enumerate() {
            for &v in tri {
                incident[v as usize].push(t);
            }
        }

        let want_uv = projection.is_some() || self.uvs.iter().any(Option::is_some);
        let mut out = MeshData {
            positions: vec![],
            normals: vec![],
            uvs: want_uv.then(Vec::new),
            indices: vec![],
            repairs,
        };
        // position index, rounded normal and uv
        type VertexKey = (u32, [i64; 3], Option<[i64; 2]>);
        let mut seen: HashMap<VertexKey, u32> = HashMap::new();
        let normal_maps: Vec<Option<[[f64; 3]; 3]>> =
            self.frames.iter().map(UvFrame::normal_map).collect();
        for (t, tri) in self.tris.iter().enumerate() {
            let nt = unit_n[t];
            let smooth = self.smooth[t];
            // a frame of its own: the face's normal in that frame
            let framed = match (self.uvs[t], projection, self.frame[t]) {
                (None, Some(_), Some(f)) => normal_maps[f as usize].map(|nm| {
                    let f = self.frames[f as usize];
                    let n = normalize(nm.map(|row| dot(row, nt)));
                    let axis = match f.projection {
                        Projection::Axis(a) => a,
                        Projection::Box => dominant_axis(n),
                    };
                    (f, n, axis)
                }),
                _ => None,
            };
            let axis = match projection {
                Some(Projection::Axis(a)) => a,
                // the face's direction once scaled (normals scale inversely)
                _ => dominant_axis([0, 1, 2].map(|i| nt[i] / uv_scale[i])),
            };
            for (corner, &v) in tri.iter().enumerate() {
                let n = match self.normals[t] {
                    Some(given) => normalize(given[corner]),
                    None => {
                        let mut acc = ZERO;
                        for &s in &incident[v as usize] {
                            let d = dot(unit_n[s], nt);
                            if d >= smooth {
                                acc = add(acc, face_n[s]);
                            } else if d <= -smooth {
                                acc = sub(acc, face_n[s]);
                            }
                        }
                        normalize(acc)
                    }
                };
                let n = if n == ZERO { nt } else { n };
                let uv = want_uv.then(|| {
                    let mut uv = match (self.uvs[t], projection, framed) {
                        (Some(given), _, _) => given[corner],
                        (None, Some(_), Some((f, n, axis))) => {
                            project(apply(&f.to_frame, pos[v as usize]), n, axis, f.projection)
                        }
                        (None, Some(proj), None) => {
                            let p = pos[v as usize];
                            let scaled = std::array::from_fn(|k| p[k] * uv_scale[k]);
                            project(scaled, nt, axis, proj)
                        }
                        (None, None, _) => [0.0, 0.0],
                    };
                    // glTF's UV origin is top-left: flip v first, then
                    // apply the material transform (the transmatrix as a
                    // KHR_texture_transform: the same scale and offset,
                    // the rotation conjugated by the flip). Flipping last
                    // shifted a scaled texture by (1 - scale) mod 1.
                    uv = [uv[0], 1.0 - uv[1]];
                    if let Some(m) = uv_matrix {
                        uv = affine_apply(gltf_uv_affine(m), uv);
                    }
                    uv
                });
                let key = (
                    v,
                    n.map(|c| (c * 1e4).round_ties_even() as i64),
                    uv.map(|uv| uv.map(|c| (c * 1e6).round_ties_even() as i64)),
                );
                let next = out.positions.len() as u32;
                let idx = *seen.entry(key).or_insert(next);
                if idx == next {
                    out.positions.push(sub(pos[v as usize], origin));
                    out.normals.push(n);
                    if let (Some(list), Some(uv)) = (out.uvs.as_mut(), uv) {
                        list.push(uv);
                    }
                }
                out.indices.push(idx);
            }
        }
        out
    }

    /// Reverse triangle `t`.
    fn turn(&mut self, t: usize) {
        self.tris[t].swap(1, 2);
        if let Some(n) = self.normals[t].as_mut() {
            n.swap(1, 2);
        }
        if let Some(uv) = self.uvs[t].as_mut() {
            uv.swap(1, 2);
        }
    }

    /// Make the winding consistent within each edge-connected piece:
    /// neighbours across an edge shared by exactly two triangles must run
    /// that edge in opposite directions. A closed piece is turned to face
    /// outwards; an open one follows its triangles of known orientation
    /// (B-rep faces), else keeps the orientation of most of its area.
    /// Triangles with stored normals keep theirs (and decide for their
    /// piece), except where their normals do not face the way their
    /// neighbours' do: a face stored inside out, which is turned with its
    /// normals negated. (A folded triangle whose normals agree with its
    /// neighbours' keeps facing its normals.) Pieces that cannot be oriented (a
    /// Moebius-like strip) stay as they are. Returns the triangles to turn,
    /// and those to turn with negated normals.
    fn orient(&self) -> (Vec<usize>, Vec<usize>) {
        let n = self.tris.len();
        let mut edges: HashMap<(u32, u32), Vec<(usize, bool)>> = HashMap::new();
        for (t, tri) in self.tris.iter().enumerate() {
            for k in 0..3 {
                let (a, b) = (tri[k], tri[(k + 1) % 3]);
                edges
                    .entry((a.min(b), a.max(b)))
                    .or_default()
                    .push((t, a < b));
            }
        }
        // neighbours across manifold edges; `same`: the two run the edge the
        // same way, so one of them must turn
        let mut adj: Vec<Vec<(usize, bool)>> = vec![vec![]; n];
        let mut open = vec![false; n];
        for uses in edges.values() {
            if let [(t1, f1), (t2, f2)] = uses[..] {
                adj[t1].push((t2, f1 == f2));
                adj[t2].push((t1, f1 == f2));
            } else {
                for &(t, _) in uses {
                    open[t] = true;
                }
            }
        }
        let area = |tri: &[u32; 3]| {
            let [a, b, c] = tri.map(|i| self.positions[i as usize]);
            length(cross(sub(b, a), sub(c, a)))
        };
        let mut side = vec![u8::MAX; n]; // 0: keep, 1: turn (relative to the seed)
        let mut turned = vec![];
        let mut inverted = vec![];
        // the direction a triangle presents: its stored normals, else its face
        let facing = |t: usize| match self.normals[t] {
            Some(g) => normalize(g.iter().fold(ZERO, |a, &v| add(a, normalize(v)))),
            None => normalize(face_normal(&self.positions, &self.tris[t])),
        };
        for seed in 0..n {
            if side[seed] != u8::MAX {
                continue;
            }
            side[seed] = 0;
            let mut piece = vec![seed];
            let mut stack = vec![seed];
            let mut orientable = true;
            while let Some(t) = stack.pop() {
                for &(u, same) in &adj[t] {
                    let want = side[t] ^ u8::from(same);
                    if side[u] == u8::MAX {
                        side[u] = want;
                        piece.push(u);
                        stack.push(u);
                    } else if side[u] != want {
                        orientable = false;
                    }
                }
            }
            if !orientable {
                continue;
            }
            // the traversal follows `edges`' (random) hash order; the sums
            // below must not, or a tie comes out either way from run to run
            piece.sort_unstable();
            // which of the two sides keeps its winding
            let anchored: f64 = piece
                .iter()
                .filter(|&&t| self.normals[t].is_some())
                .map(|&t| {
                    if side[t] == 0 {
                        area(&self.tris[t])
                    } else {
                        -area(&self.tris[t])
                    }
                })
                .sum();
            let closed = piece.iter().all(|&t| !open[t]);
            let known: f64 = piece
                .iter()
                .filter(|&&t| self.oriented[t])
                .map(|&t| {
                    if side[t] == 0 {
                        area(&self.tris[t])
                    } else {
                        -area(&self.tris[t])
                    }
                })
                .sum();
            let keep_zero = if anchored != 0.0 {
                anchored > 0.0
            } else if !closed && known != 0.0 {
                known > 0.0
            } else if closed {
                // signed volume with side-0 triangles as they are (about a
                // point of the piece, for precision far from the origin)
                let o = self.positions[self.tris[seed][0] as usize];
                let vol: f64 = piece
                    .iter()
                    .map(|&t| {
                        let [a, b, c] = self.tris[t].map(|i| sub(self.positions[i as usize], o));
                        let v = dot(a, cross(b, c));
                        if side[t] == 0 { v } else { -v }
                    })
                    .sum();
                vol >= 0.0
            } else {
                // an open piece that wraps around something (a box of
                // 3DFACEs, a polyface missing a cap) faces away from its
                // centre; a (nearly) flat one keeps most of its area's side
                let (mut a0, mut a1) = (0.0, 0.0);
                let mut centre = ZERO;
                for &t in &piece {
                    let a = area(&self.tris[t]);
                    if side[t] == 0 {
                        a0 += a;
                    } else {
                        a1 += a;
                    }
                    let [p, q, r] = self.tris[t].map(|i| self.positions[i as usize]);
                    centre = add(centre, mul(add(add(p, q), r), a / 3.0));
                }
                let total = a0 + a1;
                let c = mul(centre, 1.0 / total);
                let vol: f64 = piece
                    .iter()
                    .map(|&t| {
                        let [a, b, d] = self.tris[t].map(|i| sub(self.positions[i as usize], c));
                        let v = dot(a, cross(b, d));
                        if side[t] == 0 { v } else { -v }
                    })
                    .sum();
                // |volume| / area^1.5 (areas here are doubled): about 0.07
                // for a closed cube, 0 for a flat sheet
                let wraps = vol.abs() / 6.0 / (total / 2.0).powf(1.5) >= OPEN_PIECE_WRAPS;
                if wraps { vol >= 0.0 } else { a0 >= a1 }
            };
            let turns = |t: usize| (side[t] == 1) == keep_zero;
            for &t in &piece {
                if !turns(t) {
                    continue;
                }
                if self.normals[t].is_none() {
                    // B-rep faces of an open piece stay as they are
                    if closed || !self.oriented[t] {
                        turned.push(t);
                    }
                    continue;
                }
                // a fold faces (nearly) the way its neighbours do
                let f = facing(t);
                let near: Vec<f64> = adj[t]
                    .iter()
                    .filter(|&&(u, _)| !turns(u))
                    .map(|&(u, _)| dot(f, facing(u)))
                    .collect();
                if near.is_empty() || near.iter().sum::<f64>() < 0.5 * near.len() as f64 {
                    inverted.push(t);
                }
            }
        }
        (turned, inverted)
    }
}

/// The material's 2x3 UV transform `m` (DWG transmatrix, row-major
/// [a b tx; c d ty]) as it acts on glTF's v-flipped UVs, written as a
/// KHR_texture_transform: scale and offset as they are, the rotation
/// conjugated by the flip.
pub fn gltf_uv_affine(m: [f64; 6]) -> [f64; 6] {
    [m[0], -m[1], m[2], -m[3], m[4], m[5]]
}

/// `m` applied to `p` (2x3 affine, row-major).
pub fn affine_apply(m: [f64; 6], p: Vec2) -> Vec2 {
    [
        m[0] * p[0] + m[1] * p[1] + m[2],
        m[3] * p[0] + m[4] * p[1] + m[5],
    ]
}

/// a · b for 2x3 affine maps (row-major).
pub fn affine_mul(a: [f64; 6], b: [f64; 6]) -> [f64; 6] {
    [
        a[0] * b[0] + a[1] * b[3],
        a[0] * b[1] + a[1] * b[4],
        a[0] * b[2] + a[1] * b[5] + a[2],
        a[3] * b[0] + a[4] * b[3],
        a[3] * b[1] + a[4] * b[4],
        a[3] * b[2] + a[4] * b[5] + a[5],
    ]
}

/// The inverse of a 2x3 affine map, None when it is singular.
pub fn affine_inverse(m: [f64; 6]) -> Option<[f64; 6]> {
    let det = m[0] * m[4] - m[1] * m[3];
    if det.abs() < 1e-15 || !det.is_finite() {
        return None;
    }
    let (a, b, c, d) = (m[4] / det, -m[1] / det, -m[3] / det, m[0] / det);
    Some([a, b, -(a * m[2] + b * m[5]), c, d, -(c * m[2] + d * m[5])])
}

fn face_normal(pos: &[Vec3], t: &[u32; 3]) -> Vec3 {
    let [a, b, c] = t.map(|i| pos[i as usize]);
    cross(sub(b, a), sub(c, a))
}

/// Lower and upper corner of the points.
pub fn bounds(pts: &[Vec3]) -> Option<(Vec3, Vec3)> {
    let first = *pts.first()?;
    Some(pts.iter().fold((first, first), |(lo, hi), p| {
        (
            std::array::from_fn(|k| lo[k].min(p[k])),
            std::array::from_fn(|k| hi[k].max(p[k])),
        )
    }))
}

#[derive(Default)]
pub struct LineBuilder {
    pub positions: Vec<Vec3>,
    pub indices: Vec<u32>,
}

impl LineBuilder {
    pub fn is_empty(&self) -> bool {
        self.indices.is_empty()
    }

    pub fn add_polyline(&mut self, pts: &[Vec3], closed: bool) {
        if pts.len() < 2 {
            return;
        }
        let base = self.positions.len() as u32;
        self.positions.extend_from_slice(pts);
        let n = pts.len() as u32;
        for i in 0..n - 1 {
            self.indices.extend([base + i, base + i + 1]);
        }
        if closed && n > 2 {
            self.indices.extend([base + n - 1, base]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The unit cube's side with outward normal `n`, wound to face outwards.
    fn side(n: Vec3) -> Vec<Vec3> {
        let k = dominant_axis(n);
        let (a, b) = ((k + 1) % 3, (k + 2) % 3);
        let c = if n[k] > 0.0 { 1.0 } else { 0.0 };
        let mut pts: Vec<Vec3> = [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
            .iter()
            .map(|&(s, t)| {
                let mut p = [0.0; 3];
                p[k] = c;
                p[a] = s;
                p[b] = t;
                p
            })
            .collect();
        if n[k] < 0.0 {
            pts.reverse();
        }
        pts
    }

    /// Output vertices of one side: positions and UVs.
    fn side_uvs(n: Vec3, projection: Projection, uv_matrix: Option<[f64; 6]>) -> MeshData {
        let mut b = TriangleBuilder::new(1e-9);
        b.add_polygon(&side(n), None, None);
        b.build(Some(projection), [1.0; 3], uv_matrix, ZERO)
    }

    /// How u changes along `dir` over the side's output vertices.
    fn du_along(n: Vec3, dir: Vec3, projection: Projection) -> f64 {
        let m = side_uvs(n, projection, None);
        let uv = m.uvs.unwrap();
        let along = |i: usize| dot(m.positions[i], dir);
        let lo = (0..uv.len())
            .min_by(|&i, &j| along(i).total_cmp(&along(j)))
            .unwrap();
        let hi = (0..uv.len())
            .max_by(|&i, &j| along(i).total_cmp(&along(j)))
            .unwrap();
        uv[hi][0] - uv[lo][0]
    }

    /// OMATS block mapping: seen from outside, U runs to the right on every
    /// vertical side (and along +X on top, -X below), so a texture is not
    /// mirrored on the back, left and bottom sides.
    #[test]
    fn box_mapping_mirrors_u_on_back_left_bottom() {
        let up = [0.0, 0.0, 1.0];
        for n in [
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
        ] {
            let right = cross(up, n);
            assert!(du_along(n, right, Projection::Box) > 0.5, "side {n:?}");
        }
        assert!(du_along([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], Projection::Box) > 0.5);
        assert!(du_along([0.0, 0.0, -1.0], [-1.0, 0.0, 0.0], Projection::Box) > 0.5);
        // a planar projection has one UV plane: no mirror on the far side
        assert!(du_along([0.0, 0.0, -1.0], [1.0, 0.0, 0.0], Projection::Axis(2)) > 0.5);
        assert!(du_along([0.0, 0.0, 1.0], [1.0, 0.0, 0.0], Projection::Axis(2)) > 0.5);
    }

    /// The material transform (DWG transmatrix = translate * rotate * scale)
    /// acts as a KHR_texture_transform on the v-flipped UVs: offset and
    /// scale as they are, rotation [[c, s], [-s, c]] in glTF's UV space.
    #[test]
    fn material_transform_after_the_v_flip() {
        for (deg, sx, sy, tx, ty) in [
            (0.0f64, 1.5, 1.5, 0.0, 0.0),
            (0.0, 1.5, 2.5, -0.25, 0.3),
            (90.0, 2.0, 1.0, 0.1, 0.0),
            (30.0, 8.0, 8.0, 0.0, 0.5),
        ] {
            let (s, c) = deg.to_radians().sin_cos();
            let m = [c * sx, -s * sy, tx, s * sx, c * sy, ty];
            let data = side_uvs([0.0, 0.0, 1.0], Projection::Axis(2), Some(m));
            for (p, uv) in data.positions.iter().zip(data.uvs.unwrap()) {
                // planar along Z: raw (u, v) = (x, y); glTF flips v
                let (u, v) = (p[0], 1.0 - p[1]);
                let (a, b) = (sx * u, sy * v);
                let want = [c * a + s * b + tx, -s * a + c * b + ty];
                assert!(
                    (uv[0] - want[0]).abs() < 1e-9 && (uv[1] - want[1]).abs() < 1e-9,
                    "{deg} {sx} {sy}: {uv:?} != {want:?}"
                );
            }
        }
    }

    /// A face in its own frame (a material mapper): projected in the
    /// frame's unit coordinates, ignoring the unit scale, before the v flip
    /// and the material transform; stored UVs still win.
    fn shifted_frame(x: f64) -> UvFrame {
        let mut to_frame = crate::geom::IDENTITY;
        to_frame[3] = x;
        UvFrame {
            projection: Projection::Axis(2),
            to_frame,
        }
    }

    #[test]
    fn equal_frames_are_kept_once() {
        let mut b = TriangleBuilder::new(1e-9);
        let (a, c) = (shifted_frame(1.0), shifted_frame(2.0));
        let mut minus_zero = a;
        minus_zero.to_frame[1] = -0.0;
        let nan = shifted_frame(f64::NAN);
        let mut seen = vec![];
        for f in [a, c, minus_zero, a, nan, nan, c] {
            b.set_frame(Some(f));
            seen.push(b.frame_now.unwrap());
        }
        // -0.0 == 0.0; NaN equals nothing, not even itself
        assert_eq!(seen, [0, 1, 0, 0, 2, 3, 1]);
        assert_eq!(b.frames.len(), 4);
        b.set_frame(None);
        assert_eq!(b.frame_now, None);
    }

    /// Each entity may bring a frame of its own: four times the frames may
    /// take about four times as long, not sixteen.
    #[test]
    fn many_frames_scale_linearly() {
        let square = side([0.0, 0.0, 1.0]);
        let time = |n: usize| {
            (0..3)
                .map(|_| {
                    let start = std::time::Instant::now();
                    let mut b = TriangleBuilder::new(1e-9);
                    for k in 0..n {
                        b.set_frame(Some(shifted_frame(k as f64)));
                        b.add_polygon(&square, None, None);
                    }
                    assert_eq!(b.frames.len(), n);
                    start.elapsed().as_secs_f64()
                })
                .fold(f64::INFINITY, f64::min)
        };
        let n = 20_000;
        let (small, large) = (time(n), time(4 * n));
        assert!(
            large < 8.0 * small,
            "{n} frames: {small:.3} s, {} frames: {large:.3} s",
            4 * n
        );
    }

    #[test]
    fn faces_with_a_frame_project_in_it() {
        // a 170 x 240 rug top at z = 1, the frame spanning it once
        let rug = [
            [-85.0, -120.0, 1.0],
            [85.0, -120.0, 1.0],
            [85.0, 120.0, 1.0],
            [-85.0, 120.0, 1.0],
        ];
        let mapper = crate::mapper::MaterialMapper {
            projection: crate::mapper::BOX,
            tiling: 1,
            auto_transform: 1,
            origin: [-85.0, -120.0, 0.0],
            axes_ends: [
                [85.0, -120.0, 0.0],
                [-85.0, 120.0, 0.0],
                [-85.0, -120.0, 100.0],
            ],
        };
        let frame = UvFrame {
            projection: Projection::Box,
            to_frame: mapper.frame().unwrap(),
        };
        let uvs_of = |frame: Option<UvFrame>, stored: Option<&[Vec2]>, m: Option<[f64; 6]>| {
            let mut b = TriangleBuilder::new(1e-9);
            b.set_frame(frame);
            b.add_polygon(&rug, None, stored);
            let data = b.build(Some(Projection::Box), [0.001; 3], m, ZERO);
            data.positions
                .iter()
                .zip(data.uvs.unwrap())
                .map(|(p, uv)| (*p, uv))
                .collect::<Vec<_>>()
        };
        let near = |a: Vec2, b: Vec2| (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9;
        for (p, uv) in uvs_of(Some(frame), None, None) {
            let want = [(p[0] + 85.0) / 170.0, 1.0 - (p[1] + 120.0) / 240.0];
            assert!(near(uv, want), "{p:?}: {uv:?} != {want:?}");
        }
        // the material transform on top, after the flip
        let m = [4.0, 0.0, 0.5, 0.0, 2.0, 0.0];
        for (p, uv) in uvs_of(Some(frame), None, Some(m)) {
            let raw = [(p[0] + 85.0) / 170.0, 1.0 - (p[1] + 120.0) / 240.0];
            let want = [4.0 * raw[0] + 0.5, 2.0 * raw[1]];
            assert!(near(uv, want), "{p:?}: {uv:?} != {want:?}");
        }
        // without the frame: meters (0.001 per unit) as before
        for (p, uv) in uvs_of(None, None, None) {
            assert!(
                near(uv, [p[0] * 0.001, 1.0 - p[1] * 0.001]),
                "{p:?}: {uv:?}"
            );
        }
        // stored UVs win over the frame
        let stored = [[0.0, 0.0], [3.0, 0.0], [3.0, 3.0], [0.0, 3.0]];
        let got = uvs_of(Some(frame), Some(&stored), None);
        assert!(got.iter().all(|(_, uv)| [0.0, 3.0].contains(&uv[0])));
    }

    /// Box projection in a rotated frame picks the side by the normal in
    /// that frame and mirrors U on the frame's back, left and bottom.
    #[test]
    fn box_frame_sides_follow_the_frame() {
        // the frame's X along the block's Y, its Y along -X: a quarter turn
        let frame = UvFrame {
            projection: Projection::Box,
            to_frame: [
                0.0, 1.0, 0.0, 0.0, -1.0, 0.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
            ],
        };
        let up = [0.0, 0.0, 1.0];
        for n in [
            [1.0, 0.0, 0.0],
            [-1.0, 0.0, 0.0],
            [0.0, 1.0, 0.0],
            [0.0, -1.0, 0.0],
        ] {
            let mut b = TriangleBuilder::new(1e-9);
            b.set_frame(Some(frame));
            b.add_polygon(&side(n), None, None);
            let m = b.build(Some(Projection::Axis(2)), [1.0; 3], None, ZERO);
            let uv = m.uvs.unwrap();
            let right = cross(up, n);
            let along = |i: usize| dot(m.positions[i], right);
            let lo = (0..uv.len())
                .min_by(|&i, &j| along(i).total_cmp(&along(j)))
                .unwrap();
            let hi = (0..uv.len())
                .max_by(|&i, &j| along(i).total_cmp(&along(j)))
                .unwrap();
            // a vertical side: U runs to the right seen from outside, V up
            assert!(uv[hi][0] - uv[lo][0] > 0.5, "side {n:?}");
            assert!(
                m.positions
                    .iter()
                    .zip(&uv)
                    .all(|(p, t)| (t[1] - (1.0 - p[2])).abs() < 1e-9)
            );
        }
    }
}
