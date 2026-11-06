// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Triangulate B-rep faces.
//!
//! Per face: sample the boundary edges in 3D (shared edges are sampled once,
//! so neighbouring faces stay watertight), map the loops into the surface's
//! uv domain, close loops that run around a periodic seam (cylinder bands,
//! sphere caps), triangulate the uv polygon with its holes, then split
//! interior edges until the surface deviates less than the tolerance from
//! the triangles. Faces whose surface cannot be evaluated fall back to a
//! Coons patch over the boundary (3/4-sided blends) or a flat triangulation
//! of the boundary.

use std::collections::{HashMap, HashSet};

use super::brep::{Body, Curve, Edge, Face, Loop, Surface, SurfaceDef, best_pipe, best_spline};
use crate::geom::{
    Vec2, Vec3, ZERO, add, cross, dist2, dot, length, mul, newell_normal, normalize, sub,
    triangulate_2d, triangulate_2d_flat,
};
use crate::limits::MAX_TRIANGLES_PER_FACE;

const RELATIVE_DEVIATION: f64 = 1e-3;

/// Refinement rounds in which the largest deviation must at least halve.
/// On a smooth surface every round quarters it (halves it at a crease); a
/// face whose uv triangles do not follow its surface (a parameterisation
/// that folds or jumps) keeps the same deviation however often it is split.
const STALL_ROUNDS: usize = 8;

/// Corners of a curved face's uv boundary whose sine (in the surface's
/// metric) is at most this count as straight when the face is ear-clipped:
/// no ear tips, no diagonal along them. A straight boundary run in uv (a
/// spline's domain edge, curved on the surface) carries rounding noise of
/// about 1e-9; clipped by its sign it became fans of zero-area uv
/// triangles, on the surface fins folding across the boundary curve and
/// showing as gaps. A sampled arc's corners stay far above it (the sine of
/// the angle step, 1.5e-3 at 4096 steps per turn).
const FLAT_CORNER: f64 = 1e-5;

pub type Tri = [Vec3; 3];

/// A loop in the uv domain with its 3D points.
type UvPolygon = (Vec<Vec2>, Vec<Vec3>);

/// A loop that wraps around a periodic direction: uv, xyz, wraps in (u, v).
type WrapLoop<'a> = (&'a [Vec2], &'a [Vec3], (bool, bool));

/// A face that cannot be tessellated (where the reference implementation
/// hits a math error).
#[derive(Debug)]
pub struct Failed;

/// Python's round(): NaN and infinities are errors.
fn round(x: f64) -> Result<f64, Failed> {
    if x.is_finite() {
        Ok(x.round_ties_even())
    } else {
        Err(Failed)
    }
}

pub struct Tessellator {
    base_tol: f64,
    tol: f64,
    edge_pts: HashMap<usize, Vec<Vec3>>,
    /// Faces that produced no triangles (a math error).
    pub failed_faces: usize,
    /// Faces filled from their boundary only: no usable surface, or a seam
    /// layout the uv mapping does not handle.
    pub approximated_faces: usize,
    /// Faces whose boundary polygon could not be ear-clipped cleanly.
    pub degraded_faces: usize,
    /// Faces whose refinement hit [`MAX_TRIANGLES_PER_FACE`] or stopped
    /// converging ([`STALL_ROUNDS`]) before reaching the tolerance.
    pub capped_faces: usize,
    /// Per triangle of the last [`Tessellator::body`]: the surface's unit
    /// normals at its corners (None for faces without a surface).
    pub normals: Vec<Option<[Vec3; 3]>>,
    /// The same for the last [`Tessellator::face`].
    face_normals: Option<Vec<[Vec3; 3]>>,
    /// Triangles [`Tessellator::body`] may return; a body that needs more
    /// is cut short and `over_budget` set.
    pub budget: usize,
    pub over_budget: bool,
}

/// What the uv mapping of a face gave.
enum UvFace {
    Tris(Vec<Tri>),
    /// The loops cannot be closed in the uv domain.
    Unmapped,
}

impl Tessellator {
    pub fn new(tolerance: f64) -> Self {
        Tessellator {
            base_tol: tolerance,
            tol: tolerance,
            edge_pts: HashMap::new(),
            failed_faces: 0,
            approximated_faces: 0,
            degraded_faces: 0,
            capped_faces: 0,
            normals: vec![],
            face_normals: None,
            budget: usize::MAX,
            over_budget: false,
        }
    }

    // -- edges -------------------------------------------------------------------
    fn edge_points(&mut self, edge: &Edge) -> Vec<Vec3> {
        if let Some(pts) = self.edge_pts.get(&edge.index) {
            return pts.clone();
        }
        let pts = self.sample_edge(edge);
        self.edge_pts.insert(edge.index, pts.clone());
        pts
    }

    fn sample_edge(&self, e: &Edge) -> Vec<Vec3> {
        let Some(curve) = &e.curve else {
            return vec![e.start, e.end];
        };
        if matches!(curve, Curve::Straight { .. }) {
            // inner points would only be collinear: zero-area triangles that
            // leave T-junctions (cracks) once they are dropped
            return vec![e.start, e.end];
        }
        let Some((f, a, b)) = self.edge_param_fn(e, curve) else {
            return vec![e.start, e.end];
        };
        let mut pts = vec![e.start];
        self.adaptive(&*f, a, b, f(a), f(b), &mut pts, 0);
        *pts.last_mut().unwrap() = e.end;
        pts
    }

