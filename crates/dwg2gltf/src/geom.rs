// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Small vector/matrix helpers, curve sampling and polygon triangulation.

use std::f64::consts::PI;

use crate::limits::MAX_LINE_POINTS;

pub type Vec3 = [f64; 3];
pub type Vec2 = [f64; 2];
/// 4x4 row-major matrix.
pub type Mat = [f64; 16];

pub const IDENTITY: Mat = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];
pub const ZERO: Vec3 = [0.0; 3];

pub fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}

pub fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub fn mul(a: Vec3, s: f64) -> Vec3 {
    [a[0] * s, a[1] * s, a[2] * s]
}

pub fn dot(a: Vec3, b: Vec3) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

pub fn length(a: Vec3) -> f64 {
    dot(a, a).sqrt()
}

pub fn normalize(a: Vec3) -> Vec3 {
    let n = length(a);
    if n > 1e-300 {
        [a[0] / n, a[1] / n, a[2] / n]
    } else {
        ZERO
    }
}

pub fn dist2(a: Vec3, b: Vec3) -> f64 {
    let d = sub(a, b);
    dot(d, d)
}

/// Finite and not absurdly large (garbage coordinates show up as ~1e300).
pub fn finite(p: Vec3) -> bool {
    p.iter().all(|c| c.is_finite() && c.abs() < 1e18)
}

/// Index of the component with the largest magnitude (the first on ties).
pub fn dominant_axis(n: Vec3) -> usize {
    let mut axis = 0;
    for i in 1..3 {
        if n[i].abs() > n[axis].abs() {
            axis = i;
        }
    }
    axis
}

// -- matrices -----------------------------------------------------------------

pub fn mat_mul(a: &Mat, b: &Mat) -> Mat {
    let mut out = [0.0; 16];
    for r in 0..4 {
        for c in 0..4 {
            out[r * 4 + c] = (0..4).map(|k| a[r * 4 + k] * b[k * 4 + c]).sum();
        }
    }
    out
}

pub fn chain(ms: &[Mat]) -> Mat {
    ms.iter().fold(IDENTITY, |acc, m| mat_mul(&acc, m))
}

pub fn translate(v: Vec3) -> Mat {
    [
        1.0, 0.0, 0.0, v[0], 0.0, 1.0, 0.0, v[1], 0.0, 0.0, 1.0, v[2], 0.0, 0.0, 0.0, 1.0,
    ]
}