    /// The edge as `f(t)` for t in a..b. ACIS negates the curve parameter of
    /// edges whose sense is reversed; validated against the vertex positions,
    /// taking the sign that fits them best.
    #[allow(clippy::type_complexity)]
    fn edge_param_fn<'c>(
        &self,
        e: &Edge,
        c: &'c Curve,
    ) -> Option<(Box<dyn Fn(f64) -> Vec3 + 'c>, f64, f64)> {
        let tol = (self.tol * 20.0).max(1e-9 + 1e-6 * length(sub(e.start, e.end)));
        let err = |sign: f64| {
            length(sub(c.eval(sign * e.t0), e.start)).max(length(sub(c.eval(sign * e.t1), e.end)))
        };
        // the sense decides; the other sign only when it fits clearly better
        // (both fit a half circle from angle -pi, but trace different halves)
        let preferred = if e.reversed { -1.0 } else { 1.0 };
        let (e1, e2) = (err(preferred), err(-preferred));
        let sign = if e2 < e1 - 1e-3 * tol {
            -preferred
        } else {
            preferred
        };
        if e1.min(e2) <= tol {
            return Some((Box::new(move |t| c.eval(sign * t)), e.t0, e.t1));
        }
        // bs3 approximations may use their own parameter range: project the vertices
        if let Curve::Spline(bs) = c {
            let a = project(c, bs.t0, bs.t1, e.start);
            let span = bs.t1 - bs.t0;
            if e.start == e.end && span > 0.0 {
                // closed: once around, starting at the vertex
                let dir = if e.reversed { -1.0 } else { 1.0 };
                let f = move |t: f64| c.eval(bs.t0 + (t - bs.t0).rem_euclid(span));
                return Some((Box::new(f), a, a + dir * span));
            }
            let b = project(c, bs.t0, bs.t1, e.end);
            return Some((Box::new(move |t| c.eval(t)), a, b));
        }
        None
    }

    #[allow(clippy::too_many_arguments)]
    fn adaptive(
        &self,
        f: &dyn Fn(f64) -> Vec3,
        a: f64,
        b: f64,
        pa: Vec3,
        pb: Vec3,
        out: &mut Vec<Vec3>,
        depth: u32,
    ) {
        let m = (a + b) / 2.0;
        let pm = f(m);
        let (q1, q3) = (f((a + m) / 2.0), f((m + b) / 2.0));
        let mid = mul(add(pa, pb), 0.5);
        let dev = length(sub(pm, mid))
            .max(seg_dist(q1, pa, pm))
            .max(seg_dist(q3, pm, pb));
        if depth < 3 || (dev > self.tol && depth < 12) {
            self.adaptive(f, a, m, pa, pm, out, depth + 1);
            self.adaptive(f, m, b, pm, pb, out, depth + 1);
        } else {
            out.push(pb);
        }
    }

    // -- faces -------------------------------------------------------------------

    /// Triangles in drawing units; the tolerance applies after the body
    /// transform (some exporters write bodies in mm with a 0.001 scale).
    pub fn body(&mut self, body: &Body) -> Vec<Tri> {
        let scale = match body.transform {
            Some((_, _, s)) if s != 0.0 => s.abs(),
            _ => 1.0,
        };
        // absolute deviation, but never finer than 0.1 % of the body size
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        let mut any = false;
        for p in body
            .faces
            .iter()
            .flat_map(|f| &f.loops)
            .flatten()
            .map(|(e, _)| e.start)
        {
            any = true;
            for i in 0..3 {
                lo[i] = lo[i].min(p[i]);
                hi[i] = hi[i].max(p[i]);
            }
        }
        let diag = if any { length(sub(hi, lo)) } else { 0.0 };
        self.tol = (self.base_tol / scale).max(RELATIVE_DEVIATION * diag);
        self.edge_pts.clear();
        let mut tris = vec![];
        let mut normals = vec![];
        for face in &body.faces {
            if tris.len() > self.budget {
                self.over_budget = true;
                break;
            }
            match self.face(face) {
                // a math error in the reference implementation shows up as NaN here
                Ok(t) if t.iter().flatten().flatten().all(|c| c.is_finite()) => {
                    match self.face_normals.take() {
                        Some(n) if n.iter().flatten().flatten().all(|c| c.is_finite()) => {
                            normals.extend(n.into_iter().map(Some))
                        }
                        _ => normals.extend(t.iter().map(|_| None)),
                    }
                    tris.extend(t);
                }
                _ => self.failed_faces += 1,
            }
        }
        if tris.len() > self.budget {
            self.over_budget = true;
        }
        if let Some((rot, trans, s)) = body.transform {
            // a mirroring transform turns every face inside out
            let det = rot[0] * (rot[4] * rot[8] - rot[5] * rot[7])
                - rot[1] * (rot[3] * rot[8] - rot[5] * rot[6])
                + rot[2] * (rot[3] * rot[7] - rot[4] * rot[6]);
            if det * s * s * s < 0.0 {
                for t in &mut tris {
                    t.swap(1, 2);
                }
                for n in normals.iter_mut().flatten() {
                    n.swap(1, 2);
                }
            }
            // rotation (or mirroring) only: normals keep facing out
            let xn = |[x, y, z]: Vec3| -> Vec3 {
                normalize(mul(
                    [
                        x * rot[0] + y * rot[3] + z * rot[6],
                        x * rot[1] + y * rot[4] + z * rot[7],
                        x * rot[2] + y * rot[5] + z * rot[8],
                    ],
                    s.signum(),
                ))
            };
            for n in normals.iter_mut().flatten() {
                *n = n.map(xn);
            }
            let xf = |[x, y, z]: Vec3| -> Vec3 {
                [
                    s * (x * rot[0] + y * rot[3] + z * rot[6]) + trans[0],
                    s * (x * rot[1] + y * rot[4] + z * rot[7]) + trans[1],
                    s * (x * rot[2] + y * rot[5] + z * rot[8]) + trans[2],
                ]
            };
            for t in &mut tris {
                *t = t.map(xf);
            }
        }
        self.normals = normals;
        tris
    }

    fn loop_points(&mut self, lp: &Loop) -> Vec<Vec3> {
        let mut pts: Vec<Vec3> = vec![];
        for (edge, rev) in lp {
            let mut seg = self.edge_points(edge);
            if *rev {
                seg.reverse();
            }
            let skip = usize::from(pts.last().is_some_and(|&last| dist2(last, seg[0]) < 1e-24));
            pts.extend_from_slice(&seg[skip..]);
        }
        if pts.len() > 1 && dist2(pts[0], pts[pts.len() - 1]) < 1e-24 {
            pts.pop();
        }
        pts
    }

    /// Triangles wound counter-clockwise around the face's outward normal.
    /// Coedge loops run counter-clockwise around it (the face lies to their
    /// left), so the boundary-only fallbacks keep the loop order; the uv
    /// triangulation follows the surface's parametric normal Su x Sv and is
    /// flipped where the face or its surface is reversed.
    pub fn face(&mut self, face: &Face) -> Result<Vec<Tri>, Failed> {
        self.face_normals = None;
        let all_loops: Vec<Vec<Vec3>> = face.loops.iter().map(|l| self.loop_points(l)).collect();
        // single-vertex loops (a cone apex) take no part in the triangulation
        let loops: Vec<Vec<Vec3>> = all_loops.iter().filter(|l| l.len() >= 2).cloned().collect();
        let mut surf = match &face.surface {
            None => None,
            Some(SurfaceDef::Analytic(s)) => Some(s.clone()),
            Some(SurfaceDef::SplineChoice(cands)) => {
                // procedural surfaces: use the embedded NURBS only if it really
                // passes through this face's boundary
                let all: Vec<Vec3> = loops.iter().flatten().copied().collect();
                let (best, err) = best_spline(cands, &all);
                let limit = (10.0 * self.tol).max(1e-9);
                best.filter(|_| err <= limit || err.is_nan())
                    .map(Surface::Spline)
            }
            Some(SurfaceDef::PipeChoice(spines)) => {
                let all: Vec<Vec3> = loops.iter().flatten().copied().collect();
                let (best, err) = best_pipe(spines, &all);
                let limit = (10.0 * self.tol).max(1e-9);
                best.filter(|_| err <= limit).map(Surface::Pipe)
            }
        };
        let Some(surf) = surf.as_mut() else {
            self.approximated_faces += 1;
            return self.fallback(face, &loops);
        };
        if let Surface::Cone(cone) = surf {
            // including the apex vertex, which decides the sign
            cone.fit_slope_sign(&all_loops.concat(), self.tol);
        }
        // a pipe's normal convention is not in the file: orient by the loops
        let pipe = matches!(surf, Surface::Pipe(_));
        let flip = !pipe && face.reversed != face.surface_reversed;
        let mut tris = if loops.is_empty() {
            self.full_surface(surf)?
        } else {
            match self.uv_face(surf, &loops, flip)? {
                UvFace::Tris(t) => t,
                UvFace::Unmapped => {
                    self.approximated_faces += 1;
                    return Ok(self.fallback_loops(&loops));
                }
            }
        };
        let turned = flip || (pipe && against_loops(&tris, &loops));
        if turned {
            for t in &mut tris {
                t.swap(1, 2);
            }
        }
        self.face_normals = Some(surface_normals(surf, &tris, turned));
        Ok(tris)
    }

    /// Triangles wound around Su x Sv. `flip`: the face lies on the side of
    /// -(Su x Sv), which decides where seam-wrapping loops are closed.
    fn uv_face(
        &mut self,
        surf: &Surface,
        loops: &[Vec<Vec3>],
        flip: bool,
    ) -> Result<UvFace, Failed> {
        let (pu, pv) = (surf.period_u(), surf.period_v());
        let mut uv_loops = vec![];
        let mut wraps = vec![];
        let mut xyz_loops: Vec<Vec<Vec3>> = vec![];
        for pts in loops {
            let (uv, pts) = map_loop(surf, pts)?;
            xyz_loops.push(pts);
            let (first, last) = (uv[0], uv[uv.len() - 1]);
            let du = last[0] - first[0] + wrap_step(last[0], first[0], pu)?;
            let dv = last[1] - first[1] + wrap_step(last[1], first[1], pv)?;
            wraps.push((
                pu != 0.0 && du.abs() > pu / 2.0,
                pv != 0.0 && dv.abs() > pv / 2.0,
            ));
            uv_loops.push(uv);
        }
        let loops = &xyz_loops;

        let mut polys: Vec<UvPolygon> = vec![];
        let wrapping: Vec<usize> = (0..wraps.len())
            .filter(|&i| wraps[i].0 || wraps[i].1)
            .collect();
        // where a seam can be cut without running through a loop that does
        // not wrap (a hole in a band or cap), per periodic direction
        let others: Vec<&[Vec2]> = (0..uv_loops.len())
            .filter(|i| !wrapping.contains(i))
            .map(|i| uv_loops[i].as_slice())
            .collect();
        let gaps = [0, 1].map(|k| {
            let spans: Vec<(f64, f64)> = others.iter().map(|uv| range(uv, k)).collect();
            free_gap(&spans, [pu, pv][k])
        });
        if !wrapping.is_empty() {
            let items: Vec<WrapLoop> = wrapping
                .iter()
                .map(|&i| (uv_loops[i].as_slice(), loops[i].as_slice(), wraps[i]))
                .collect();
            match self.close_wrapping(surf, &items, flip, gaps)? {
                Some(band) => polys.push(band),
                None => return Ok(UvFace::Unmapped),
            }
        } else if !others.is_empty()
            && others
                .iter()
                .all(|uv| (if flip { -area(uv) } else { area(uv) }) < 0.0)
        {
            // every loop runs round a hole (the face lies to its left): the
            // face is the whole closed surface but for them, like a sphere
            // a cylinder runs through
            if let Some(domain) = self.full_domain(surf, gaps) {
                polys.push(domain);
            }
        }
        for i in (0..uv_loops.len()).filter(|i| !wrapping.contains(i)) {
            polys.push((uv_loops[i].clone(), loops[i].clone()));
        }

        // outer boundary = largest |area|; others are holes (shifted by periods into it)
        polys.sort_by(|a, b| area(&b.0).abs().total_cmp(&area(&a.0).abs()));
        let mut polys = polys.into_iter();
        let (mut outer_uv, mut outer_xyz) = polys.next().ok_or(Failed)?;
        if area(&outer_uv) < 0.0 {
            outer_uv.reverse();
            outer_xyz.reverse();
        }
        let mut holes_uv = vec![];
        let mut xyz_all = outer_xyz;
        for (uv, mut xyz) in polys {
            let mut uv = shift_into(&uv, &outer_uv, pu, pv)?;
            if area(&uv) > 0.0 {
                uv.reverse();
                xyz.reverse();
            }
            holes_uv.push(uv);
            xyz_all.extend(xyz);
        }
        let (uv_all, mut tri) = triangulate_2d(&outer_uv, &holes_uv);
        if let Some(l) = stretched(surf)
            .then(|| uv_metric(surf, &outer_uv))
            .flatten()
        {
            // again in the surface's metric, with straight runs kept out of
            // ears and diagonals (the triangles index the same rings); taken
            // where it covers the polygon at least as fully
            let map = |ring: &[Vec2]| -> Vec<Vec2> { ring.iter().map(|&q| l(q)).collect() };
            let holes: Vec<Vec<Vec2>> = holes_uv.iter().map(|h| map(h)).collect();
            let (_, flat) = triangulate_2d_flat(&map(&outer_uv), &holes, FLAT_CORNER);
            if !flat.degraded && flat.tris.len() >= tri.tris.len() {
                tri = flat;
            }
        }
        if tri.degraded {
            self.degraded_faces += 1;
        }
        let tri_idx = tri.tris;
        let mut boundary = HashSet::new();
        let mut start = 0;
        for ring in std::iter::once(&outer_uv).chain(&holes_uv) {
            let n = ring.len();
            for k in 0..n {
                let (a, b) = (start + k, start + (k + 1) % n);
                boundary.insert((a.min(b), a.max(b)));
            }
            start += n;
        }
        if let Surface::Plane(_) = surf {
            // no refinement, but Delaunay flips remove the slivers ear clipping leaves
            let tris = delaunay_flips(&uv_all, None, tri_idx, &boundary);
            return Ok(UvFace::Tris(
                tris.iter().map(|t| t.map(|i| xyz_all[i])).collect(),
            ));
        }
        Ok(UvFace::Tris(
            self.refine(surf, uv_all, xyz_all, tri_idx, &boundary, 0),
        ))
    }

    /// Turn loops that run around the seam into one closed uv polygon: two
    /// loops bound a band, one loop is closed over the pole on the face's
    /// side. Loops that wrap in v (a torus elbow) are handled with u and v
    /// swapped.
    fn close_wrapping(
        &mut self,
        surf: &Surface,
        items: &[WrapLoop],
        flip: bool,
        gaps: [Option<(f64, f64)>; 2],
    ) -> Result<Option<UvPolygon>, Failed> {
        let swap = if items.iter().all(|w| w.2.0 && !w.2.1) {
            false
        } else if items.iter().all(|w| w.2.1 && !w.2.0) {
            true
        } else {
            return Ok(None);
        };
        let sw = |q: Vec2| if swap { [q[1], q[0]] } else { q };
        let (p, p_other) = if swap {
            (surf.period_v(), surf.period_u())
        } else {
            (surf.period_u(), surf.period_v())
        };
        // A loop running towards +u has the face on its left, at larger v,
        // unless the face lies on the -(Su x Sv) side; swapping u and v
        // mirrors the domain, which swaps the sides once more.
        let side_flip = flip != swap;
        let gap = gaps[usize::from(swap)];
        let loops: Vec<(Vec<Vec2>, &[Vec3])> = items
            .iter()
            .map(|(uv, xyz, _)| (uv.iter().map(|&q| sw(q)).collect(), *xyz))
            .collect();
        let eval = |q: Vec2| {
            let q = sw(q);
            surf.eval(q[0], q[1])
        };
        let closed = match loops.as_slice() {
            [(a_uv, a_xyz), (b_uv, b_xyz)] => {
                let uc = cut_u(a_uv, gap, p);
                let (Some(mut a), Some(mut b)) =
                    (cut_at(a_uv, a_xyz, uc, p)?, cut_at(b_uv, b_xyz, uc, p)?)
                else {
                    return Ok(None);
                };
                if p_other != 0.0 {
                    // periodic across too (torus): B on A's face side, less
                    // than one period away
                    let up = (a.0[a.0.len() - 1][0] > a.0[0][0]) != side_flip;
                    let mean = |q: &[Vec2]| q.iter().map(|x| x[1]).sum::<f64>() / q.len() as f64;
                    let (va, vb) = (mean(&a.0), mean(&b.0));
                    let d = (vb - va).rem_euclid(p_other);
                    let target = if up { d } else { d - p_other };
                    for q in &mut b.0 {
                        q[1] += va + target - vb;
                    }
                }
                // A runs uc -> uc+P, B back uc+P -> uc
                if a.0[a.0.len() - 1][0] < a.0[0][0] {
                    a.0.reverse();
                    a.1.reverse();
                }
                if b.0[b.0.len() - 1][0] > b.0[0][0] {
                    b.0.reverse();
                    b.1.reverse();
                }
                // B must run back from where A ends (uc + P) to uc
                let shift = round((a.0[a.0.len() - 1][0] - b.0[0][0]) / p)? * p;
                let b_uv: Vec<Vec2> = b.0.iter().map(|q| [q[0] + shift, q[1]]).collect();
                let (mut uv, mut xyz) = a;
                self.seam(&eval, uv[uv.len() - 1], b_uv[0], &mut uv, &mut xyz, 0);
                uv.extend(b_uv);
                xyz.extend(b.1);
                self.seam(&eval, uv[uv.len() - 1], uv[0], &mut uv, &mut xyz, 0);
                (uv, xyz)
            }
            [(uv, xyz)] => {
                let uc = cut_u(uv, gap, p);
                let Some((mut auv, mut axyz)) = cut_at(uv, xyz, uc, p)? else {
                    return Ok(None);
                };
                let up = (auv[auv.len() - 1][0] > auv[0][0]) != side_flip;
                if auv[auv.len() - 1][0] < auv[0][0] {
                    auv.reverse();
                    axyz.reverse();
                }
                // close over the pole/apex on the face's side
                let pole = |upper: bool| {
                    if swap {
                        surf.pole_u(upper)
                    } else {
                        surf.pole_v(upper)
                    }
                };
                let pole = match (pole(up), pole(!up)) {
                    (Some(v), _) => v,
                    (None, Some(v)) => {
                        // no pole where the face should be: keep the shape
                        // closed, but report it
                        self.approximated_faces += 1;
                        v
                    }
                    (None, None) => return Ok(None),
                };
                // along a meridian to the pole, across it, back down
                let [end, start] = [[auv[auv.len() - 1][0], pole], [auv[0][0], pole]];
                self.seam(&eval, auv[auv.len() - 1], end, &mut auv, &mut axyz, 0);
                for q in [end, start] {
                    auv.push(q);
                    axyz.push(eval(q));
                }
                self.seam(&eval, start, auv[0], &mut auv, &mut axyz, 0);
                (auv, axyz)
            }
            _ => return Ok(None),
        };
        let (uv, xyz) = closed;
        Ok(Some((uv.into_iter().map(sw).collect(), xyz)))
    }

    /// Append the points strictly between uv points `a` and `b` that make
    /// the straight uv segment follow the surface within the tolerance (seam
    /// cuts and pole meridians, which are face boundary but no edge).
    #[allow(clippy::too_many_arguments)]
    fn seam(
        &self,
        eval: &dyn Fn(Vec2) -> Vec3,
        a: Vec2,
        b: Vec2,
        uv: &mut Vec<Vec2>,
        xyz: &mut Vec<Vec3>,
        depth: u32,
    ) {
        let m = [(a[0] + b[0]) / 2.0, (a[1] + b[1]) / 2.0];
        let pm = eval(m);
        if depth < 12 && length(sub(pm, mul(add(eval(a), eval(b)), 0.5))) > self.tol {
            self.seam(eval, a, m, uv, xyz, depth + 1);
            uv.push(m);
            xyz.push(pm);
            self.seam(eval, m, b, uv, xyz, depth + 1);
        }
    }

    /// The whole domain of a closed surface (periodic in u, and periodic in
    /// v or closed by poles) as one uv polygon, cut open along the middle of
    /// the free gaps so no hole crosses its border.
    fn full_domain(&self, surf: &Surface, gaps: [Option<(f64, f64)>; 2]) -> Option<UvPolygon> {
        let (pu, pv) = (surf.period_u(), surf.period_v());
        let mid = |g: Option<(f64, f64)>| g.map(|(lo, hi)| (lo + hi) / 2.0);
        if pu == 0.0 {
            return None;
        }
        let u0 = mid(gaps[0])?;
        let (v0, v1) = if pv != 0.0 {
            let v0 = mid(gaps[1])?;
            (v0, v0 + pv)
        } else {
            (surf.pole_v(false)?, surf.pole_v(true)?)
        };
        let corners = [[u0, v0], [u0 + pu, v0], [u0 + pu, v1], [u0, v1]];
        let eval = |q: Vec2| surf.eval(q[0], q[1]);
        let (mut uv, mut xyz) = (vec![], vec![]);
        for k in 0..4 {
            let (a, b) = (corners[k], corners[(k + 1) % 4]);
            uv.push(a);
            xyz.push(eval(a));
            // meridians and seams; a side along a pole stays one edge
            self.seam(&eval, a, b, &mut uv, &mut xyz, 0);
        }
        Some((uv, xyz))
    }

    /// A face without loops covers the whole (closed) surface.
    fn full_surface(&mut self, surf: &Surface) -> Result<Vec<Tri>, Failed> {
        let [u0, u1, lo, hi] = match surf.domain() {
            Some(d) => d,
            None => {
                let (pu, pv) = (surf.period_u(), surf.period_v());
                if pu == 0.0 {
                    return Ok(vec![]);
                }
                let (lo, hi) = if pv != 0.0 {
                    (Some(0.0), Some(pv))
                } else {
                    (surf.pole_v(false), surf.pole_v(true))
                };
                let (Some(lo), Some(hi)) = (lo, hi) else {
                    return Ok(vec![]);
                };
                [0.0, pu, lo, hi]
            }
        };
        let rect = [[u0, lo], [u1, lo], [u1, hi], [u0, hi]];
        let xyz: Vec<Vec3> = rect.iter().map(|q| surf.eval(q[0], q[1])).collect();
        let (uv_all, tri) = triangulate_2d(&rect, &[]);
        Ok(self.refine(surf, uv_all, xyz, tri.tris, &HashSet::new(), 4))
    }

    fn refine(
        &mut self,
        surf: &Surface,
        mut uv: Vec<Vec2>,
        mut xyz: Vec<Vec3>,
        tris: Vec<[usize; 3]>,
        boundary: &HashSet<(usize, usize)>,
        force: usize,
    ) -> Vec<Tri> {
        let tol = self.tol;
        // how far the given points lie off the surface (boundary points of a
        // spline that only approximates the face): no refinement removes that
        let mut off: Vec<f64> = uv
            .iter()
            .zip(&xyz)
            .map(|(q, &p)| length(sub(surf.eval(q[0], q[1]), p)))
            .collect();
        // poles (cone apex, sphere pole): moving along u leaves the point
        let extent = |k: usize, pts: &[Vec2]| {
            let (lo, hi) = pts
                .iter()
                .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), q| {
                    (lo.min(q[k]), hi.max(q[k]))
                });
            hi - lo
        };
        let du = 0.01 * extent(0, &uv);
        let size = (0..3)
            .map(|k| {
                let (lo, hi) = xyz
                    .iter()
                    .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), p| {
                        (lo.min(p[k]), hi.max(p[k]))
                    });
                hi - lo
            })
            .fold(0.0, f64::max);
        let pole: Vec<bool> = uv
            .iter()
            .zip(&xyz)
            .map(|(q, &p)| {
                // both ways: a clamped spline domain ends at the face border too
                du > 0.0
                    && [du, -du]
                        .iter()
                        .all(|d| length(sub(surf.eval(q[0] + d, q[1]), p)) <= 1e-9 * size)
            })
            .collect();
        // the surface's own metric where uv may be stretched
        let stretched = stretched(surf);
        let h = [1e-6 * extent(0, &uv), 1e-6 * extent(1, &uv)];
        let measure = |q: Vec2| surface_metric(surf, q, h);
        let mut metric: Vec<[f64; 3]> = if stretched {
            uv.iter().map(|&q| measure(q)).collect()
        } else {
            vec![]
        };
        let mut tris = delaunay_flips(&uv, stretched.then_some(&metric[..]), tris, boundary);
        let mut done = false;
        // the largest deviation found in each round
        let mut worst: Vec<f64> = vec![];
        for it in 0..64 {
            let mut split: HashMap<(usize, usize), usize> = HashMap::new();
            let mut round_worst: f64 = 0.0;
            for &[a, b, c] in &tris {
                for (x, y) in [(a, b), (b, c), (c, a)] {
                    let key = (x.min(y), x.max(y));
                    if split.contains_key(&key) || boundary.contains(&key) {
                        continue;
                    }
                    let muv = [(uv[x][0] + uv[y][0]) / 2.0, (uv[x][1] + uv[y][1]) / 2.0];
                    let p = surf.eval(muv[0], muv[1]);
                    let chord = mul(add(xyz[x], xyz[y]), 0.5);
                    // an edge from a pole runs along the meridian of its other
                    // end; the uv midpoint would measure a spiral instead
                    let test = match (pole.get(x) == Some(&true), pole.get(y) == Some(&true)) {
                        (true, true) => chord,
                        (true, false) => surf.eval(uv[y][0], muv[1]),
                        (false, true) => surf.eval(uv[x][0], muv[1]),
                        (false, false) => p,
                    };
                    // the distance from the segment, not from its midpoint:
                    // where the parameter runs unevenly along the edge, the
                    // uv midpoint lies to one side along the surface, an
                    // offset that only shrinks with the edge length (not
                    // with its square, as the deviation does)
                    let dev = seg_dist(test, xyz[x], xyz[y]) - (off[x] + off[y]) / 2.0;
                    if dev > tol {
                        round_worst = round_worst.max(dev);
                    }
                    if it < force || dev > tol {
                        split.insert(key, uv.len());
                        uv.push(muv);
                        xyz.push(p);
                        off.push(0.0);
                        if stretched {
                            metric.push(measure(muv));
                        }
                    }
                }
            }
            if split.is_empty() {
                done = true;
                break;
            }
            if tris.len() > MAX_TRIANGLES_PER_FACE {
                break;
            }
            worst.push(round_worst);
            if it >= force + STALL_ROUNDS && round_worst > 0.5 * worst[it - STALL_ROUNDS] {
                break;
            }
            let new: Vec<[usize; 3]> = tris.iter().flat_map(|&t| split_tri(t, &split)).collect();
            tris = delaunay_flips(&uv, stretched.then_some(&metric[..]), new, boundary);
        }
        if !done {
            self.capped_faces += 1;
        }
        tris.iter().map(|t| t.map(|i| xyz[i])).collect()
    }

    // -- fallbacks ---------------------------------------------------------------
    fn fallback(&mut self, face: &Face, loops: &[Vec<Vec3>]) -> Result<Vec<Tri>, Failed> {
        if face.loops.len() == 1 && matches!(face.loops[0].len(), 3 | 4) {
            let sides: Vec<Vec<Vec3>> = face.loops[0]
                .iter()
                .map(|(edge, rev)| {
                    let mut seg = self.edge_points(edge);
                    if *rev {
                        seg.reverse();
                    }
                    seg
                })
                .collect();
            return Ok(coons(sides));
        }
        Ok(self.fallback_loops(loops))
    }

    fn fallback_loops(&mut self, loops: &[Vec<Vec3>]) -> Vec<Tri> {
        if loops.is_empty() {
            return vec![];
        }
        let mut loops: Vec<&Vec<Vec3>> = loops.iter().collect();
        loops.sort_by(|a, b| length(newell_normal(b)).total_cmp(&length(newell_normal(a))));
        let n = normalize(newell_normal(loops[0]));
        if n == ZERO {
            return vec![];
        }
        let u = normalize(cross(
            n,
            if n[0].abs() < 0.9 {
                [1.0, 0.0, 0.0]
            } else {
                [0.0, 1.0, 0.0]
            },
        ));
        let v = cross(n, u);
        let proj =
            |l: &[Vec3]| -> Vec<Vec2> { l.iter().map(|&p| [dot(p, u), dot(p, v)]).collect() };
        let mut outer = proj(loops[0]);
        let mut xyz_all = loops[0].clone();
        if area(&outer) > 0.0 {
            // counter-clockwise already
        } else {
            outer.reverse();
            xyz_all.reverse();
        }
        let mut holes = vec![];
        for l in &loops[1..] {
            let (mut p2, mut p3) = (proj(l), (*l).clone());
            if area(&p2) > 0.0 {
                p2.reverse();
                p3.reverse();
            }
            holes.push(p2);
            xyz_all.extend(p3);
        }
        let (_, tri) = triangulate_2d(&outer, &holes);
        if tri.degraded {
            self.degraded_faces += 1;
        }
        tri.tris.iter().map(|t| t.map(|i| xyz_all[i])).collect()
    }
}

// -- helpers --------------------------------------------------------------------

/// Unit normals of the surface at the triangles' corners, Su x Sv or (when
/// `turned`) its opposite. Where the surface has none (a pole or apex) a
/// corner takes its triangle's other corners' normals, else the facet's;
/// so does a corner whose normal faces away from its triangle (a point the
/// inverse put on the wrong sheet).
fn surface_normals(surf: &Surface, tris: &[Tri], turned: bool) -> Vec<[Vec3; 3]> {
    let sign = if turned { -1.0 } else { 1.0 };
    let mut cache: HashMap<[u64; 3], Option<Vec3>> = HashMap::new();
    let mut at = |p: Vec3| -> Option<Vec3> {
        *cache.entry(p.map(f64::to_bits)).or_insert_with(|| {
            let [u, v] = surf.inverse(p);
            let (hu, hv) = (1e-6 * u.abs().max(1.0), 1e-6 * v.abs().max(1.0));
            let su = sub(surf.eval(u + hu, v), surf.eval(u - hu, v));
            let sv = sub(surf.eval(u, v + hv), surf.eval(u, v - hv));
            let n = cross(su, sv);
            let size = length(su) * length(sv);
            (size > 0.0 && length(n) > 1e-6 * size).then(|| mul(normalize(n), sign))
        })
    };
    tris.iter()
        .map(|t| {
            let facet = normalize(cross(sub(t[1], t[0]), sub(t[2], t[0])));
            let ns = t.map(|p| at(p).filter(|&n| dot(n, facet) > 0.0));
            let known: Vec<Vec3> = ns.iter().flatten().copied().collect();
            let fill = if known.is_empty() {
                facet
            } else {
                normalize(known.iter().fold(ZERO, |a, &n| add(a, n)))
            };
            ns.map(|n| n.unwrap_or(fill))
        })
        .collect()
}