pub fn scale(s: Vec3) -> Mat {
    [
        s[0], 0.0, 0.0, 0.0, 0.0, s[1], 0.0, 0.0, 0.0, 0.0, s[2], 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

pub fn rotate_z(a: f64) -> Mat {
    let (s, c) = a.sin_cos();
    [
        c, -s, 0.0, 0.0, s, c, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
}

pub fn apply(m: &Mat, p: Vec3) -> Vec3 {
    [
        m[0] * p[0] + m[1] * p[1] + m[2] * p[2] + m[3],
        m[4] * p[0] + m[5] * p[1] + m[6] * p[2] + m[7],
        m[8] * p[0] + m[9] * p[1] + m[10] * p[2] + m[11],
    ]
}

/// AutoCAD arbitrary-axis algorithm: OCS x/y/z axes in WCS.
pub fn ocs_axes(normal: Option<Vec3>) -> [Vec3; 3] {
    let mut n = normal.map(normalize).unwrap_or([0.0, 0.0, 1.0]);
    if n == ZERO {
        n = [0.0, 0.0, 1.0];
    }
    let ax = if n[0].abs() < 1.0 / 64.0 && n[1].abs() < 1.0 / 64.0 {
        normalize(cross([0.0, 1.0, 0.0], n))
    } else {
        normalize(cross([0.0, 0.0, 1.0], n))
    };
    let ay = normalize(cross(n, ax));
    [ax, ay, n]
}

pub fn ocs_matrix(normal: Option<Vec3>) -> Mat {
    let [ax, ay, az] = ocs_axes(normal);
    [
        ax[0], ay[0], az[0], 0.0, ax[1], ay[1], az[1], 0.0, ax[2], ay[2], az[2], 0.0, 0.0, 0.0,
        0.0, 1.0,
    ]
}

pub fn is_identity(m: &Mat) -> bool {
    m.iter()
        .zip(IDENTITY.iter())
        .all(|(a, b)| (a - b).abs() < 1e-12)
}

// -- curves -------------------------------------------------------------------

/// Segments so the chord sagitta stays below tolerance (0.1 mm by
/// default); bounded to keep huge arcs sane.
pub fn arc_segments(radius: f64, sweep: f64, tolerance: f64) -> usize {
    let sweep = sweep.abs();
    if !(radius > 0.0 && sweep > 0.0 && radius.is_finite() && sweep.is_finite()) {
        return 1;
    }
    let ratio = (tolerance / radius).min(1.0);
    let step = if ratio < 1.0 {
        2.0 * (1.0 - ratio).acos()
    } else {
        PI / 2.0
    };
    let n = (sweep / step.max(1e-9)).ceil();
    let full = sweep / (2.0 * PI);
    let lo = (8.0 * full).ceil().max(2.0);
    let hi = (256.0 * full).ceil().max(lo);
    // callers pass at most a few turns; this keeps garbage from overflowing
    n.max(lo).min(hi).min(MAX_LINE_POINTS as f64) as usize
}

/// The end angle of an arc from `start` counter-clockwise to `end`, at
/// most one full turn further (more retraces it); None for non-finite
/// angles.
pub fn arc_end(start: f64, mut end: f64) -> Option<f64> {
    if !(start.is_finite() && end.is_finite()) {
        return None;
    }
    // bounded: adding 2 pi to a huge angle changes nothing
    let mut turns = 0;
    while end <= start && turns < 1000 {
        end += 2.0 * PI;
        turns += 1;
    }
    if end <= start || end - start > 2.0 * PI {
        end = start + 2.0 * PI;
    }
    Some(end)
}

pub fn arc_points(center: Vec3, radius: f64, start: f64, end: f64, tolerance: f64) -> Vec<Vec3> {
    let Some(end) = arc_end(start, end) else {
        return vec![];
    };
    let n = arc_segments(radius, end - start, tolerance);
    (0..=n)
        .map(|i| {
            let a = start + (end - start) * i as f64 / n as f64;
            [
                center[0] + radius * a.cos(),
                center[1] + radius * a.sin(),
                center[2],
            ]
        })
        .collect()
}

/// Points from p0 (exclusive) to p1 (inclusive) along a polyline bulge arc.
pub fn bulge_points(p0: Vec3, p1: Vec3, bulge: f64, tolerance: f64) -> Vec<Vec3> {
    if bulge.abs() < 1e-12 {
        return vec![p1];
    }
    let chord = (p1[0] - p0[0]).hypot(p1[1] - p0[1]);
    if chord < 1e-12 {
        return vec![p1];
    }
    let sweep = 4.0 * bulge.atan();
    let radius = chord / (2.0 * (sweep / 2.0).sin().abs());
    let mid = [(p0[0] + p1[0]) / 2.0, (p0[1] + p1[1]) / 2.0];
    // distance from chord midpoint to centre, signed by bulge direction
    let h = radius * (sweep / 2.0).cos();
    let (nx, ny) = (-(p1[1] - p0[1]) / chord, (p1[0] - p0[0]) / chord);
    let sign = if bulge > 0.0 { 1.0 } else { -1.0 };
    let (cx, cy) = (mid[0] + sign * nx * h, mid[1] + sign * ny * h);
    let a0 = (p0[1] - cy).atan2(p0[0] - cx);
    let n = arc_segments(radius, sweep, tolerance);
    let (z0, z1) = (p0[2], p1[2]);
    let mut out: Vec<Vec3> = (1..n)
        .map(|i| {
            let f = i as f64 / n as f64;
            let a = a0 + sweep * f;
            [
                cx + radius * a.cos(),
                cy + radius * a.sin(),
                z0 + (z1 - z0) * f,
            ]
        })
        .collect();
    out.push(p1);
    out
}

/// Evaluate a (rational) B-spline with de Boor's algorithm.
pub fn bspline_points(
    degree: usize,
    ctrl: &[Vec3],
    knots: &[f64],
    weights: &[f64],
    samples: usize,
) -> Vec<Vec3> {
    let n = ctrl.len();
    if n < 2 || knots.len() < n + degree + 1 {
        return ctrl.to_vec();
    }
    let hom: Vec<[f64; 4]> = ctrl
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let w = if weights.len() == n { weights[i] } else { 1.0 };
            [p[0] * w, p[1] * w, p[2] * w, w]
        })
        .collect();
    let (t0, t1) = (knots[degree], knots[n]);
    let mut out = Vec::with_capacity(samples + 1);
    for s in 0..=samples {
        let t = t0 + (t1 - t0) * s as f64 / samples as f64;
        let mut k = degree;
        while k < n - 1 && t >= knots[k + 1] {
            k += 1;
        }
        let mut d: Vec<[f64; 4]> = (0..=degree).map(|j| hom[j + k - degree]).collect();
        for r in 1..=degree {
            for j in (r..=degree).rev() {
                let i = j + k - degree;
                let denom = knots[i + degree - r + 1] - knots[i];
                let a = if denom == 0.0 {
                    0.0
                } else {
                    (t - knots[i]) / denom
                };
                let (prev, cur) = (d[j - 1], d[j]);
                d[j] = std::array::from_fn(|c| (1.0 - a) * prev[c] + a * cur[c]);
            }
        }
        let [x, y, z, w] = d[degree];
        out.push(if w != 0.0 {
            [x / w, y / w, z / w]
        } else {
            [x, y, z]
        });
    }
    out
}

/// Smooth curve through fit points (used when a SPLINE only stores fit data).
pub fn catmull_rom(points: &[Vec3], per_segment: usize) -> Vec<Vec3> {
    if points.len() < 3 {
        return points.to_vec();
    }
    let mut pts = vec![points[0]];
    pts.extend_from_slice(points);
    pts.push(points[points.len() - 1]);
    let mut out = vec![points[0]];
    for i in 1..pts.len() - 2 {
        let (p0, p1, p2, p3) = (pts[i - 1], pts[i], pts[i + 1], pts[i + 2]);
        for s in 1..=per_segment {
            let t = s as f64 / per_segment as f64;
            let (t2, t3) = (t * t, t * t * t);
            out.push(std::array::from_fn(|c| {
                0.5 * (2.0 * p1[c]
                    + (-p0[c] + p2[c]) * t
                    + (2.0 * p0[c] - 5.0 * p1[c] + 4.0 * p2[c] - p3[c]) * t2
                    + (-p0[c] + 3.0 * p1[c] - 3.0 * p2[c] + p3[c]) * t3)
            }));
        }
    }
    out
}

// -- polygons -----------------------------------------------------------------

pub fn newell_normal(pts: &[Vec3]) -> Vec3 {
    let mut n = ZERO;
    for (i, p) in pts.iter().enumerate() {
        let q = pts[(i + 1) % pts.len()];
        n[0] += (p[1] - q[1]) * (p[2] + q[2]);
        n[1] += (p[2] - q[2]) * (p[0] + q[0]);
        n[2] += (p[0] - q[0]) * (p[1] + q[1]);
    }
    n
}

/// Triangles of a polygon (index triples), and whether it needed a
/// fallback: a hole that found no bridge, or a remainder that could only be
/// fanned (a self-intersecting polygon).
#[derive(Debug, Default)]
pub struct Triangulation {
    pub tris: Vec<[usize; 3]>,
    pub degraded: bool,
}

/// Ear-clip a simple planar-ish 3D polygon. The triangles keep the
/// polygon's winding.
pub fn triangulate(pts: &[Vec3]) -> Triangulation {
    let n = pts.len();
    if n < 3 {
        return Triangulation::default();
    }
    if n == 3 {
        return Triangulation {
            tris: vec![[0, 1, 2]],
            degraded: false,
        };
    }
    let normal = newell_normal(pts);
    if length(normal) < 1e-300 {
        // no area at all: whatever comes out is dropped as degenerate
        return Triangulation {
            tris: (1..n - 1).map(|i| [0, i, i + 1]).collect(),
            degraded: false,
        };
    }
    if n == 4 {
        // the diagonal whose worse triangle faces along the quad's normal
        // most (a twisted or bow-tie quad has no split without an inverted
        // triangle; this keeps the inverted one smallest)
        let along = |t: [usize; 3]| {
            dot(
                cross(sub(pts[t[1]], pts[t[0]]), sub(pts[t[2]], pts[t[0]])),
                normal,
            )
        };
        let splits = [[[0, 1, 3], [1, 2, 3]], [[0, 1, 2], [0, 2, 3]]];
        let score = |s: &[[usize; 3]; 2]| along(s[0]).min(along(s[1]));
        let best = if score(&splits[1]) > score(&splits[0]) {
            1
        } else {
            0
        };
        return Triangulation {
            tris: splits[best].to_vec(),
            degraded: false,
        };
    }
    if n <= 32 && !planar(pts, normal) {
        return Triangulation {
            tris: least_area(pts, normal),
            degraded: false,
        };
    }
    // project onto the dominant plane, counter-clockwise
    let axis = dominant_axis(normal);
    let (u, v) = [(1, 2), (2, 0), (0, 1)][axis];
    let sign = if normal[axis] < 0.0 { -1.0 } else { 1.0 };
    let p2: Vec<Vec2> = pts.iter().map(|p| [p[u], sign * p[v]]).collect();
    let ids: Vec<usize> = (0..n).collect();
    earclip(&p2, &ids)
}

/// Whether the points lie in one plane (within 1e-4 of their extent).
fn planar(pts: &[Vec3], normal: Vec3) -> bool {
    let n = normalize(normal);
    let ds: Vec<f64> = pts.iter().map(|&p| dot(p, n)).collect();
    let spread = ds.iter().fold(f64::NEG_INFINITY, |m, &d| m.max(d))
        - ds.iter().fold(f64::INFINITY, |m, &d| m.min(d));
    let size = pts
        .iter()
        .map(|&p| length(sub(p, pts[0])))
        .fold(0.0, f64::max);
    spread <= 1e-4 * size
}

/// The triangulation of a non-planar polygon with the least total area
/// (the least folded surface over its boundary); triangles facing against
/// the polygon's normal count tenfold. O(n^3), for small polygons.
fn least_area(pts: &[Vec3], normal: Vec3) -> Vec<[usize; 3]> {
    let n = pts.len();
    let weight = |i: usize, k: usize, j: usize| {
        let c = cross(sub(pts[k], pts[i]), sub(pts[j], pts[i]));
        let a = length(c) / 2.0;
        if dot(c, normal) < 0.0 { 10.0 * a } else { a }
    };
    // cost[i][j]: best triangulation of the sub-polygon i..=j
    let mut cost = vec![vec![0.0f64; n]; n];
    let mut split = vec![vec![0usize; n]; n];
    for len in 2..n {
        for i in 0..n - len {
            let j = i + len;
            let (best, k) = (i + 1..j)
                .map(|k| (cost[i][k] + cost[k][j] + weight(i, k, j), k))
                .fold((f64::INFINITY, i + 1), |b, c| if c.0 < b.0 { c } else { b });
            (cost[i][j], split[i][j]) = (best, k);
        }
    }
    let mut tris = vec![];
    let mut stack = vec![(0, n - 1)];
    while let Some((i, j)) = stack.pop() {
        if j < i + 2 {
            continue;
        }
        let k = split[i][j];
        tris.push([i, k, j]);
        stack.push((i, k));
        stack.push((k, j));
    }
    tris
}

/// Triangulate a counter-clockwise outer ring with clockwise holes.
///
/// Returns the rings concatenated in order [outer, holes...] and index
/// triples into them. Holes are joined to the outer ring by bridge edges.
pub fn triangulate_2d(outer: &[Vec2], holes: &[Vec<Vec2>]) -> (Vec<Vec2>, Triangulation) {
    triangulate_2d_flat(outer, holes, 0.0)
}

/// [`triangulate_2d`] for rings whose straight runs carry rounding noise:
/// a corner whose sine is at most `flat` counts as straight, so it is no
/// ear, and no diagonal leaves a vertex at such an angle to the boundary
/// (it would cut off a sliver of zero area, triangulated as a fan of
/// zero-area triangles). 0 is plain [`triangulate_2d`].
pub fn triangulate_2d_flat(
    outer: &[Vec2],
    holes: &[Vec<Vec2>],
    flat: f64,
) -> (Vec<Vec2>, Triangulation) {
    let mut pts: Vec<Vec2> = outer.to_vec();
    let mut rings: Vec<Vec<usize>> = Vec::with_capacity(holes.len());
    for hole in holes {
        rings.push((pts.len()..pts.len() + hole.len()).collect());
        pts.extend_from_slice(hole);
    }
    let mut poly: Vec<usize> = (0..outer.len()).collect();
    let max_x = |ring: &[usize]| {
        ring.iter()
            .map(|&i| pts[i][0])
            .fold(f64::NEG_INFINITY, f64::max)
    };
    let mut order: Vec<usize> = (0..rings.len()).collect();
    order.sort_by(|&a, &b| max_x(&rings[b]).total_cmp(&max_x(&rings[a])));
    let mut dropped = false;
    for (pos, &k) in order.iter().enumerate() {
        let ring = &rings[k];
        if ring.len() < 3 {
            continue;
        }
        // rightmost hole vertex (the first one on ties)
        let mut hk = 0;
        for (i, &v) in ring.iter().enumerate() {
            if pts[v][0] > pts[ring[hk]][0] {
                hk = i;
            }
        }
        let h = ring[hk];
        let hp = pts[h];
        let (h_prev, h_next) = (
            pts[ring[(hk + ring.len() - 1) % ring.len()]],
            pts[ring[(hk + 1) % ring.len()]],
        );
        // every edge the bridge must not touch: the polygon so far and the
        // holes not merged yet (this one included)
        let mut edges: Vec<(Vec2, Vec2)> = (0..poly.len())
            .map(|i| (pts[poly[i]], pts[poly[(i + 1) % poly.len()]]))
            .collect();
        for &kk in &order[pos..] {
            let ri = &rings[kk];
            edges.extend((0..ri.len()).map(|i| (pts[ri[i]], pts[ri[(i + 1) % ri.len()]])));
        }
        let mut cands: Vec<usize> = (0..poly.len()).collect();
        cands.sort_by(|&a, &b| d2_2(pts[poly[a]], hp).total_cmp(&d2_2(pts[poly[b]], hp)));
        let m = poly.len();
        let best = cands.into_iter().find(|&i| {
            let q = pts[poly[i]];
            let (q_prev, q_next) = (pts[poly[(i + m - 1) % m]], pts[poly[(i + 1) % m]]);
            q == hp
                || (locally_inside(q_prev, q, q_next, hp)
                    && locally_inside(h_prev, hp, h_next, q)
                    && !edges.iter().any(|&(a, b)| blocks(hp, q, a, b)))
        });
        let Some(best) = best else {
            dropped = true;
            continue;
        };
        let mut seq: Vec<usize> = poly[..=best].to_vec();
        seq.extend_from_slice(&ring[hk..]);
        seq.extend_from_slice(&ring[..hk]);
        seq.push(h);
        seq.push(poly[best]);
        seq.extend_from_slice(&poly[best + 1..]);
        poly = seq;
    }
    let p2: Vec<Vec2> = poly.iter().map(|&i| pts[i]).collect();
    let mut tri = earclip_flat(&p2, &poly, flat);
    tri.degraded |= dropped;
    (pts, tri)
}

/// |sin| of the angle at `o` between the directions to `a` and `b` (0 for a
/// zero-length side).
fn sine(o: Vec2, a: Vec2, b: Vec2) -> f64 {
    let l = (d2_2(o, a) * d2_2(o, b)).sqrt();
    if l > 0.0 {
        orient(o, a, b).abs() / l
    } else {
        0.0
    }
}

fn d2_2(a: Vec2, b: Vec2) -> f64 {
    (a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2)
}

/// Twice the signed area of triangle o, a, b (positive: counter-clockwise).
fn orient(o: Vec2, a: Vec2, b: Vec2) -> f64 {
    (a[0] - o[0]) * (b[1] - o[1]) - (a[1] - o[1]) * (b[0] - o[0])
}

/// Whether the direction from vertex `v` (of a ring that has the region on
/// its left, between `prev` and `next`) towards `d` points into the region.
fn locally_inside(prev: Vec2, v: Vec2, next: Vec2, d: Vec2) -> bool {
    let (after_next, before_prev) = (orient(v, next, d) > 0.0, orient(v, d, prev) > 0.0);
    if orient(prev, v, next) > 0.0 {
        after_next && before_prev
    } else {
        after_next || before_prev
    }
}

fn on_segment(p: Vec2, a: Vec2, b: Vec2) -> bool {
    orient(a, b, p) == 0.0
        && p[0] >= a[0].min(b[0])
        && p[0] <= a[0].max(b[0])
        && p[1] >= a[1].min(b[1])
        && p[1] <= a[1].max(b[1])
}

/// Whether segments p-q and a-b share a point other than an endpoint they
/// have in common (crossing, touching a vertex, or overlapping).
fn blocks(p: Vec2, q: Vec2, a: Vec2, b: Vec2) -> bool {
    let (d1, d2) = (orient(a, b, p), orient(a, b, q));
    let (d3, d4) = (orient(p, q, a), orient(p, q, b));
    if ((d1 > 0.0 && d2 < 0.0) || (d1 < 0.0 && d2 > 0.0))
        && ((d3 > 0.0 && d4 < 0.0) || (d3 < 0.0 && d4 > 0.0))
    {
        return true;
    }
    let inner = |x: Vec2, s: Vec2, t: Vec2| x != s && x != t && on_segment(x, s, t);
    inner(a, p, q) || inner(b, p, q) || inner(p, a, b) || inner(q, a, b)
}

/// Ear-clip a counter-clockwise polygon given as 2D points; returns triples
/// of `ids`. Near-collinear corners are never clipped (their zero-area
/// triangles would leave cracks once dropped); when no ear is left, the
/// remainder is split along a diagonal inside it, and only as a last resort
/// fanned (keeping just the correctly wound triangles).
pub fn earclip(p2: &[Vec2], ids: &[usize]) -> Triangulation {
    earclip_flat(p2, ids, 0.0)
}

fn earclip_flat(p2: &[Vec2], ids: &[usize], flat: f64) -> Triangulation {
    let n = p2.len();
    let mut out = Triangulation::default();
    if n < 3 {
        return out;
    }
    let (mut lo, mut hi) = ([f64::INFINITY; 2], [f64::NEG_INFINITY; 2]);
    for p in p2 {
        for k in 0..2 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    let ext = (hi[0] - lo[0]).max(hi[1] - lo[1]);
    let eps = 1e-12 * ext * ext;
    let polygon: f64 = (0..n)
        .map(|i| orient(p2[0], p2[i], p2[(i + 1) % n]) / 2.0)
        .sum();
    if polygon.abs() <= 1e-9 * ext * ext {
        // a slit: the boundary runs out and back along itself, no area
        return out;
    }
    // bounds the diagonal search (quadratic per remainder)
    let mut budget = 20_000_000usize;
    clip(p2, (0..n).collect(), eps, flat, &mut budget, &mut out);
    absorb_unused(p2, n, &mut out.tris);
    // a fallback is only a defect where the triangles miss or double area
    // (a zero-area slit polygon rightly gives nothing)
    let area = |a: usize, b: usize, c: usize| orient(p2[a], p2[b], p2[c]) / 2.0;
    let covered: f64 = out.tris.iter().map(|&[a, b, c]| area(a, b, c)).sum();
    out.degraded = (covered - polygon).abs() > 1e-6 * polygon.abs() + 1e-12 * ext * ext;
    out.tris = out
        .tris
        .into_iter()
        .map(|[a, b, c]| [ids[a], ids[b], ids[c]])
        .collect();
    out
}

/// Vertices no triangle uses (left over on a straight run of the boundary)
/// would be T-junctions for the neighbouring face: split the triangle edge
/// each one lies on.
fn absorb_unused(p2: &[Vec2], n: usize, tris: &mut Vec<[usize; 3]>) {
    let mut used = vec![false; n];
    for t in tris.iter() {
        for &v in t {
            used[v] = true;
        }
    }
    for v in (0..n).filter(|&v| !used[v]) {
        let p = p2[v];
        // a vertex at a used vertex's position is no gap
        if tris.iter().flatten().any(|&u| p2[u] == p) {
            continue;
        }
        let mut best: Option<(usize, usize, f64)> = None;
        for (ti, t) in tris.iter().enumerate() {
            for k in 0..3 {
                let (a, b) = (p2[t[k]], p2[t[(k + 1) % 3]]);
                let ab = [b[0] - a[0], b[1] - a[1]];
                let l2 = ab[0] * ab[0] + ab[1] * ab[1];
                if l2 == 0.0 {
                    continue;
                }
                let s = ((p[0] - a[0]) * ab[0] + (p[1] - a[1]) * ab[1]) / l2;
                if s <= 0.0 || s >= 1.0 {
                    continue;
                }
                let d = orient(a, b, p).abs() / l2.sqrt();
                if best.is_none_or(|(_, _, bd)| d < bd) {
                    best = Some((ti, k, d));
                }
            }
        }
        if let Some((ti, k, _)) = best {
            let t = tris[ti];
            let (a, b, c) = (t[k], t[(k + 1) % 3], t[(k + 2) % 3]);
            tris[ti] = [a, v, c];
            tris.push([v, b, c]);
        }
    }
}

fn clip(
    p2: &[Vec2],
    mut idx: Vec<usize>,
    eps: f64,
    flat: f64,
    budget: &mut usize,
    out: &mut Triangulation,
) {
    let area2 = |a: usize, b: usize, c: usize| orient(p2[a], p2[b], p2[c]);
    // a corner that clearly turns left
    let convex = |a: usize, b: usize, c: usize| {
        area2(a, b, c) > eps && (flat == 0.0 || sine(p2[b], p2[c], p2[a]) > flat)
    };
    let inside = |p: usize, a: usize, b: usize, c: usize| {
        area2(a, b, p) >= 0.0 && area2(b, c, p) >= 0.0 && area2(c, a, p) >= 0.0
    };
    // keep looking for ears where the last one was cut, not always at the
    // start: always clipping next to vertex 0 fans everything from there
    let mut from = 0;
    loop {
        let m = idx.len();
        if m < 3 {
            return;
        }
        if m == 3 {
            let [a, b, c] = [idx[0], idx[1], idx[2]];
            let ar = area2(a, b, c);
            if ar > eps {
                out.tris.push([a, b, c]);
            } else if ar < -eps {
                out.degraded = true; // a clockwise remainder
            }
            return;
        }
        let corner = |k: usize| (idx[(k + m - 1) % m], idx[k], idx[(k + 1) % m]);
        // only reflex (or flat) vertices can lie inside a candidate ear
        let reflex: Vec<usize> = (0..m)
            .filter(|&k| {
                let (a, b, c) = corner(k);
                !convex(a, b, c)
            })
            .map(|k| idx[k])
            .collect();
        let ear = (0..m).map(|i| (from + i) % m).find(|&k| {
            let (a, b, c) = corner(k);
            // nor may its new side run along the boundary at either end:
            // the rest would start with a sliver of zero area
            let (before, after) = (idx[(k + m - 2) % m], idx[(k + 2) % m]);
            convex(a, b, c)
                && (flat == 0.0
                    || (sine(p2[a], p2[c], p2[before]) > flat
                        && sine(p2[c], p2[a], p2[after]) > flat))
                && !reflex.iter().any(|&p| {
                    p != a
                        && p != b
                        && p != c
                        && p2[p] != p2[a]
                        && p2[p] != p2[b]
                        && p2[p] != p2[c]
                        && inside(p, a, b, c)
                })
        });
        if let Some(k) = ear {
            let (a, b, c) = corner(k);
            out.tris.push([a, b, c]);
            idx.remove(k);
            from = k % (m - 1);
            continue;
        }
        // a vertex repeating its neighbour adds nothing
        if let Some(k) = (0..m).find(|&k| p2[idx[k]] == p2[idx[(k + 1) % m]]) {
            idx.remove(k);
            continue;
        }
        if let Some((i, j)) = find_diagonal(p2, &idx, flat, budget) {
            let a: Vec<usize> = idx[i..=j].to_vec();
            let mut b: Vec<usize> = idx[j..].to_vec();
            b.extend_from_slice(&idx[..=i]);
            clip(p2, a, eps, flat, budget, out);
            clip(p2, b, eps, flat, budget, out);
            return;
        }
        if flat > 0.0 {
            // straight corners everywhere: as without the bound
            clip(p2, idx, eps, 0.0, budget, out);
            return;
        }
        out.degraded = true;
        for k in 1..m - 1 {
            if area2(idx[0], idx[k], idx[k + 1]) > eps {
                out.tris.push([idx[0], idx[k], idx[k + 1]]);
            }
        }
        return;
    }
}

/// A diagonal (i < j, positions in `idx`) that runs inside the polygon
/// without touching its boundary.
fn find_diagonal(
    p2: &[Vec2],
    idx: &[usize],
    flat: f64,
    budget: &mut usize,
) -> Option<(usize, usize)> {
    let m = idx.len();
    let at = |k: usize| p2[idx[k % m]];
    for i in 0..m {
        for j in i + 2..m {
            if i == 0 && j == m - 1 {
                continue; // neighbours
            }
            if *budget < m {
                return None;
            }
            *budget -= m;
            let (p, q) = (at(i), at(j));
            if p == q
                || !locally_inside(at(i + m - 1), p, at(i + 1), q)
                || !locally_inside(at(j + m - 1), q, at(j + 1), p)
            {
                continue;
            }
            // not along the boundary at either end
            if flat > 0.0
                && [
                    (p, q, at(i + m - 1), at(i + 1)),
                    (q, p, at(j + m - 1), at(j + 1)),
                ]
                .iter()
                .any(|&(v, w, a, b)| sine(v, w, a) <= flat || sine(v, w, b) <= flat)
            {
                continue;
            }
            if (0..m).all(|k| !blocks(p, q, at(k), at(k + 1))) {
                return Some((i, j));
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A narrow strip (as a spline's uv domain often is: one parameter
    /// spans a fraction of the other) whose top run is straight, but the
    /// corners at its ends lie a rounding error below it.
    fn noisy_strip() -> Vec<Vec2> {
        let mut ring = vec![[0.0, 1.0 - 4.5e-9]];
        for i in (1..8).rev() {
            ring.push([0.0, i as f64 * 0.125]);
        }
        for i in 0..10 {
            ring.push([i as f64 * 0.005, 0.0]);
        }
        for i in 0..8 {
            ring.push([0.05, i as f64 * 0.125]);
        }
        ring.push([0.05, 1.0 - 4e-9]);
        for i in 1..33 {
            ring.push([0.05 - i as f64 * 0.0015, 1.0]);
        }
        ring
    }

    fn flattest(pts: &[Vec2], tris: &[[usize; 3]]) -> f64 {
        tris.iter()
            .map(|&[a, b, c]| {
                let (p, q, r) = (pts[a], pts[b], pts[c]);
                sine(p, q, r).max(sine(q, r, p)).max(sine(r, p, q))
            })
            .fold(f64::INFINITY, f64::min)
    }

    #[test]
    fn flat_bound_keeps_noisy_runs_out_of_ears_and_diagonals() {
        let ring = noisy_strip();
        let area = |pts: &[Vec2], tris: &[[usize; 3]]| -> f64 {
            tris.iter()
                .map(|&[a, b, c]| orient(pts[a], pts[b], pts[c]) / 2.0)
                .sum()
        };
        let (pts, plain) = triangulate_2d(&ring, &[]);
        assert!(
            flattest(&pts, &plain.tris) < 1e-6,
            "the noise gives zero-area triangles"
        );
        let (pts, tri) = triangulate_2d_flat(&ring, &[], 1e-5);
        assert!(!tri.degraded);
        assert!((area(&pts, &tri.tris) - 0.05).abs() < 1e-6);
        assert!(
            flattest(&pts, &tri.tris) > 1e-4,
            "{}",
            flattest(&pts, &tri.tris)
        );
    }
}