/// The triangles run against the loops: the face lies left of its coedge
/// loops, so a triangle on a boundary edge a -> b has it in the same order.
fn against_loops(tris: &[Tri], loops: &[Vec<Vec3>]) -> bool {
    let key = |p: Vec3| p.map(f64::to_bits);
    let mut directed = HashSet::new();
    for t in tris {
        for k in 0..3 {
            directed.insert((key(t[k]), key(t[(k + 1) % 3])));
        }
    }
    let (mut with, mut against) = (0, 0);
    for l in loops {
        for k in 0..l.len() {
            let (a, b) = (key(l[k]), key(l[(k + 1) % l.len()]));
            with += usize::from(directed.contains(&(a, b)));
            against += usize::from(directed.contains(&(b, a)));
        }
    }
    against > with
}

fn project(c: &Curve, t0: f64, t1: f64, p: Vec3) -> f64 {
    let n = 64;
    let at = |i: usize| t0 + (t1 - t0) * i as f64 / n as f64;
    let mut best = 0;
    for i in 1..=n {
        if dist2(c.eval(at(i)), p) < dist2(c.eval(at(best)), p) {
            best = i;
        }
    }
    let (mut lo, mut hi) = (at(best.saturating_sub(1)), at((best + 1).min(n)));
    for _ in 0..40 {
        // golden-section refine
        let (m1, m2) = (lo + (hi - lo) * 0.382, lo + (hi - lo) * 0.618);
        if dist2(c.eval(m1), p) < dist2(c.eval(m2), p) {
            hi = m2;
        } else {
            lo = m1;
        }
    }
    (lo + hi) / 2.0
}

fn seg_dist(p: Vec3, a: Vec3, b: Vec3) -> f64 {
    let ab = sub(b, a);
    let l2 = dot(ab, ab);
    let t = if l2 == 0.0 {
        0.0
    } else {
        (dot(sub(p, a), ab) / l2).clamp(0.0, 1.0)
    };
    length(sub(p, add(a, mul(ab, t))))
}

fn area(poly: &[Vec2]) -> f64 {
    let n = poly.len();
    (0..n)
        .map(|i| poly[i][0] * poly[(i + 1) % n][1] - poly[(i + 1) % n][0] * poly[i][1])
        .sum::<f64>()
        / 2.0
}

/// A loop's points in the surface's uv domain, unwrapped across periodic
/// seams. A point on a pole (sphere pole, cone apex) has no u of its own:
/// it becomes a pole segment from its predecessor's u to its successor's,
/// so the loop neither seems to wrap nor crosses itself there.
fn map_loop(surf: &Surface, pts: &[Vec3]) -> Result<(Vec<Vec2>, Vec<Vec3>), Failed> {
    let (pu, pv) = (surf.period_u(), surf.period_v());
    let n = pts.len();
    let raw: Vec<Vec2> = pts.iter().map(|&p| surf.inverse(p)).collect();
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in pts {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let size = length(sub(hi, lo));
    let (ulo, uhi) = raw
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(a, b), q| {
            (a.min(q[0]), b.max(q[0]))
        });
    let du = if pu != 0.0 {
        pu / 4.0
    } else {
        (uhi - ulo) / 4.0
    };
    let on_pole = |k: usize| {
        let [u, v] = raw[k];
        du > 0.0
            && size > 0.0
            && [du, -du]
                .iter()
                .all(|d| length(sub(surf.eval(u + d, v), pts[k])) <= 1e-9 * size)
    };
    let pole: Vec<bool> = (0..n).map(on_pole).collect();
    let Some(first) = (0..n).find(|&k| !pole[k]) else {
        return Ok((unwrap(&raw, pu, pv)?, pts.to_vec()));
    };
    // start at a regular point; a pole takes its predecessor's u for now
    let order: Vec<usize> = (0..n).map(|i| (first + i) % n).collect();
    let mut seq: Vec<Vec2> = vec![];
    for &k in &order {
        let q = if pole[k] {
            [seq.last().map_or(raw[k][0], |l: &Vec2| l[0]), raw[k][1]]
        } else {
            raw[k]
        };
        seq.push(q);
    }
    let seq = unwrap(&seq, pu, pv)?;
    let (mut uv, mut xyz) = (vec![], vec![]);
    let mut after_pole = None;
    for (i, &k) in order.iter().enumerate() {
        if pole[k] {
            // the successor (after the last point: the first, a period on)
            let next = if i + 1 < n {
                seq[i + 1][0]
            } else {
                seq[0][0] + wrap_step(seq[0][0], seq[i][0], pu)?
            };
            for u in [seq[i][0], next] {
                uv.push([u, seq[i][1]]);
                xyz.push(pts[k]);
            }
            after_pole.get_or_insert(uv.len() - 1);
        } else {
            uv.push(seq[i]);
            xyz.push(pts[k]);
        }
    }
    // a loop through a pole never runs around the axis: its pole segment
    // goes the other way round instead
    if let (Some(j), true) = (after_pole, pu != 0.0) {
        let (first, last) = (uv[0][0], uv[uv.len() - 1][0]);
        // the closing step taken the short way: what the loop winds in total
        let net = wrap_step(last, first, pu)?;
        if net != 0.0 {
            for q in &mut uv[j..] {
                q[0] -= net;
            }
        }
    }
    Ok((uv, xyz))
}

/// Correction so a -> b is the short way round a periodic parameter.
fn wrap_step(a: f64, b: f64, period: f64) -> Result<f64, Failed> {
    if period == 0.0 {
        return Ok(0.0);
    }
    Ok(-round((b - a) / period)? * period)
}

fn unwrap(uv: &[Vec2], pu: f64, pv: f64) -> Result<Vec<Vec2>, Failed> {
    let mut out = vec![uv[0]];
    for &[mut u, mut v] in &uv[1..] {
        let [lu, lv] = out[out.len() - 1];
        if pu != 0.0 {
            u += round((lu - u) / pu)? * pu;
        }
        if pv != 0.0 {
            v += round((lv - v) / pv)? * pv;
        }
        out.push([u, v]);
    }
    Ok(out)
}

/// Where to cut a seam-wrapping loop open: at a vertex whose neighbours
/// both differ from it in u, so the seam cut never runs along the loop's own
/// u-constant pieces (that would retrace them). With a `gap` (mod `p`) free
/// of holes, at such a vertex inside it, nearest its middle: a cut through
/// a hole leaves the hole half outside the band.
fn cut_u(uv: &[Vec2], gap: Option<(f64, f64)>, p: f64) -> f64 {
    let n = uv.len();
    let step = |k: usize| (uv[k][0] - uv[(k + n - 1) % n][0]).abs();
    let min_step = |k: usize| step(k).min(step((k + 1) % n));
    let mut best = 0;
    let mut best_gap = -1.0;
    for k in 0..n {
        let gap = min_step(k);
        if gap > best_gap {
            (best, best_gap) = (k, gap);
        }
    }
    if let Some((lo, hi)) = gap {
        let at = |k: usize| lo + (uv[k][0] - lo).rem_euclid(p);
        let inside: Vec<usize> = (0..n).filter(|&k| at(k) > lo && at(k) < hi).collect();
        let widest = inside.iter().map(|&k| min_step(k)).fold(0.0, f64::max);
        let centre = (lo + hi) / 2.0;
        if let Some(k) = inside
            .into_iter()
            .filter(|&k| min_step(k) >= 0.5 * widest && widest > 0.0)
            .min_by(|&a, &b| (at(a) - centre).abs().total_cmp(&(at(b) - centre).abs()))
        {
            best = k;
        }
    }
    uv[best][0]
}

/// The extent of a polyline in u (k = 0) or v (k = 1).
fn range(pts: &[Vec2], k: usize) -> (f64, f64) {
    pts.iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), q| {
            (lo.min(q[k]), hi.max(q[k]))
        })
}

/// The widest interval of a periodic parameter (period `p`) outside all
/// `spans`, as lo..hi with lo < hi (hi may pass a period on). None when
/// there are no spans to avoid, the parameter is not periodic, or the spans
/// cover it all.
fn free_gap(spans: &[(f64, f64)], p: f64) -> Option<(f64, f64)> {
    if p == 0.0 || spans.is_empty() {
        return None;
    }
    let mut spans: Vec<(f64, f64)> = spans
        .iter()
        .map(|&(lo, hi)| {
            let s = lo.rem_euclid(p);
            (s, s + (hi - lo))
        })
        .collect();
    if spans
        .iter()
        .any(|&(lo, hi)| hi - lo >= p || (hi - lo).is_nan())
    {
        return None;
    }
    spans.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut best: Option<(f64, f64)> = None;
    let mut end = spans[0].1;
    for &(lo, hi) in &spans[1..] {
        if lo > end && best.is_none_or(|(a, b)| lo - end > b - a) {
            best = Some((end, lo));
        }
        end = end.max(hi);
    }
    // round the period, back to the first span
    let first = spans[0].0 + p;
    if first > end && best.is_none_or(|(a, b)| first - end > b - a) {
        best = Some((end, first));
    }
    best
}

/// Re-open a seam-wrapping loop as a polyline from uc to uc +/- P.
fn cut_at(uv: &[Vec2], xyz: &[Vec3], uc: f64, p: f64) -> Result<Option<UvPolygon>, Failed> {
    let n = uv.len();
    // unwrap two turns so any crossing is found
    let mut ext_uv: Vec<Vec2> = Vec::with_capacity(2 * n + 1);
    for k in 0..=2 * n {
        let mut q = uv[k % n];
        if let Some(prev) = ext_uv.last() {
            q[0] += round((prev[0] - q[0]) / p)? * p;
        }
        ext_uv.push(q);
    }
    // the start index whose u is closest to uc (mod P)
    let dist = |k: usize| ((ext_uv[k][0] - uc + p / 2.0).rem_euclid(p) - p / 2.0).abs();
    let mut k0 = 0;
    for k in 1..n {
        if dist(k) < dist(k0) {
            k0 = k;
        }
    }
    let shift = ext_uv[k0][0] - uc;
    let mut seg_uv: Vec<Vec2> = ext_uv[k0..=k0 + n]
        .iter()
        .map(|q| [q[0] - shift, q[1]])
        .collect();
    let seg_xyz: Vec<Vec3> = (k0..=k0 + n).map(|k| xyz[k % n]).collect();
    let (first, last) = (seg_uv[0][0], seg_uv[n][0]);
    if ((last - first).abs() - p).abs() > p * 0.25 {
        return Ok(None);
    }
    // force exact seam coordinates at both ends
    let sgn = if last > first { 1.0 } else { -1.0 };
    seg_uv[n][0] = first + sgn * p;
    Ok(Some((seg_uv, seg_xyz)))
}

/// Move a hole by whole periods so it lies inside the outer loop's range:
/// their middles less than half a period apart. (The mean of the outer
/// loop's points leans to where they are dense, and a hole half a period
/// from it, as on a band cut open beside a hole, stayed outside.)
fn shift_into(hole: &[Vec2], outer: &[Vec2], pu: f64, pv: f64) -> Result<Vec<Vec2>, Failed> {
    if pu == 0.0 && pv == 0.0 {
        return Ok(hole.to_vec());
    }
    let mid = |pts: &[Vec2], k: usize| {
        let (lo, hi) = range(pts, k);
        (lo + hi) / 2.0
    };
    let du = if pu != 0.0 {
        round((mid(outer, 0) - mid(hole, 0)) / pu)? * pu
    } else {
        0.0
    };
    let dv = if pv != 0.0 {
        round((mid(outer, 1) - mid(hole, 1)) / pv)? * pv
    } else {
        0.0
    };
    Ok(hole.iter().map(|q| [q[0] + du, q[1] + dv]).collect())
}

/// Lawson flips in the uv domain: ear clipping leaves slivers that make
/// midpoint refinement explode; flipping to Delaunay keeps triangles round.
fn delaunay_flips(
    uv: &[Vec2],
    metric: Option<&[[f64; 3]]>,
    mut tris: Vec<[usize; 3]>,
    boundary: &HashSet<(usize, usize)>,
) -> Vec<[usize; 3]> {
    // directed edge -> triangle, and the edges in first-insertion order
    let mut owner: HashMap<(usize, usize), usize> = HashMap::new();
    let mut order = vec![];
    for (ti, &[a, b, c]) in tris.iter().enumerate() {
        for e in [(a, b), (b, c), (c, a)] {
            if owner.insert(e, ti).is_none() {
                order.push(e);
            }
        }
    }
    let turn = |a: usize, b: usize, c: usize| orient(uv[a], uv[b], uv[c]);
    // Delaunay in the surface's own metric where it is known: a
    // parameterisation that stretches one direction (most splines, a torus
    // whose tube is wider than its ring) would otherwise give triangles
    // that are round in uv but slivers on the surface, and refining those
    // takes many times the triangles
    let not_delaunay = |a: usize, b: usize, c: usize, d: usize| {
        let map = metric.and_then(|m| quad_metric(m, [a, b, c, d]));
        let q = |i: usize| map.as_ref().map_or(uv[i], |l| l(uv[i]));
        let (qa, qb, qc, qd) = (q(a), q(b), q(c), q(d));
        incircle(qa, qb, qc, qd) > 1e-12 * (orient(qa, qb, qc).powi(2) + 1e-300)
    };
    let apex = |t: [usize; 3], a: usize, b: usize| {
        let [x, y, z] = t;
        if (x, y) == (a, b) {
            z
        } else if (y, z) == (a, b) {
            x
        } else {
            y
        }
    };

    let mut stack: Vec<(usize, usize)> = order
        .into_iter()
        .filter(|&(a, b)| a < b && owner.contains_key(&(b, a)))
        .collect();
    let mut budget = 30 * tris.len() + 100;
    while budget > 0 {
        let Some((a, b)) = stack.pop() else { break };
        budget -= 1;
        if boundary.contains(&(a.min(b), a.max(b))) {
            continue;
        }
        let (Some(&t1), Some(&t2)) = (owner.get(&(a, b)), owner.get(&(b, a))) else {
            continue;
        };
        let (c, d) = (apex(tris[t1], a, b), apex(tris[t2], b, a));
        if !not_delaunay(a, b, c, d) {
            continue;
        }
        if turn(a, d, c) <= 0.0 || turn(d, b, c) <= 0.0 {
            continue; // non-convex quad
        }
        tris[t1] = [a, d, c];
        tris[t2] = [d, b, c];
        owner.remove(&(a, b));
        owner.remove(&(b, a));
        for e in [(a, d), (d, c), (c, a)] {
            owner.insert(e, t1);
        }
        for e in [(d, b), (b, c), (c, d)] {
            owner.insert(e, t2);
        }
        stack.extend([(a, d), (d, b), (b, c), (c, a)]);
    }
    tris
}

fn orient(a: Vec2, b: Vec2, c: Vec2) -> f64 {
    (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
}

/// Positive when d lies inside the circle through a, b, c (counter-clockwise).
fn incircle(a: Vec2, b: Vec2, c: Vec2, d: Vec2) -> f64 {
    let (ax, ay) = (a[0] - d[0], a[1] - d[1]);
    let (bx, by) = (b[0] - d[0], b[1] - d[1]);
    let (cx, cy) = (c[0] - d[0], c[1] - d[1]);
    (ax * ax + ay * ay) * (bx * cy - cx * by) - (bx * bx + by * by) * (ax * cy - cx * ay)
        + (cx * cx + cy * cy) * (ax * by - bx * ay)
}

/// The surface's first fundamental form [E, F, G] = [Su.Su, Su.Sv, Sv.Sv]
/// at a uv point, from differences of step h. Each derivative takes the
/// longer of the forward and the backward difference: at a clamped end of
/// a spline domain one of them is zero.
fn surface_metric(surf: &Surface, q: Vec2, h: Vec2) -> [f64; 3] {
    let p = surf.eval(q[0], q[1]);
    let derivative = |k: usize| {
        let diff = |sign: f64| {
            let mut r = q;
            r[k] += sign * h[k];
            mul(sub(surf.eval(r[0], r[1]), p), sign / h[k])
        };
        let (fwd, back) = (diff(1.0), diff(-1.0));
        if dot(fwd, fwd) >= dot(back, back) {
            fwd
        } else {
            back
        }
    };
    let (su, sv) = (derivative(0), derivative(1));
    [dot(su, su), dot(su, sv), dot(sv, sv)]
}

/// Whether uv may be stretched against the surface, so that triangulating
/// and refining in it needs the surface's own metric: a torus's u is the
/// angle times the ring radius (the tube may be far wider), a spline's
/// parameters can be anything. A cone's and a sphere's u and v are lengths
/// on the surface (stretched only towards an apex or a pole, which costs a
/// few triangles there): they keep plain uv.
fn stretched(surf: &Surface) -> bool {
    matches!(
        surf,
        Surface::Torus(_) | Surface::Spline(_) | Surface::Pipe(_)
    )
}

/// The map from uv to lengths on the surface for a whole face: by the mean
/// first fundamental form at its boundary points.
fn uv_metric(surf: &Surface, ring: &[Vec2]) -> Option<impl Fn(Vec2) -> Vec2> {
    let extent = |k: usize| {
        let (lo, hi) = ring
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), q| {
                (lo.min(q[k]), hi.max(q[k]))
            });
        hi - lo
    };
    let h = [1e-6 * extent(0), 1e-6 * extent(1)];
    let mut m = [0.0; 3];
    for &q in ring {
        let f = surface_metric(surf, q, h);
        for k in 0..3 {
            m[k] += f[k] / ring.len() as f64;
        }
    }
    metric_map(m)
}

/// The linear map that takes uv to lengths on the surface around four
/// points: by their mean first fundamental form.
fn quad_metric(metric: &[[f64; 3]], ids: [usize; 4]) -> Option<impl Fn(Vec2) -> Vec2> {
    let mut m = [0.0; 3];
    for i in ids {
        for k in 0..3 {
            m[k] += metric[i][k] / 4.0;
        }
    }
    metric_map(m)
}

/// The upper triangular factor L of a first fundamental form M = [E, F, G]
/// as a map (|L q|^2 = q^T M q). None where M is degenerate (at a pole).
fn metric_map(m: [f64; 3]) -> Option<impl Fn(Vec2) -> Vec2> {
    let [e, f, g] = m;
    if !(e > 0.0 && g > 0.0) {
        return None;
    }
    let det = g - f * f / e;
    if !(det > 1e-12 * e.max(g) && det.is_finite()) {
        return None;
    }
    let (l00, l01, l11) = (e.sqrt(), f / e.sqrt(), det.sqrt());
    Some(move |q: Vec2| [l00 * q[0] + l01 * q[1], l11 * q[1]])
}

fn split_tri(t: [usize; 3], split: &HashMap<(usize, usize), usize>) -> Vec<[usize; 3]> {
    let [mut a, mut b, mut c] = t;
    let mid = |x: usize, y: usize| split.get(&(x.min(y), x.max(y))).copied();
    let mut ms = [mid(a, b), mid(b, c), mid(c, a)];
    let k = ms.iter().filter(|m| m.is_some()).count();
    if k == 0 {
        return vec![t];
    }
    if k == 3 {
        let [mab, mbc, mca] = ms.map(Option::unwrap);
        return vec![[a, mab, mca], [mab, b, mbc], [mca, mbc, c], [mab, mbc, mca]];
    }
    // rotate so the split edges come first
    for _ in 0..3 {
        if (k == 1 && ms[0].is_some()) || (k == 2 && ms[2].is_none()) {
            break;
        }
        (a, b, c) = (b, c, a);
        ms.rotate_left(1);
    }
    if k == 1 {
        let m = ms[0].unwrap();
        return vec![[a, m, c], [m, b, c]];
    }
    let (mab, mbc) = (ms[0].unwrap(), ms[1].unwrap());
    vec![[mab, b, mbc], [a, mab, mbc], [a, mbc, c]]
}

/// Bilinearly blended Coons patch over 3 or 4 boundary polylines.
fn coons(mut sides: Vec<Vec<Vec3>>) -> Vec<Tri> {
    if sides.len() == 3 {
        let side = vec![sides[2][sides[2].len() - 1], sides[0][0]]; // degenerate 4th side
        sides.push(side);
    }
    let at = |poly: &[Vec3], s: f64| -> Vec3 {
        if poly.len() == 1 {
            return poly[0];
        }
        let mut lens = vec![0.0];
        for i in 1..poly.len() {
            lens.push(lens[i - 1] + length(sub(poly[i], poly[i - 1])));
        }
        let total = nonzero(lens[lens.len() - 1]);
        let target = s * total;
        for i in 1..poly.len() {
            if lens[i] >= target {
                let seg = nonzero(lens[i] - lens[i - 1]);
                let f = (target - lens[i - 1]) / seg;
                return add(poly[i - 1], mul(sub(poly[i], poly[i - 1]), f));
            }
        }
        poly[poly.len() - 1]
    };
    // c0: P00->P10, c1: P10->P11, c2: P11->P01, c3: P01->P00
    let (c0, c1, c2, c3) = (&sides[0], &sides[1], &sides[2], &sides[3]);
    let n = sides.iter().map(Vec::len).max().unwrap().clamp(2, 32);
    let (p00, p10, p11, p01) = (c0[0], c0[c0.len() - 1], c2[0], c2[c2.len() - 1]);
    let mut grid = vec![];
    for i in 0..=n {
        let s = i as f64 / n as f64;
        let mut row = vec![];
        for j in 0..=n {
            let t = j as f64 / n as f64;
            let (bottom, top) = (at(c0, s), at(c2, 1.0 - s));
            let (left, right) = (at(c3, 1.0 - t), at(c1, t));
            let p = add(
                add(mul(bottom, 1.0 - t), mul(top, t)),
                add(mul(left, 1.0 - s), mul(right, s)),
            );
            let corr = add(
                add(mul(p00, (1.0 - s) * (1.0 - t)), mul(p10, s * (1.0 - t))),
                add(mul(p01, (1.0 - s) * t), mul(p11, s * t)),
            );
            row.push(sub(p, corr));
        }
        grid.push(row);
    }
    let mut tris = vec![];
    for i in 0..n {
        for j in 0..n {
            let (a, b, c, d) = (
                grid[i][j],
                grid[i + 1][j],
                grid[i + 1][j + 1],
                grid[i][j + 1],
            );
            tris.push([a, b, c]);
            tris.push([a, c, d]);
        }
    }
    tris
}

/// `x or 1.0`
fn nonzero(x: f64) -> f64 {
    if x != 0.0 { x } else { 1.0 }
}

#[cfg(test)]
mod tests {
    use std::f64::consts::{FRAC_PI_2, PI, TAU};
    use std::rc::Rc;

    use super::*;
    use crate::acis::brep::{Cone, Pipe, Plane, Sphere, Torus};
    use crate::acis::nurbs::BSplineCurve;

    fn total_area(tris: &[Tri]) -> f64 {
        tris.iter()
            .map(|[a, b, c]| length(cross(sub(*b, *a), sub(*c, *a))) / 2.0)
            .sum()
    }

    fn circle_edge(index: usize, z: f64, r: f64) -> Rc<Edge> {
        let p = [r, 0.0, z];
        Rc::new(Edge {
            index,
            start: p,
            end: p,
            t0: 0.0,
            t1: TAU,
            curve: Some(Curve::ellipse(
                [0.0, 0.0, z],
                [0.0, 0.0, 1.0],
                [r, 0.0, 0.0],
                1.0,
            )),
            reversed: false,
        })
    }

    fn line_edge(index: usize, a: Vec3, b: Vec3) -> Rc<Edge> {
        Rc::new(Edge {
            index,
            start: a,
            end: b,
            t0: 0.0,
            t1: 1.0,
            curve: Some(Curve::straight(a, sub(b, a))),
            reversed: false,
        })
    }

    fn face(surface: Surface, loops: Vec<Loop>) -> Face {
        Face {
            surface: Some(SurfaceDef::Analytic(surface)),
            surface_type: String::new(),
            reversed: false,
            surface_reversed: false,
            loops,
        }
    }

    fn square(size: f64) -> Loop {
        let sq = [
            [0.0, 0.0, 0.0],
            [size, 0.0, 0.0],
            [size, size, 0.0],
            [0.0, size, 0.0],
        ];
        (0..4)
            .map(|i| (line_edge(i, sq[i], sq[(i + 1) % 4]), false))
            .collect()
    }

    #[test]
    fn cylinder_band() {
        let surf = Surface::Cone(Cone::new(
            [0.0; 3],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0],
            1.0,
            0.0,
            1.0,
        ));
        let f = face(
            surf,
            vec![
                vec![(circle_edge(0, 0.0, 1.0), false)],
                vec![(circle_edge(1, 1.0, 1.0), true)],
            ],
        );
        let tris = Tessellator::new(1e-3).face(&f).unwrap();
        assert!((total_area(&tris) - TAU).abs() < 0.01 * TAU);
        for p in tris.iter().flatten() {
            assert!((p[0].hypot(p[1]) - 1.0).abs() < 1e-6);
            assert!(-1e-9 <= p[2] && p[2] <= 1.0 + 1e-9);
        }
    }

    fn arc_edge(index: usize, z: f64, r: f64, t1: f64) -> Rc<Edge> {
        Rc::new(Edge {
            index,
            start: [r, 0.0, z],
            end: [r * t1.cos(), r * t1.sin(), z],
            t0: 0.0,
            t1,
            curve: Some(Curve::ellipse(
                [0.0, 0.0, z],
                [0.0, 0.0, 1.0],
                [r, 0.0, 0.0],
                1.0,
            )),
            reversed: false,
        })
    }

    #[test]
    fn pipe_inverse_round_trips_on_a_curved_spine() {
        // a quadratic Bezier bend
        let spine = BSplineCurve::new(
            2,
            vec![0.0, 0.0, 0.0, 1.0, 1.0, 1.0],
            vec![[0.0, 0.0, 0.0], [1.0, 0.0, 0.0], [1.0, 1.0, 0.5]],
            None,
        );
        let pipe = Pipe::new(spine, 0.2);
        let len = 1.6;
        for i in 1..8 {
            for j in -3..4 {
                let (u, v) = (len * i as f64 / 8.0, 0.2 * j as f64 * 0.9);
                let p = pipe.eval(u, v);
                assert!((pipe.spine_distance(p) - 0.2).abs() < 1e-6);
                let [u2, v2] = pipe.inverse(p);
                assert!(length(sub(pipe.eval(u2, v2), p)) < 1e-6);
            }
        }
    }

    #[test]
    fn pipe_blend_face_follows_the_surface_and_its_loop() {
        // a quarter of a unit pipe around the z axis between z = 0 and 1,
        // like a fillet strip; the loop runs counter-clockwise seen from
        // outside (+x+y), so the face's normal points away from the axis
        let spine = BSplineCurve::new(
            1,
            vec![0.0, 0.0, 1.0, 1.0],
            vec![[0.0, 0.0, -0.5], [0.0, 0.0, 1.5]],
            None,
        );
        let (a, b) = ([1.0, 0.0, 0.0], [0.0, 1.0, 0.0]);
        let (c, d) = ([0.0, 1.0, 1.0], [1.0, 0.0, 1.0]);
        let lp: Loop = vec![
            (arc_edge(0, 0.0, 1.0, FRAC_PI_2), false),
            (line_edge(1, b, c), false),
            (arc_edge(2, 1.0, 1.0, FRAC_PI_2), true),
            (line_edge(3, d, a), false),
        ];
        let mut f = Face {
            surface: Some(SurfaceDef::PipeChoice(vec![spine])),
            surface_type: "spline-surface".into(),
            reversed: false,
            surface_reversed: false,
            loops: vec![lp.clone()],
        };
        let radial = |tris: &[Tri]| -> Vec<f64> {
            tris.iter()
                .map(|[a, b, c]| {
                    let n = cross(sub(*b, *a), sub(*c, *a));
                    let m = mul(add(add(*a, *b), *c), 1.0 / 3.0);
                    dot(n, [m[0], m[1], 0.0])
                })
                .collect()
        };
        let mut tess = Tessellator::new(1e-3);
        let tris = tess.face(&f).unwrap();
        assert_eq!(tess.approximated_faces, 0);
        assert!((total_area(&tris) - FRAC_PI_2).abs() < 0.01);
        for p in tris.iter().flatten() {
            assert!((p[0].hypot(p[1]) - 1.0).abs() < 1e-6);
        }
        assert!(radial(&tris).iter().all(|&d| d > 0.0));
        // the same strip with the loop the other way round faces the axis
        f.loops = vec![lp.iter().rev().map(|(e, r)| (e.clone(), !r)).collect()];
        let tris = Tessellator::new(1e-3).face(&f).unwrap();
        assert!(radial(&tris).iter().all(|&d| d < 0.0));
    }

    #[test]
    fn normals_come_from_the_surface() {
        // a coarse cylinder band: facet normals would be off by half a
        // segment angle, the surface's are radial at every corner
        let surf = Surface::Cone(Cone::new(
            [0.0; 3],
            [0.0, 0.0, 1.0],
            [1.0, 0.0, 0.0],
            1.0,
            0.0,
            1.0,
        ));
        let mut f = face(
            surf,
            vec![
                vec![(circle_edge(0, 0.0, 1.0), false)],
                vec![(circle_edge(1, 1.0, 1.0), true)],
            ],
        );
        for (reversed, sign) in [(false, 1.0), (true, -1.0)] {
            f.reversed = reversed;
            let body = Body {
                faces: vec![f.clone()],
                transform: None,
            };
            let mut tess = Tessellator::new(0.05);
            let tris = tess.body(&body);
            assert_eq!(tess.normals.len(), tris.len());
            for (t, n) in tris.iter().zip(&tess.normals) {
                let n = n.expect("a face with a surface has normals");
                for (p, n) in t.iter().zip(n) {
                    let radial = normalize([p[0], p[1], 0.0]);
                    assert!(dot(n, radial) * sign > 1.0 - 1e-6);
                }
            }
        }
    }

    #[test]
    fn plane_with_hole() {
        let sq = [
            [-1.0, -1.0, 0.0],
            [1.0, -1.0, 0.0],
            [1.0, 1.0, 0.0],
            [-1.0, 1.0, 0.0],
        ];
        let outer: Loop = (0..4)
            .map(|i| (line_edge(i, sq[i], sq[(i + 1) % 4]), false))
            .collect();
        let hole = vec![(circle_edge(9, 0.0, 0.5), true)];
        let f = face(
            Surface::Plane(Plane::new([0.0; 3], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0])),
            vec![outer, hole],
        );
        let tris = Tessellator::new(1e-4).face(&f).unwrap();
        assert!((total_area(&tris) - (4.0 - PI * 0.25)).abs() < 0.005);
    }

    #[test]
    fn full_sphere() {
        let f = face(
            Surface::Sphere(Sphere::new([0.0; 3], 1.0, [1.0, 0.0, 0.0], [0.0, 0.0, 1.0])),
            vec![],
        );
        let tris = Tessellator::new(1e-3).face(&f).unwrap();
        assert!((total_area(&tris) - 4.0 * PI).abs() < 0.02 * 4.0 * PI);
    }

    #[test]
    fn stretched_parameters_refine_on_the_surface() {
        // a torus whose ring (0.05) is far smaller than its tube (10): u is
        // the angle times 0.05, so the surface is 200 times longer in u than
        // its uv; round uv triangles would be slivers on it
        let f = face(
            Surface::Torus(Torus::new(
                [0.0; 3],
                [0.0, 0.0, 1.0],
                0.05,
                10.0,
                [1.0, 0.0, 0.0],
            )),
            vec![],
        );
        let mut tess = Tessellator::new(0.01);
        let tris = tess.face(&f).unwrap();
        assert_eq!(tess.capped_faces, 0);
        // about 29 000 (both sheets of what is nearly a sphere of radius 10);
        // stretched uv triangles reached the per-face limit
        assert!(tris.len() < 40_000, "{} triangles", tris.len());
    }

    #[test]
    fn reversed_face_faces_down() {
        let mut f = face(
            Surface::Plane(Plane::new([0.0; 3], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0])),
            vec![square(1.0)],
        );
        let up = |tris: &[Tri]| {
            tris.iter()
                .all(|[a, b, c]| cross(sub(*b, *a), sub(*c, *a))[2] > 0.0)
        };
        assert!(up(&Tessellator::new(1e-4).face(&f).unwrap()));
        // a reversed face (its loop then runs the other way round in ACIS,
        // but the sense alone decides here)
        f.reversed = true;
        let tris = Tessellator::new(1e-4).face(&f).unwrap();
        assert!(
            tris.iter()
                .all(|[a, b, c]| cross(sub(*b, *a), sub(*c, *a))[2] < 0.0)
        );
    }

    #[test]
    fn sphere_cap_closes_on_the_face_side() {
        let sphere =
            || Surface::Sphere(Sphere::new([0.0; 3], 1.0, [1.0, 0.0, 0.0], [0.0, 0.0, 1.0]));
        let r = 0.75f64.sqrt();
        // the loop runs counter-clockwise seen from above: the face is the
        // cap inside it, the north one (area 2 pi r h, h = 0.5)
        let north = face(sphere(), vec![vec![(circle_edge(0, 0.5, r), false)]]);
        let tris = Tessellator::new(1e-4).face(&north).unwrap();
        assert!(
            (total_area(&tris) - PI).abs() < 0.01 * PI,
            "{}",
            total_area(&tris)
        );
        assert!(tris.iter().flatten().all(|p| p[2] >= 0.5 - 1e-9));
        // run the other way: everything south of it
        let south = face(sphere(), vec![vec![(circle_edge(1, 0.5, r), true)]]);
        let tris = Tessellator::new(1e-4).face(&south).unwrap();
        assert!(
            (total_area(&tris) - 3.0 * PI).abs() < 0.01 * 3.0 * PI,
            "{}",
            total_area(&tris)
        );
        // outward: the triangles face away from the centre
        for [a, b, c] in &tris {
            let n = cross(sub(*b, *a), sub(*c, *a));
            assert!(dot(n, add(add(*a, *b), *c)) > 0.0);
        }
    }

    /// A circle (or arc from angle t0 to t1) round `normal` through
    /// `center + major`.
    fn ring(index: usize, center: Vec3, normal: Vec3, major: Vec3, t0: f64, t1: f64) -> Rc<Edge> {
        let curve = Curve::ellipse(center, normal, major, 1.0);
        Rc::new(Edge {
            index,
            start: curve.eval(t0),
            end: curve.eval(t1),
            t0,
            t1,
            curve: Some(curve),
            reversed: false,
        })
    }

    /// A sphere a cylinder of radius a runs through along x: the face has
    /// two holes and no outer loop, and the hole round -x lies across the
    /// sphere's seam. The holes were taken for the face: one cap was left.
    #[test]
    fn sphere_with_two_holes_and_no_outer_loop() {
        let (r, a) = (1.0f64, 0.25f64);
        let x = (r * r - a * a).sqrt();
        // seen from outside, a hole runs clockwise: round the inward axis
        let holes = [1.0, -1.0].map(|s| {
            vec![(
                ring(
                    usize::from(s < 0.0),
                    [s * x, 0.0, 0.0],
                    [-s, 0.0, 0.0],
                    [0.0, 0.0, a],
                    0.0,
                    TAU,
                ),
                false,
            )]
        });
        let f = face(
            Surface::Sphere(Sphere::new([0.0; 3], r, [1.0, 0.0, 0.0], [0.0, 0.0, 1.0])),
            holes.to_vec(),
        );
        let mut tess = Tessellator::new(1e-4);
        let tris = tess.face(&f).unwrap();
        assert_eq!((tess.degraded_faces, tess.approximated_faces), (0, 0));
        let want = 4.0 * PI * r * r - 2.0 * TAU * r * (r - x);
        let got = total_area(&tris);
        assert!((got - want).abs() < 0.005 * want, "{got} != {want}");
        for [p, q, s] in &tris {
            let n = cross(sub(*q, *p), sub(*s, *p));
            assert!(dot(n, add(add(*p, *q), *s)) > -1e-12, "facing in");
            // nothing inside a hole
            assert!([p, q, s].iter().any(|c| c[0].abs() <= x + 1e-9));
        }
        let top = tris.iter().flatten().map(|p| p[2]).fold(f64::MIN, f64::max);
        assert!((top - r).abs() < 1e-9);
    }

    /// A cylinder band with a hole that covers the band's widest-sampled
    /// stretch (a short arc of its bottom loop, where the seam was cut): the
    /// cut must go round the hole, or the hole comes out half outside the
    /// band and is lost.
    #[test]
    fn band_is_not_cut_open_through_a_hole() {
        let deg = PI / 180.0;
        let (z, axis, major) = ([0.0, 0.0, 1.0], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0]);
        let at = |h: f64| [0.0, 0.0, h];
        let ruling = |i: usize, ang: f64, h0: f64, h1: f64| {
            let (c, s) = (ang.cos(), ang.sin());
            line_edge(i, [c, s, h0], [c, s, h1])
        };
        // bottom runs counter-clockwise seen from above, the top back
        let bottom: Loop = vec![
            (
                ring(0, at(0.0), axis, major, -15.0 * deg, 15.0 * deg),
                false,
            ),
            (
                ring(1, at(0.0), axis, major, 15.0 * deg, 345.0 * deg),
                false,
            ),
        ];
        let top: Loop = vec![
            (ring(3, at(1.0), z, major, 15.0 * deg, 345.0 * deg), true),
            (ring(2, at(1.0), z, major, -15.0 * deg, 15.0 * deg), true),
        ];
        // u -20..20 degrees, height 0.4..0.6; clockwise seen from outside
        let (u0, u1) = (-20.0 * deg, 20.0 * deg);
        let hole: Loop = vec![
            (ring(4, at(0.6), axis, major, u0, u1), false),
            (ruling(5, u1, 0.6, 0.4), false),
            (ring(6, at(0.4), axis, major, u0, u1), true),
            (ruling(7, u0, 0.4, 0.6), false),
        ];
        let surf = Surface::Cone(Cone::new([0.0; 3], z, major, 1.0, 0.0, 1.0));
        let f = face(surf, vec![bottom, top, hole]);
        let mut tess = Tessellator::new(1e-3);
        let tris = tess.face(&f).unwrap();
        assert_eq!(tess.degraded_faces, 0);
        let want = TAU - 40.0 * deg * 0.2;
        let got = total_area(&tris);
        assert!((got - want).abs() < 0.002 * want, "{got} != {want}");
        for t in &tris {
            let c = mul(add(add(t[0], t[1]), t[2]), 1.0 / 3.0);
            let ang = c[1].atan2(c[0]);
            assert!(!(ang.abs() < 18.0 * deg && (0.42..0.58).contains(&c[2])));
        }
    }

    #[test]
    fn free_gap_goes_round_the_period() {
        let p = 10.0;
        assert_eq!(free_gap(&[], p), None);
        assert_eq!(free_gap(&[(1.0, 2.0)], 0.0), None);
        assert_eq!(free_gap(&[(1.0, 2.0)], p), Some((2.0, 11.0)));
        // the widest of two, one across the seam
        assert_eq!(free_gap(&[(-1.0, 1.0), (4.0, 5.0)], p), Some((5.0, 9.0)));
        // overlapping spans merge
        assert_eq!(free_gap(&[(2.0, 4.0), (3.0, 8.0)], p), Some((8.0, 12.0)));
        assert_eq!(free_gap(&[(0.0, 10.0)], p), None);
    }

    #[test]
    fn body_transform_scales_tolerance() {
        let f = face(
            Surface::Plane(Plane::new([0.0; 3], [0.0, 0.0, 1.0], [1.0, 0.0, 0.0])),
            vec![square(1000.0)],
        );
        let mm_to_m = (
            [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
            [0.0; 3],
            0.001,
        );
        let tris = Tessellator::new(1e-4).body(&Body {
            faces: vec![f],
            transform: Some(mm_to_m),
        });
        assert!((total_area(&tris) - 1.0).abs() < 1e-9);
    }
}
