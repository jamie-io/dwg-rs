// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Boundary representation built from ACIS records: faces with surfaces and
//! loops of edges with curves.
//!
//! Surfaces use their own (u, v) parameterisation with an inverse (point ->
//! u, v); faces are tessellated in that domain. Curves keep ACIS's parameter
//! so edge ranges stored in the file apply. u/v of analytic surfaces are
//! scaled to roughly arc length so the uv domain is not badly distorted.

use std::collections::HashMap;
use std::f64::consts::{FRAC_PI_2, TAU};
use std::rc::Rc;

use super::nurbs::{BSplineCurve, BSplineSurface, clamp_knots};
use super::sab::{Field, Model, Record};
use crate::geom::{Vec2, Vec3, ZERO, add, cross, dist2, dot, length, mul, normalize, sub};

// -- token helpers ---------------------------------------------------------------

/// SAB writes logicals as booleans, SAT as words (forward/reversed, ...),
/// ACIS 1.x SAT as 0/1.
fn flag(f: &Field) -> bool {
    match f {
        Field::Bool(b) => *b,
        Field::Int(i) => *i == 1,
        Field::Str(s) => matches!(s.as_str(), "reversed" | "reverse" | "double" | "in"),
        _ => false,
    }
}

fn vec3(f: &Field) -> Option<Vec3> {
    match f {
        Field::Vec3(v) => Some(*v),
        _ => None,
    }
}

/// SAT stores vectors as loose numbers; group them per geometry type.
fn sat_layout(kind: &str) -> Option<&'static str> {
    Some(match kind {
        "point" => "v",
        "straight-curve" => "vv",
        "ellipse-curve" => "vvvn",
        "plane-surface" => "vvv",
        "cone-surface" => "vvvn",
        "sphere-surface" => "vnvv",
        "torus-surface" => "vvnnv",
        _ => return None,
    })
}

/// Python's max(): NaN-aware the way `>` comparisons are.
#[allow(clippy::neg_cmp_op_on_partial_ord)]
pub(crate) fn py_max(values: impl IntoIterator<Item = f64>) -> Option<f64> {
    values.into_iter().fold(None, |acc, x| match acc {
        Some(m) if !(x > m) => Some(m),
        _ => Some(x),
    })
}

#[derive(Debug)]
struct BadSpline;

/// Sequential reader over a flattened subtype token list.
struct Cursor<'a> {
    t: &'a [Field],
    i: usize,
}

impl Cursor<'_> {
    fn peek(&self) -> Option<&Field> {
        self.t.get(self.i)
    }

    fn next(&mut self) -> Option<&Field> {
        let v = self.t.get(self.i);
        self.i += 1;
        v
    }

    fn num(&mut self) -> Result<f64, BadSpline> {
        match self.next() {
            Some(Field::Enum(v)) => Ok(*v as f64),
            Some(f) => f.num().ok_or(BadSpline),
            None => Err(BadSpline),
        }
    }

    fn int(&mut self) -> Result<i64, BadSpline> {
        let v = self.num()?;
        if v.is_finite() {
            Ok(v.trunc() as i64)
        } else {
            Err(BadSpline)
        }
    }

    /// A count, degree or multiplicity; garbage values are rejected.
    fn count(&mut self) -> Result<usize, BadSpline> {
        usize::try_from(self.int()?)
            .ok()
            .filter(|&n| n < 1_000_000)
            .ok_or(BadSpline)
    }

    /// Tokens not read yet.
    fn left(&self) -> usize {
        self.t.len().saturating_sub(self.i)
    }

    fn knots(&mut self, n: usize) -> Result<Vec<f64>, BadSpline> {
        let mut pairs = vec![];
        // every knot past the first degree + 1 needs a control point, three
        // tokens or more: a vector longer than the data left is garbage (and
        // would be allocated before anything else is checked)
        let mut total = 0usize;
        for _ in 0..n {
            let value = self.num()?;
            // a negative multiplicity contributes nothing
            let mult = self.int()?.max(0);
            let mult = usize::try_from(mult)
                .ok()
                .filter(|&m| m < 1_000_000)
                .ok_or(BadSpline)?;
            total += mult;
            if total > self.t.len() {
                return Err(BadSpline);
            }
            pairs.push((value, mult));
        }
        Ok(clamp_knots(&pairs))
    }

    fn point(&mut self) -> Result<Vec3, BadSpline> {
        Ok([self.num()?, self.num()?, self.num()?])
    }
}

fn flatten(tokens: &[Field], out: &mut Vec<Field>) {
    for t in tokens {
        match t {
            Field::List(inner) => flatten(inner, out),
            // SAB vectors inside spline data
            Field::Vec3(v) => out.extend(v.iter().map(|&c| Field::Float(c))),
            Field::Vec2(v) => out.extend(v.iter().map(|&c| Field::Float(c))),
            _ => out.push(t.clone()),
        }
    }
}

enum Bs3 {
    Curve(BSplineCurve),
    Surface(BSplineSurface),
}

/// Parse an ACIS bs3_curve / bs3_surface whose 'nubs'/'nurbs' marker is at i.
fn bs3_at(tokens: &[Field], i: usize, surface: bool) -> Result<Bs3, BadSpline> {
    let mut c = Cursor { t: tokens, i };
    let rational = c.next().is_some_and(|f| f.is_type("nurbs"));
    if surface {
        let (du, dv) = (c.count()?, c.count()?);
        if rational && matches!(c.peek(), Some(Field::Type(_))) {
            c.next(); // which directions are rational: both / u / v
        }
        // closure u, v (open / closed / periodic), then singularity u, v
        let closed = |f: Option<&Field>| match f {
            Some(Field::Enum(v) | Field::Int(v)) => *v != 0,
            Some(Field::Type(t)) => matches!(t.as_str(), "closed" | "periodic"),
            _ => false,
        };
        let (closed_u, closed_v) = (closed(c.next()), closed(c.next()));
        c.next();
        c.next();
        let (nku, nkv) = (c.count()?, c.count()?);
        let ku = c.knots(nku)?;
        let kv = c.knots(nkv)?;
        let (nu, nv) = (
            ku.len().saturating_sub(du + 1),
            kv.len().saturating_sub(dv + 1),
        );
        if nu < 1 || nv < 1 || nu <= du || nv <= dv {
            return Err(BadSpline);
        }
        // three tokens or more per control point must still be there
        if nu
            .checked_mul(nv)
            .and_then(|n| n.checked_mul(3))
            .is_none_or(|n| n > c.left())
        {
            return Err(BadSpline);
        }
        let mut ctrl = vec![vec![ZERO; nv]; nu];
        let mut weights = vec![vec![1.0; nv]; nu];
        // ACIS writes control points with u varying fastest
        for j in 0..nv {
            for i in 0..nu {
                ctrl[i][j] = c.point()?;
                if rational {
                    weights[i][j] = c.num()?;
                }
            }
        }
        let mut srf = BSplineSurface::new(du, dv, ku, kv, ctrl, rational.then_some(weights));
        (srf.closed_u, srf.closed_v) = (closed_u, closed_v);
        return Ok(Bs3::Surface(srf));
    }
    let degree = c.count()?;
    c.next(); // closure
    let nk = c.count()?;
    let knots = c.knots(nk)?;
    let n = knots.len().saturating_sub(degree + 1);
    if n < 2 || n <= degree || n.saturating_mul(3) > c.left() {
        return Err(BadSpline);
    }
    let mut ctrl = Vec::with_capacity(n);
    let mut weights = Vec::with_capacity(n);
    for _ in 0..n {
        ctrl.push(c.point()?);
        if rational {
            weights.push(c.num()?);
        }
    }
    Ok(Bs3::Curve(BSplineCurve::new(
        degree,
        knots,
        ctrl,
        rational.then_some(weights),
    )))
}

/// A 2D curve in a surface's parameter space (bs2_curve) whose
/// 'nubs'/'nurbs' marker is at i: (degree, knots, control points, weights).
fn bs2_at(
    tokens: &[Field],
    i: usize,
) -> Result<(usize, Vec<f64>, Vec<Vec2>, Option<Vec<f64>>), BadSpline> {
    let mut c = Cursor { t: tokens, i };
    let rational = c.next().is_some_and(|f| f.is_type("nurbs"));
    let degree = c.count()?;
    c.next(); // closure
    let nk = c.count()?;
    let knots = c.knots(nk)?;
    let n = knots.len().saturating_sub(degree + 1);
    if n < 2 || n <= degree || n.saturating_mul(2) > c.left() {
        return Err(BadSpline);
    }
    let mut ctrl = Vec::with_capacity(n);
    let mut weights = Vec::with_capacity(n);
    for _ in 0..n {
        ctrl.push([c.num()?, c.num()?]);
        if rational {
            weights.push(c.num()?);
        }
    }
    Ok((degree, knots, ctrl, rational.then_some(weights)))
}

/// A curve given as a surface along a 2D parameter curve (ACIS `parcur`),
/// sampled into a polyline B-spline with the 2D curve's parameters. ACIS 7.0
/// files (DWG 2007, 2010) keep only the knots of such an intcurve's 3D
/// approximation, so the curve has to come from its surface.
fn curve_on_surface(srf: &BSplineSurface, uv: &BSplineCurve) -> Option<BSplineCurve> {
    const SAMPLES: usize = 512;
    let (t0, t1) = (uv.t0, uv.t1);
    if !(t0.is_finite() && t1.is_finite() && t1 > t0) {
        return None;
    }
    let ts: Vec<f64> = (0..=SAMPLES)
        .map(|k| t0 + (t1 - t0) * k as f64 / SAMPLES as f64)
        .collect();
    let ctrl: Vec<Vec3> = ts
        .iter()
        .map(|&t| {
            let p = uv.eval(t);
            srf.eval(p[0], p[1])
        })
        .collect();
    if !ctrl.iter().flatten().all(|c| c.is_finite()) {
        return None;
    }
    let mut knots = Vec::with_capacity(ts.len() + 2);
    knots.push(t0);
    knots.extend(&ts);
    knots.push(t1);
    Some(BSplineCurve::new(1, knots, ctrl, None))
}

/// Every bs3 curve/surface definition in a subtype's token list. Procedural
/// subtypes (blends, sweeps, offsets) also embed their support geometry, so
/// the caller picks the candidate that fits the topology.
fn all_bs3(tokens: &[Field], surface: bool) -> Vec<Bs3> {
    let mut found = vec![];
    for (i, t) in tokens.iter().enumerate() {
        if t.is_type("nubs") || t.is_type("nurbs") {
            // surfaces have two degrees in a row
            let next_is_num = tokens.get(i + 2).is_some_and(Field::is_num);
            if next_is_num != surface {
                continue;
            }
            if let Ok(bs) = bs3_at(tokens, i, surface) {
                found.push(bs);
            }
        }
    }
    found
}

// -- curves -----------------------------------------------------------------------

#[derive(Debug, Clone)]
pub enum Curve {
    /// P = root + t * dir
    Straight {
        root: Vec3,
        dir: Vec3,
    },
    Ellipse {
        center: Vec3,
        major: Vec3,
        minor: Vec3,
    },
    Spline(BSplineCurve),
}

impl Curve {
    pub fn straight(root: Vec3, dir: Vec3) -> Curve {
        Curve::Straight { root, dir }
    }

    pub fn ellipse(center: Vec3, normal: Vec3, major: Vec3, ratio: f64) -> Curve {
        Curve::Ellipse {
            center,
            major,
            minor: mul(cross(normalize(normal), major), ratio),
        }
    }

    pub fn eval(&self, t: f64) -> Vec3 {
        match self {
            Curve::Straight { root, dir } => add(*root, mul(*dir, t)),
            Curve::Ellipse {
                center,
                major,
                minor,
            } => add(*center, add(mul(*major, t.cos()), mul(*minor, t.sin()))),
            Curve::Spline(bs) => bs.eval(t),
        }
    }

    /// The parameter of a point on the curve: exact for analytic curves, the
    /// nearest point for splines (old SAT edges carry no parameters).
    pub fn param(&self, p: Vec3) -> Option<f64> {
        match self {
            Curve::Straight { root, dir } => {
                Some(dot(sub(p, *root), *dir) / nonzero(dot(*dir, *dir)))
            }
            Curve::Ellipse {
                center,
                major,
                minor,
            } => {
                let d = sub(p, *center);
                Some(
                    (dot(d, *minor) / nonzero(dot(*minor, *minor)))
                        .atan2(dot(d, *major) / nonzero(dot(*major, *major))),
                )
            }
            Curve::Spline(bs) => spline_param(bs, p),
        }
    }

    pub fn period(&self) -> Option<f64> {
        matches!(self, Curve::Ellipse { .. }).then_some(TAU)
    }
}

/// The parameter of the point on `bs` nearest to `p`: the closest of dense
/// samples, refined by golden-section search between its neighbours.
fn spline_param(bs: &BSplineCurve, p: Vec3) -> Option<f64> {
    let (t0, t1) = (bs.t0, bs.t1);
    if !(t0.is_finite() && t1.is_finite() && t1 >= t0) {
        return None;
    }
    let n = (8 * bs.ctrl.len()).clamp(64, 4096);
    let at = |k: usize| t0 + (t1 - t0) * k as f64 / n as f64;
    let d = |t: f64| dist2(bs.eval(t), p);
    let best = (0..=n).min_by(|&a, &b| d(at(a)).total_cmp(&d(at(b))))?;
    let (mut a, mut b) = (at(best.saturating_sub(1)), at((best + 1).min(n)));
    const G: f64 = 0.618_033_988_749_894_8;
    for _ in 0..60 {
        let (x1, x2) = (b - G * (b - a), a + G * (b - a));
        if d(x1) <= d(x2) {
            b = x2;
        } else {
            a = x1;
        }
    }
    let t = 0.5 * (a + b);
    t.is_finite().then_some(t)
}

/// An edge from a vertex back to it, on a spline that is closed too.
fn closed_spline_edge(bs: &BSplineCurve, p0: Vec3, p1: Vec3) -> bool {
    let size = bs
        .ctrl
        .iter()
        .map(|&q| length(sub(q, bs.ctrl[0])))
        .fold(0.0, f64::max);
    let near = |a: Vec3, b: Vec3| length(sub(a, b)) <= 1e-6 * size;
    size > 0.0 && near(p0, p1) && near(bs.eval(bs.t0), bs.eval(bs.t1))
}

/// `x or 1.0`
fn nonzero(x: f64) -> f64 {
    if x != 0.0 { x } else { 1.0 }
}

// -- surfaces ---------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Plane {
    o: Vec3,
    u: Vec3,
    v: Vec3,
}

impl Plane {
    pub fn new(root: Vec3, normal: Vec3, udir: Vec3) -> Plane {
        let n = normalize(normal);
        let u = if length(udir) > 1e-12 {
            normalize(udir)
        } else {
            any_perp(n)
        };
        Plane {
            o: root,
            u,
            v: cross(n, u),
        }
    }
}

/// Elliptic cone/cylinder: base ellipse (centre, axis, major axis, ratio),
/// half angle given as sin/cos. u = arc length on the base, v = axial distance.
#[derive(Debug, Clone)]
pub struct Cone {
    c: Vec3,
    a: Vec3,
    r: f64,
    x: Vec3,
    y: Vec3,
    ratio: f64,
    slope: f64,
}

impl Cone {
    pub fn new(center: Vec3, axis: Vec3, major: Vec3, ratio: f64, sin_a: f64, cos_a: f64) -> Cone {
        let a = normalize(axis);
        let x = normalize(major);
        Cone {
            c: center,
            a,
            r: length(major),
            x,
            y: cross(a, x),
            ratio: nonzero(ratio),
            slope: if cos_a.abs() > 1e-12 {
                sin_a / cos_a
            } else {
                0.0
            },
        }
    }

    /// The sign convention of the half angle varies; take the other sign
    /// only when it reproduces the face's own boundary points clearly better
    /// (by more than `tol`). Points on the base ellipse fit either sign, so
    /// a face bounded by it alone keeps the stored sign.
    pub fn fit_slope_sign(&mut self, points: &[Vec3], tol: f64) {
        if self.slope == 0.0 || points.is_empty() {
            return;
        }
        let err = |cone: &Cone| -> f64 {
            points
                .iter()
                .map(|&p| {
                    let [u, v] = cone.inverse(p);
                    length(sub(cone.eval(u, v), p))
                })
                .sum()
        };
        let e1 = err(self);
        self.slope = -self.slope;
        if err(self) >= e1 - tol {
            self.slope = -self.slope;
        }
    }

    fn eval(&self, u: f64, v: f64) -> Vec3 {
        let ang = u / self.r;
        let r = (self.r + v * self.slope) / self.r;
        let radial = add(
            mul(self.x, self.r * r * ang.cos()),
            mul(self.y, self.r * r * self.ratio * ang.sin()),
        );
        add(self.c, add(mul(self.a, v), radial))
    }

    fn inverse(&self, p: Vec3) -> Vec2 {
        let d = sub(p, self.c);
        let ang = (dot(d, self.y) / self.ratio).atan2(dot(d, self.x));
        [ang * self.r, dot(d, self.a)]
    }
}

#[derive(Debug, Clone)]
pub struct Sphere {
    c: Vec3,
    r: f64,
    a: Vec3,
    x: Vec3,
    y: Vec3,
}

impl Sphere {
    pub fn new(center: Vec3, radius: f64, udir: Vec3, pole: Vec3) -> Sphere {
        let a = normalize(pole);
        let x = if length(udir) > 1e-12 {
            normalize(udir)
        } else {
            any_perp(a)
        };
        Sphere {
            c: center,
            r: radius.abs(),
            a,
            x,
            y: cross(a, x),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Torus {
    c: Vec3,
    a: Vec3,
    big_r: f64,
    small_r: f64,
    x: Vec3,
    y: Vec3,
    su: f64,
    sv: f64,
}

impl Torus {
    pub fn new(center: Vec3, axis: Vec3, major: f64, minor: f64, udir: Vec3) -> Torus {
        let a = normalize(axis);
        let x = if length(udir) > 1e-12 {
            normalize(udir)
        } else {
            any_perp(a)
        };
        Torus {
            c: center,
            a,
            big_r: major,
            small_r: minor.abs(),
            x,
            y: cross(a, x),
            su: major.abs().max(1e-9),
            sv: minor.abs().max(1e-9),
        }
    }
}

#[derive(Debug, Clone)]
pub struct Spline {
    bs: BSplineSurface,
    /// start points for the inverse: a few samples per control span
    grid: Vec<(Vec2, Vec3)>,
}

impl Spline {
    pub fn new(bs: BSplineSurface) -> Spline {
        let (nu, nv) = ((3 * bs.nu).min(150), (3 * bs.nv).min(150));
        let mut grid = Vec::with_capacity((nu + 1) * (nv + 1));
        for i in 0..=nu {
            for j in 0..=nv {
                let uv = [
                    bs.u0 + (bs.u1 - bs.u0) * (i as f64 / nu as f64),
                    bs.v0 + (bs.v1 - bs.v0) * (j as f64 / nv as f64),
                ];
                grid.push((uv, bs.eval(uv[0], uv[1])));
            }
        }
        Spline { bs, grid }
    }

    /// A parameter inside the domain: wrapped where the surface is closed,
    /// clamped elsewhere.
    fn domain_u(&self, u: f64) -> f64 {
        let bs = &self.bs;
        if bs.closed_u && bs.u1 > bs.u0 {
            bs.u0 + (u - bs.u0).rem_euclid(bs.u1 - bs.u0)
        } else {
            u.max(bs.u0).min(bs.u1)
        }
    }

    fn domain_v(&self, v: f64) -> f64 {
        let bs = &self.bs;
        if bs.closed_v && bs.v1 > bs.v0 {
            bs.v0 + (v - bs.v0).rem_euclid(bs.v1 - bs.v0)
        } else {
            v.max(bs.v0).min(bs.v1)
        }
    }

    fn eval(&self, u: f64, v: f64) -> Vec3 {
        self.bs.eval(self.domain_u(u), self.domain_v(v))
    }

    /// The parameter where a boundary row/column of control points collapses
    /// to one point (a pole, e.g. the tip of a dome): u (`along_u`) or v,
    /// at the upper or lower end of the domain.
    fn pole(&self, along_u: bool, upper: bool) -> Option<f64> {
        let bs = &self.bs;
        let row: Vec<Vec3> = if along_u {
            let i = if upper { bs.nu - 1 } else { 0 };
            bs.ctrl[i].clone()
        } else {
            let j = if upper { bs.nv - 1 } else { 0 };
            bs.ctrl.iter().map(|r| r[j]).collect()
        };
        let mut lo = [f64::INFINITY; 3];
        let mut hi = [f64::NEG_INFINITY; 3];
        for p in bs.ctrl.iter().flatten() {
            for k in 0..3 {
                lo[k] = lo[k].min(p[k]);
                hi[k] = hi[k].max(p[k]);
            }
        }
        let eps = 1e-9 * length(sub(hi, lo)).max(1e-300);
        let at = match (along_u, upper) {
            (true, true) => bs.u1,
            (true, false) => bs.u0,
            (false, true) => bs.v1,
            (false, false) => bs.v0,
        };
        row.iter()
            .all(|&p| length(sub(p, row[0])) <= eps)
            .then_some(at)
    }

    fn inverse(&self, p: Vec3) -> Vec2 {
        let mut best = 0;
        for (k, g) in self.grid.iter().enumerate() {
            if dist2(g.1, p) < dist2(self.grid[best].1, p) {
                best = k;
            }
        }
        let [mut u, mut v] = self.grid[best].0;
        let bs = &self.bs;
        let (hu, hv) = ((bs.u1 - bs.u0) * 1e-6, (bs.v1 - bs.v0) * 1e-6);
        for _ in 0..30 {
            // Gauss-Newton on |S(u,v) - p|
            let s = self.eval(u, v);
            // one-sided differences that stay inside the domain (closed
            // directions wrap)
            let du_h = if bs.closed_u || u + hu <= bs.u1 {
                hu
            } else {
                -hu
            };
            let dv_h = if bs.closed_v || v + hv <= bs.v1 {
                hv
            } else {
                -hv
            };
            let su = mul(sub(self.eval(u + du_h, v), s), 1.0 / du_h);
            let sv = mul(sub(self.eval(u, v + dv_h), s), 1.0 / dv_h);
            let r = sub(s, p);
            let (a, b, c) = (dot(su, su), dot(su, sv), dot(sv, sv));
            let (gu, gv) = (dot(su, r), dot(sv, r));
            let det = a * c - b * b;
            let (du, dv) = if det.abs() > 1e-12 * (a * c).max(1e-300) {
                ((c * gu - b * gv) / det, (a * gv - b * gu) / det)
            } else {
                // degenerate direction (pole): move along the other one only
                (
                    if a > 1e-300 { gu / a } else { 0.0 },
                    if c > 1e-300 { gv / c } else { 0.0 },
                )
            };
            let (nu, nv) = (self.domain_u(u - du), self.domain_v(v - dv));
            if (nu - u).abs() < hu * 1e-2 && (nv - v).abs() < hv * 1e-2 {
                break;
            }
            (u, v) = (nu, nv);
        }
        [u, v]
    }
}

/// A circle of radius `r` swept along a spine curve. ACIS pipe surfaces are
/// this, and so are rolling-ball blends: the ball centre runs along the spine
/// and the blend is the part of the pipe between the two contact curves.
/// u: arc length along the spine, v: angle around it times `r`, measured
/// from a rotation-minimising frame. Beyond its ends the spine continues
/// along its end tangents.
#[derive(Debug, Clone)]
pub struct Pipe {
    spine: BSplineCurve,
    pub r: f64,
    /// (t, arc length, point, frame normal) along the spine
    samples: Vec<(f64, f64, Vec3, Vec3)>,
}

impl Pipe {
    pub fn new(spine: BSplineCurve, r: f64) -> Pipe {
        let n = (16 * spine.ctrl.len()).clamp(64, 1024);
        let (t0, t1) = (spine.t0, spine.t1);
        let mut samples: Vec<(f64, f64, Vec3, Vec3)> = Vec::with_capacity(n + 1);
        for i in 0..=n {
            let t = t0 + (t1 - t0) * (i as f64 / n as f64);
            let p = spine.eval(t);
            let Some(&(_, s0, p0, n0)) = samples.last() else {
                samples.push((t, 0.0, p, any_perp(Self::tangent_of(&spine, t))));
                continue;
            };
            // double reflection (Wang et al. 2008)
            let v1 = sub(p, p0);
            let c1 = dot(v1, v1);
            let t_prev = Self::tangent_of(&spine, samples[samples.len() - 1].0);
            let t_next = Self::tangent_of(&spine, t);
            let (r_l, t_l) = if c1 > 0.0 {
                (
                    sub(n0, mul(v1, 2.0 * dot(v1, n0) / c1)),
                    sub(t_prev, mul(v1, 2.0 * dot(v1, t_prev) / c1)),
                )
            } else {
                (n0, t_prev)
            };
            let v2 = sub(t_next, t_l);
            let c2 = dot(v2, v2);
            let nn = if c2 > 0.0 {
                sub(r_l, mul(v2, 2.0 * dot(v2, r_l) / c2))
            } else {
                r_l
            };
            samples.push((t, s0 + c1.sqrt(), p, normalize(nn)));
        }
        Pipe { spine, r, samples }
    }

    fn tangent_of(spine: &BSplineCurve, t: f64) -> Vec3 {
        let h = (spine.t1 - spine.t0) * 1e-6;
        let (a, b) = ((t - h).max(spine.t0), (t + h).min(spine.t1));
        normalize(sub(spine.eval(b), spine.eval(a)))
    }

    fn length(&self) -> f64 {
        self.samples[self.samples.len() - 1].1
    }

    /// Spine point, tangent and frame normal at arc length u.
    fn frame(&self, u: f64) -> (Vec3, Vec3, Vec3) {
        let last = self.samples.len() - 1;
        let (end, s_end) = if u <= 0.0 {
            (0, 0.0)
        } else if u >= self.length() {
            (last, self.length())
        } else {
            let k = self.samples.partition_point(|s| s.1 <= u).clamp(1, last);
            let (a, b) = (&self.samples[k - 1], &self.samples[k]);
            let f = (u - a.1) / (b.1 - a.1).max(1e-300);
            let t = a.0 + (b.0 - a.0) * f;
            let tan = Self::tangent_of(&self.spine, t);
            let n = add(mul(a.3, 1.0 - f), mul(b.3, f));
            let n = normalize(sub(n, mul(tan, dot(n, tan))));
            return (self.spine.eval(t), tan, n);
        };
        let (t, _, p, n) = self.samples[end];
        let tan = Self::tangent_of(&self.spine, t);
        (add(p, mul(tan, u - s_end)), tan, n)
    }

    pub fn eval(&self, u: f64, v: f64) -> Vec3 {
        let (c, tan, n) = self.frame(u);
        let b = cross(tan, n);
        let a = v / self.r;
        add(c, mul(add(mul(n, a.cos()), mul(b, a.sin())), self.r))
    }

    /// Arc length of the spine point closest to p.
    fn closest_u(&self, p: Vec3) -> f64 {
        let mut best = (f64::INFINITY, 0.0);
        for w in self.samples.windows(2) {
            let (a, b) = (w[0].2, w[1].2);
            let ab = sub(b, a);
            let l2 = dot(ab, ab);
            let f = if l2 > 0.0 {
                (dot(sub(p, a), ab) / l2).clamp(0.0, 1.0)
            } else {
                0.0
            };
            let d = dist2(add(a, mul(ab, f)), p);
            if d < best.0 {
                best = (d, w[0].1 + (w[1].1 - w[0].1) * f);
            }
        }
        // beyond an end: along the end tangent
        let mut u = best.1;
        for _ in 0..8 {
            let (c, tan, _) = self.frame(u);
            let du = dot(sub(p, c), tan);
            u += du;
            if du.abs() < 1e-12 * self.length().max(1e-300) {
                break;
            }
        }
        u
    }

    pub fn inverse(&self, p: Vec3) -> Vec2 {
        let u = self.closest_u(p);
        let (c, tan, n) = self.frame(u);
        let d = sub(p, c);
        let b = cross(tan, n);
        [u, dot(d, b).atan2(dot(d, n)) * self.r]
    }

    /// Distance of p from the spine.
    pub fn spine_distance(&self, p: Vec3) -> f64 {
        let (c, _, _) = self.frame(self.closest_u(p));
        length(sub(p, c))
    }
}

/// The pipe around one of the candidate spines that the points lie on, with
/// their largest deviation from it.
pub fn best_pipe(spines: &[BSplineCurve], points: &[Vec3]) -> (Option<Pipe>, f64) {
    let (mut best, mut err) = (None, f64::INFINITY);
    let step = (points.len() / 48).max(1);
    let sample: Vec<Vec3> = points.iter().step_by(step).copied().collect();
    if sample.is_empty() {
        return (None, err);
    }
    for spine in spines {
        let pipe = Pipe::new(spine.clone(), 1.0);
        let d: Vec<f64> = sample.iter().map(|&p| pipe.spine_distance(p)).collect();
        let r = d.iter().sum::<f64>() / d.len() as f64;
        let e = py_max(d.iter().map(|x| (x - r).abs())).unwrap_or(0.0);
        if r > 0.0 && e < err {
            let mut pipe = pipe;
            pipe.r = r;
            (best, err) = (Some(pipe), e);
        }
    }
    (best, err)
}

/// A face's surface with its own (u, v) parameterisation.
#[derive(Debug, Clone)]
pub enum Surface {
    Plane(Plane),
    Cone(Cone),
    Sphere(Sphere),
    Torus(Torus),
    Spline(Spline),
    Pipe(Pipe),
}

impl Surface {
    pub fn eval(&self, u: f64, v: f64) -> Vec3 {
        match self {
            Surface::Plane(s) => add(s.o, add(mul(s.u, u), mul(s.v, v))),
            Surface::Cone(s) => s.eval(u, v),
            Surface::Sphere(s) => {
                let (lon, lat) = (u / s.r, v / s.r);
                let cl = lat.cos();
                add(
                    s.c,
                    mul(
                        add(
                            add(mul(s.x, cl * lon.cos()), mul(s.y, cl * lon.sin())),
                            mul(s.a, lat.sin()),
                        ),
                        s.r,
                    ),
                )
            }
            Surface::Torus(s) => {
                let (th, ph) = (u / s.su, v / s.sv);
                let rad = s.big_r + s.small_r * ph.cos();
                add(
                    s.c,
                    add(
                        add(mul(s.x, rad * th.cos()), mul(s.y, rad * th.sin())),
                        mul(s.a, s.small_r * ph.sin()),
                    ),
                )
            }
            Surface::Spline(s) => s.eval(u, v),
            Surface::Pipe(s) => s.eval(u, v),
        }
    }

    pub fn inverse(&self, p: Vec3) -> Vec2 {
        match self {
            Surface::Plane(s) => {
                let d = sub(p, s.o);
                [dot(d, s.u), dot(d, s.v)]
            }
            Surface::Cone(s) => s.inverse(p),
            Surface::Sphere(s) => {
                let d = sub(p, s.c);
                let z = (dot(d, s.a) / s.r).clamp(-1.0, 1.0);
                [dot(d, s.y).atan2(dot(d, s.x)) * s.r, z.asin() * s.r]
            }
            Surface::Torus(s) => {
                let d = sub(p, s.c);
                let (x, y, z) = (dot(d, s.x), dot(d, s.y), dot(d, s.a));
                [y.atan2(x) * s.su, z.atan2(x.hypot(y) - s.big_r) * s.sv]
            }
            Surface::Spline(s) => s.inverse(p),
            Surface::Pipe(s) => s.inverse(p),
        }
    }

    /// Period of u (0 = not periodic).
    pub fn period_u(&self) -> f64 {
        match self {
            Surface::Spline(s) if s.bs.closed_u => s.bs.u1 - s.bs.u0,
            Surface::Cone(s) => TAU * s.r,
            Surface::Sphere(s) => TAU * s.r,
            Surface::Torus(s) => TAU * s.su,
            _ => 0.0,
        }
    }

    /// Period of v (0 = not periodic).
    pub fn period_v(&self) -> f64 {
        match self {
            Surface::Spline(s) if s.bs.closed_v => s.bs.v1 - s.bs.v0,
            Surface::Torus(s) => TAU * s.sv,
            Surface::Pipe(s) => TAU * s.r,
            _ => 0.0,
        }
    }

    /// v of the degenerate edge (apex/pole) above or below, if any.
    pub fn pole_v(&self, upper: bool) -> Option<f64> {
        match self {
            Surface::Cone(s) if s.slope != 0.0 => {
                let v = -s.r / s.slope; // apex
                ((v > 0.0) == upper).then_some(v)
            }
            Surface::Sphere(s) => Some(if upper {
                FRAC_PI_2 * s.r
            } else {
                -FRAC_PI_2 * s.r
            }),
            Surface::Spline(s) => s.pole(false, upper),
            _ => None,
        }
    }

    /// u of a degenerate edge at the upper or lower end, if any (splines).
    pub fn pole_u(&self, upper: bool) -> Option<f64> {
        match self {
            Surface::Spline(s) => s.pole(true, upper),
            _ => None,
        }
    }

    /// The parameter domain where it is bounded (splines), for faces without
    /// loops.
    pub fn domain(&self) -> Option<[f64; 4]> {
        match self {
            Surface::Spline(s) => Some([s.bs.u0, s.bs.u1, s.bs.v0, s.bs.v1]),
            _ => None,
        }
    }
}

/// What a face's surface record describes.
#[derive(Debug, Clone)]
pub enum SurfaceDef {
    Analytic(Surface),
    /// Candidate NURBS for a spline surface; resolved per face by fit.
    SplineChoice(Vec<BSplineSurface>),
    /// Candidate spines of a pipe or rolling-ball blend saved without an
    /// approximating NURBS; resolved per face by fit.
    PipeChoice(Vec<BSplineCurve>),
}

/// The candidate that passes closest to the points, and its largest deviation.
pub fn best_spline(cands: &[BSplineSurface], points: &[Vec3]) -> (Option<Spline>, f64) {
    let (mut best, mut err) = (None, f64::INFINITY);
    let step = (points.len() / 24).max(1);
    let sample: Vec<Vec3> = points.iter().step_by(step).copied().collect();
    for bs in cands {
        let s = Spline::new(bs.clone());
        let e = py_max(sample.iter().map(|&p| {
            let [u, v] = s.inverse(p);
            length(sub(s.eval(u, v), p))
        }))
        .unwrap_or(0.0);
        if e < err {
            (best, err) = (Some(s), e);
        }
    }
    (best, err)
}

fn any_perp(n: Vec3) -> Vec3 {
    normalize(cross(
        n,
        if n[0].abs() < 0.9 {
            [1.0, 0.0, 0.0]
        } else {
            [0.0, 1.0, 0.0]
        },
    ))
}

// -- topology ---------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct Edge {
    pub index: usize,
    pub start: Vec3,
    pub end: Vec3,
    pub t0: f64,
    pub t1: f64,
    pub curve: Option<Curve>,
    pub reversed: bool,
}

/// (edge, coedge reversed)
pub type Loop = Vec<(Rc<Edge>, bool)>;

#[derive(Debug, Clone)]
pub struct Face {
    pub surface: Option<SurfaceDef>,
    pub surface_type: String,
    /// Face sense: the outward normal is opposite to the surface normal.
    pub reversed: bool,
    /// The surface record's normal is opposite to the parametric normal
    /// Su x Sv of [`Surface`] (cone with cos < 0, sphere with radius < 0,
    /// torus with minor radius < 0, spline surface marked reversed).
    pub surface_reversed: bool,
    pub loops: Vec<Loop>,
}

/// Row-vector 3x3 matrix, translation, scale: `p' = scale * (p * A) + t`.
pub type Transform = ([f64; 9], Vec3, f64);

#[derive(Debug, Clone)]
pub struct Body {
    pub faces: Vec<Face>,
    pub transform: Option<Transform>,
}

/// Reads bodies from an ACIS [`Model`].
pub struct Brep<'a> {
    m: &'a Model,
    /// number of leading fields shared by all entities (attrib, history, ...)
    pre: usize,
    subtypes: Vec<Rc<Vec<Field>>>,
    edges: HashMap<usize, Rc<Edge>>,
    pub unknown_surfaces: HashMap<String, usize>,
}

impl<'a> Brep<'a> {
    pub fn new(model: &'a Model) -> Brep<'a> {
        let mut brep = Brep {
            m: model,
            pre: 0,
            subtypes: vec![],
            edges: HashMap::new(),
            unknown_surfaces: HashMap::new(),
        };
        brep.pre = brep.detect_prefix();
        fn walk(tokens: &[Field], out: &mut Vec<Rc<Vec<Field>>>) {
            for t in tokens {
                if let Field::List(list) = t {
                    if list.first().is_some_and(|f| !f.is_type("ref")) {
                        out.push(list.clone());
                    }
                    walk(list, out);
                }
            }
        }
        for r in &model.records {
            walk(&r.fields, &mut brep.subtypes);
        }
        brep
    }

    fn detect_prefix(&self) -> usize {
        // lump: <prefix> next_lump shell body
        if let Some(lump) = self.m.records.iter().find(|r| r.kind == "lump") {
            return lump.fields.len().saturating_sub(3).max(1);
        }
        if self.m.version >= 21500 {
            3
        } else if self.m.version >= 700 {
            2
        } else {
            1
        }
    }

    fn subtype(&self, rec: &Record) -> Option<Rc<Vec<Field>>> {
        let list = rec.fields.iter().find_map(|f| match f {
            Field::List(list) => Some(list),
            _ => None,
        })?;
        if list.first().is_some_and(|f| f.is_type("ref")) {
            if let Some(idx) = list.get(1).and_then(Field::num) {
                return usize::try_from(idx as i64)
                    .ok()
                    .and_then(|i| self.subtypes.get(i))
                    .cloned();
            }
        }
        Some(list.clone())
    }

    /// `tokens` with every nested `ref n` replaced by subtype n's tokens.
    fn flatten_refs(&self, tokens: &[Field], out: &mut Vec<Field>, depth: usize) {
        let mut i = 0;
        while i < tokens.len() {
            if depth < 8 && tokens[i].is_type("ref") {
                let sub = tokens
                    .get(i + 1)
                    .and_then(Field::num)
                    .and_then(|n| usize::try_from(n as i64).ok())
                    .and_then(|n| self.subtypes.get(n));
                if let Some(sub) = sub {
                    let mut flat = vec![];
                    flatten(sub, &mut flat);
                    self.flatten_refs(&flat, out, depth + 1);
                    i += 2;
                    continue;
                }
            }
            out.push(tokens[i].clone());
            i += 1;
        }
    }

    /// The fields after the common prefix; SAT vectors grouped.
    fn f(&self, rec: &Record) -> Vec<Field> {
        let fields = rec.fields.get(self.pre..).unwrap_or(&[]);
        let layout = if self.m.text {
            sat_layout(&rec.kind)
        } else {
            None
        };
        let Some(layout) = layout else {
            return fields.to_vec();
        };
        let mut out = vec![];
        let mut i = 0;
        while i < fields.len() && !fields[i].is_num() {
            out.push(fields[i].clone());
            i += 1;
        }
        for kind in layout.chars() {
            if kind == 'v' && i + 3 <= fields.len() && fields[i..i + 3].iter().all(Field::is_num) {
                out.push(Field::Vec3(std::array::from_fn(|k| {
                    fields[i + k].num().unwrap()
                })));
                i += 3;
            } else if i < fields.len() {
                out.push(fields[i].clone());
                i += 1;
            }
        }
        out.extend_from_slice(&fields[i..]);
        out
    }

    fn get(&self, f: &[Field], i: usize) -> Option<&'a Record> {
        self.m.get(f.get(i))
    }

    pub fn bodies(&mut self) -> Vec<Body> {
        let m = self.m;
        m.records
            .iter()
            .filter(|r| r.kind == "body")
            .map(|r| self.body(r))
            .collect()
    }

    fn body(&mut self, rec: &Record) -> Body {
        let f = self.f(rec);
        let mut lump = self.get(&f, 0);
        let transform = if f.len() > 2 {
            self.transform(self.get(&f, 2))
        } else {
            None
        };
        let mut faces = vec![];
        let mut seen = std::collections::HashSet::new();
        while let Some(l) = lump.filter(|l| seen.insert(l.index)) {
            let lf = self.f(l);
            let mut shell = self.get(&lf, 1);
            while let Some(s) = shell.filter(|s| seen.insert(s.index)) {
                let sf = self.f(s);
                let mut face = self.get(&sf, 2);
                while let Some(fr) = face.filter(|f| seen.insert(f.index)) {
                    faces.push(self.face(fr));
                    face = self.get(&self.f(fr), 0);
                }
                shell = self.get(&sf, 0);
            }
            lump = self.get(&lf, 0);
        }
        Body { faces, transform }
    }

    fn transform(&self, rec: Option<&Record>) -> Option<Transform> {
        let rec = rec.filter(|r| r.kind == "transform")?;
        let mut vals: Vec<f64> = vec![];
        let words = |s: &str, vals: &mut Vec<f64>| {
            vals.extend(s.split_whitespace().filter_map(|w| w.parse::<f64>().ok()))
        };
        // ASM writes the matrix as one text field; transforms have a shorter
        // common prefix than topology records, so scan all fields
        let texts: Vec<&String> = rec
            .fields
            .iter()
            .filter_map(|t| match t {
                Field::Str(s) if s.split_whitespace().count() >= 12 => Some(s),
                _ => None,
            })
            .collect();
        if !texts.is_empty() {
            for t in texts {
                words(t, &mut vals);
            }
        } else if !self.m.text {
            // SAB before ASM (ACIS 7.0, BricsCAD's DWG 2007 and 2010): typed
            // rows, translation and scale. The common prefix taken from the
            // lump is one field longer than this record's, so it would cut
            // the first row; the prefix holds only pointers and integers.
            for t in &rec.fields {
                match t {
                    Field::Vec3(v) => vals.extend(v),
                    Field::Float(x) => vals.push(*x),
                    _ => {}
                }
            }
        } else {
            for t in self.f(rec) {
                match t {
                    Field::Str(s) => words(&s, &mut vals),
                    Field::Vec3(v) => vals.extend(v),
                    other => vals.extend(other.num()),
                }
            }
        }
        if vals.len() < 12 {
            return None;
        }
        let rot: [f64; 9] = vals[..9].try_into().unwrap();
        Some((
            rot,
            [vals[9], vals[10], vals[11]],
            vals.get(12).copied().unwrap_or(1.0),
        ))
    }

    fn face(&mut self, rec: &Record) -> Face {
        let f = self.f(rec);
        let surf_rec = if f.len() > 4 { self.get(&f, 4) } else { None };
        let reversed = f.get(5).is_some_and(flag);
        let surface = surf_rec.and_then(|r| self.surface(r));
        let mut face = Face {
            surface,
            surface_type: surf_rec.map_or("none".into(), |r| r.kind.clone()),
            reversed,
            surface_reversed: surf_rec.is_some_and(|r| self.surface_reversed(r)),
            loops: vec![],
        };
        let mut lp = self.get(&f, 1);
        let mut seen = std::collections::HashSet::new();
        while let Some(l) = lp.filter(|l| seen.insert(l.index)) {
            let lf = self.f(l);
            let first = self.get(&lf, 1);
            face.loops.push(self.coedges(first));
            lp = self.get(&lf, 0);
        }
        face.loops.retain(|l| !l.is_empty());
        face
    }

    fn coedges(&mut self, first: Option<&'a Record>) -> Loop {
        let mut out = vec![];
        let mut co = first;
        let mut seen = std::collections::HashSet::new();
        while let Some(c) = co.filter(|c| seen.insert(c.index)) {
            let cf = self.f(c);
            let rev = cf.get(4).is_some_and(flag);
            if let Some(edge_rec) = self.get(&cf, 3) {
                if let Some(edge) = self.edge(edge_rec) {
                    out.push((edge, rev));
                }
            }
            co = self.get(&cf, 0);
        }
        out
    }

    fn point(&self, vertex: Option<&Record>) -> Option<Vec3> {
        for t in self.f(vertex?) {
            if let Some(rec) = self.m.get(Some(&t)).filter(|r| r.kind == "point") {
                if let Some(v) = self.f(rec).iter().find_map(vec3) {
                    return Some(v);
                }
            }
        }
        None
    }

    fn edge(&mut self, rec: &Record) -> Option<Rc<Edge>> {
        if let Some(e) = self.edges.get(&rec.index) {
            return Some(e.clone());
        }
        let f = self.f(rec);
        // edge: start_vertex [t0] end_vertex [t1] coedge curve sense ...
        let ptrs: Vec<usize> = f
            .iter()
            .enumerate()
            .filter(|(_, t)| matches!(t, Field::Ptr(_)))
            .map(|(i, _)| i)
            .collect();
        if ptrs.len() < 4 {
            return None;
        }
        let (v0, v1) = (self.get(&f, ptrs[0]), self.get(&f, ptrs[1]));
        let nums: Vec<f64> = f[ptrs[0]..(ptrs[1] + 2).min(f.len())]
            .iter()
            .filter_map(Field::num)
            .collect();
        let curve_rec = self.get(&f, ptrs[3]);
        let sense = f[ptrs[3] + 1..]
            .iter()
            .find(|t| matches!(t, Field::Bool(_) | Field::Str(_) | Field::Int(_)))
            .is_some_and(flag);
        let (p0, p1) = (self.point(v0)?, self.point(v1)?);
        let mut curve = curve_rec.and_then(|r| self.curve(r));
        let (t0, t1) = match (nums.first(), nums.get(1), &curve) {
            (Some(&t0), Some(&t1), _) => (t0, t1),
            // old SAT has no edge parameters: derive them from the vertices.
            // Both vertices of a closed edge have the same parameter, so on
            // a closed spline it runs once round, from t0 or (reversed) t1;
            // the tessellator projects a vertex that is not at an end.
            (_, _, Some(Curve::Spline(bs))) if closed_spline_edge(bs, p0, p1) => {
                if sense {
                    (-bs.t1, -bs.t0)
                } else {
                    (bs.t0, bs.t1)
                }
            }
            _ => match curve
                .as_ref()
                .and_then(|c| Some((c.param(p0)?, c.param(p1)?, c.period())))
            {
                None => {
                    curve = None;
                    (0.0, 0.0)
                }
                Some((a, b, period)) => {
                    let s = if sense { -1.0 } else { 1.0 };
                    let (t0, mut t1) = (s * a, s * b);
                    if let Some(period) = period {
                        // bounded: adding the period to a huge t1 changes nothing
                        let mut turns = 0;
                        while t1 <= t0 + 1e-12 && turns < 1000 {
                            t1 += period;
                            turns += 1;
                        }
                    }
                    (t0, t1)
                }
            },
        };
        let edge = Rc::new(Edge {
            index: rec.index,
            start: p0,
            end: p1,
            t0,
            t1,
            curve,
            reversed: sense,
        });
        self.edges.insert(rec.index, edge.clone());
        Some(edge)
    }

    fn curve(&self, rec: &Record) -> Option<Curve> {
        let f = self.f(rec);
        let vecs: Vec<Vec3> = f.iter().filter_map(vec3).collect();
        let nums: Vec<f64> = f.iter().filter_map(Field::num).collect();
        match rec.kind.as_str() {
            "straight-curve" if vecs.len() >= 2 => Some(Curve::straight(vecs[0], vecs[1])),
            "ellipse-curve" if vecs.len() >= 3 => Some(Curve::ellipse(
                vecs[0],
                vecs[1],
                vecs[2],
                nums.first().copied().unwrap_or(1.0),
            )),
            k if k.ends_with("intcurve-curve") => {
                let mut flat = vec![];
                flatten(&self.subtype(rec)?, &mut flat);
                let bs3 = all_bs3(&flat, false).into_iter().find_map(|b| match b {
                    Bs3::Curve(c) => Some(Curve::Spline(c)),
                    Bs3::Surface(_) => None,
                });
                bs3.or_else(|| self.parcur(&flat))
            }
            _ => None,
        }
    }

    /// An intcurve kept only as surface + parameter curve (`parcur` in ACIS
    /// 7.0, whose 3D approximation is reduced to its knots): the first
    /// support surface's NURBS along the first 2D curve. The support surface
    /// is usually a reference to the face's own spline surface.
    fn parcur(&self, flat: &[Field]) -> Option<Curve> {
        if !flat.first().is_some_and(|t| t.is_type("parcur")) {
            return None;
        }
        let mut resolved = vec![];
        self.flatten_refs(flat, &mut resolved, 0);
        let srf = all_bs3(&resolved, true).into_iter().find_map(|b| match b {
            Bs3::Surface(s) => Some(s),
            Bs3::Curve(_) => None,
        })?;
        // The surface may be written inline, and its data also reads as a
        // 2D curve: take the first 2D curve whose points lie in the
        // surface's parameter domain.
        let (du, dv) = (
            1e-6 * (srf.u1 - srf.u0).abs().max(1.0),
            1e-6 * (srf.v1 - srf.v0).abs().max(1.0),
        );
        let inside = |p: &Vec2| {
            (srf.u0 - du..=srf.u1 + du).contains(&p[0])
                && (srf.v0 - dv..=srf.v1 + dv).contains(&p[1])
        };
        let uv = flat.iter().enumerate().find_map(|(i, t)| {
            if !(t.is_type("nubs") || t.is_type("nurbs")) {
                return None;
            }
            let (degree, knots, ctrl, weights) = bs2_at(flat, i).ok()?;
            if !ctrl.iter().all(inside) {
                return None;
            }
            let ctrl = ctrl.iter().map(|p| [p[0], p[1], 0.0]).collect();
            Some(BSplineCurve::new(degree, knots, ctrl, weights))
        })?;
        curve_on_surface(&srf, &uv).map(Curve::Spline)
    }

    /// See [`Face::surface_reversed`]. The fields are those `surface` reads.
    fn surface_reversed(&self, rec: &Record) -> bool {
        let f = self.f(rec);
        let nums: Vec<f64> = f.iter().filter_map(Field::num).collect();
        match rec.kind.as_str() {
            // ratio, sin, cos, ...
            "cone-surface" => nums.get(2).is_some_and(|&cos| cos < 0.0),
            "sphere-surface" => nums.first().is_some_and(|&r| r < 0.0),
            "torus-surface" => nums.get(1).is_some_and(|&minor| minor < 0.0),
            // the sense logical comes before the subtype block
            "spline-surface" => f
                .iter()
                .take_while(|t| !matches!(t, Field::List(_)))
                .find(|t| matches!(t, Field::Bool(_) | Field::Str(_)))
                .is_some_and(flag),
            _ => false,
        }
    }

    fn surface(&mut self, rec: &Record) -> Option<SurfaceDef> {
        let f = self.f(rec);
        let vecs: Vec<Vec3> = f.iter().filter_map(vec3).collect();
        let nums: Vec<f64> = f.iter().filter_map(Field::num).collect();
        let vec_or = |i: usize, default: Vec3| vecs.get(i).copied().unwrap_or(default);
        let surface = match rec.kind.as_str() {
            "plane-surface" if vecs.len() >= 2 => Some(Surface::Plane(Plane::new(
                vecs[0],
                vecs[1],
                vec_or(2, ZERO),
            ))),
            // centre, axis, major axis, ratio, ..., sin, cos, scale
            "cone-surface" if vecs.len() >= 3 && nums.len() >= 3 => Some(Surface::Cone(Cone::new(
                vecs[0], vecs[1], vecs[2], nums[0], nums[1], nums[2],
            ))),
            "sphere-surface" if vecs.len() >= 2 && !nums.is_empty() => Some(Surface::Sphere(
                Sphere::new(vecs[0], nums[0], vecs[1], vec_or(2, [0.0, 0.0, 1.0])),
            )),
            "torus-surface" if vecs.len() >= 2 && nums.len() >= 2 => Some(Surface::Torus(
                Torus::new(vecs[0], vecs[1], nums[0], nums[1], vec_or(2, ZERO)),
            )),
            "spline-surface" => {
                let mut flat = vec![];
                if let Some(sub) = self.subtype(rec) {
                    flatten(&sub, &mut flat);
                }
                let cands: Vec<BSplineSurface> = all_bs3(&flat, true)
                    .into_iter()
                    .filter_map(|b| match b {
                        Bs3::Surface(s) => Some(s),
                        Bs3::Curve(_) => None,
                    })
                    .collect();
                if !cands.is_empty() {
                    return Some(SurfaceDef::SplineChoice(cands));
                }
                if flat
                    .first()
                    .is_some_and(|t| t.is_type("pipe_spl_sur") || t.is_type("rb_blend_spl_sur"))
                {
                    // the spine is the ball centre curve, possibly shared by reference
                    let mut resolved = vec![];
                    self.flatten_refs(&flat, &mut resolved, 0);
                    let spines: Vec<BSplineCurve> = all_bs3(&resolved, false)
                        .into_iter()
                        .filter_map(|b| match b {
                            Bs3::Curve(c) => Some(c),
                            Bs3::Surface(_) => None,
                        })
                        .collect();
                    if !spines.is_empty() {
                        return Some(SurfaceDef::PipeChoice(spines));
                    }
                }
                None
            }
            _ => None,
        };
        if surface.is_none() {
            *self.unknown_surfaces.entry(rec.kind.clone()).or_default() += 1;
        }
        surface.map(SurfaceDef::Analytic)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acis::sab::{AcisData, parse};
    use crate::acis::tessellate::Tessellator;

    /// A planar triangular REGION in SAT 4.0 text (records: body, lump, shell,
    /// face, loop, plane, 3 coedges, 3 edges, 3 vertices, 3 points, 3 lines).
    const SAT: &str = "400 0 1 0
16 Autodesk AutoCAD 19 ASM 0.0.0 test 24 Thu Jan 01 00:00:00 2026
1 9.9999999999999995e-07 1e-10
body $-1 $1 $-1 $-1 #
lump $-1 $-1 $2 $0 #
shell $-1 $-1 $-1 $3 $-1 $1 #
face $-1 $-1 $4 $2 $-1 $5 forward single #
loop $-1 $-1 $6 $3 #
plane-surface $-1 0 0 0 0 0 1 1 0 0 forward_v I I I I #
coedge $-1 $7 $8 $-1 $9 forward $4 $-1 #
coedge $-1 $8 $6 $-1 $10 forward $4 $-1 #
coedge $-1 $6 $7 $-1 $11 forward $4 $-1 #
edge $-1 $12 $13 $6 $15 forward #
edge $-1 $13 $14 $7 $16 forward #
edge $-1 $14 $12 $8 $17 forward #
vertex $-1 $9 $18 #
vertex $-1 $10 $19 #
vertex $-1 $11 $20 #
straight-curve $-1 0 0 0 1 0 0 I I #
straight-curve $-1 2 0 0 -0.70710678118654746 0.70710678118654746 0 I I #
straight-curve $-1 0 2 0 0 -1 0 I I #
point $-1 0 0 0 #
point $-1 2 0 0 #
point $-1 0 2 0 #
End-of-ACIS-data
";

    /// Old SAT edges have no parameters; for a spline edge they come from
    /// the nearest point (BricsCAD's sweeps in DWG R14 and 2000 lost their
    /// seam edge and were tessellated to the triangle limit).
    #[test]
    fn spline_param_is_the_nearest_point() {
        let bs = BSplineCurve::new(
            2,
            vec![0.0, 0.0, 0.0, 10.0, 10.0, 10.0],
            vec![[0.0, 0.0, 0.0], [5.0, 5.0, 0.0], [10.0, 0.0, 0.0]],
            None,
        );
        let c = Curve::Spline(bs.clone());
        for t in [0.0, 2.5, 7.0, 10.0] {
            let found = c.param(bs.eval(t)).unwrap();
            assert!((found - t).abs() < 1e-6, "{t} -> {found}");
        }
    }

    /// ACIS 7.0 keeps only the knots of a `parcur` intcurve's 3D curve; it
    /// is the support surface along the 2D curve. The surface here is
    /// written inline, before the 2D curve, as BricsCAD's helix sweep does,
    /// and its data also parses as a 2D curve.
    #[test]
    fn parcur_without_3d_points_follows_its_surface() {
        let (t, n, i, e) = (
            |s: &str| Field::Type(s.into()),
            Field::Float,
            Field::Int,
            Field::Enum,
        );
        // bilinear patch: x = 10u, y = 20v, z = 5uv on [0,1] x [0,1]
        let mut flat = vec![t("parcur"), e(1), i(2), n(0.0), n(1.0), t("spline")];
        flat.extend([t("nubs"), i(1), i(1), e(0), e(0), e(0), e(0), i(2), i(2)]);
        flat.extend([n(0.0), i(1), n(1.0), i(1), n(0.0), i(1), n(1.0), i(1)]);
        for p in [
            [0.0, 0.0, 0.0],
            [10.0, 0.0, 0.0],
            [0.0, 20.0, 0.0],
            [10.0, 20.0, 5.0],
        ] {
            flat.extend(p.map(n));
        }
        // 2D curve from (0,0) to (1,1) over t in [0, 1]
        flat.extend([t("nubs"), i(1), e(0), i(2), n(0.0), i(1), n(1.0), i(1)]);
        flat.extend([n(0.0), n(0.0), n(1.0), n(1.0)]);
        let model = Model {
            version: 700,
            units_mm: 1.0,
            text: false,
            records: vec![],
        };
        let Some(Curve::Spline(c)) = Brep::new(&model).parcur(&flat) else {
            panic!("no curve")
        };
        let p = c.eval(0.5);
        let want = [5.0, 10.0, 1.25];
        assert!(dist2(p, want) < 1e-12, "{p:?}");
        assert_eq!((c.t0, c.t1), (0.0, 1.0));
    }

    /// ACIS 7.0 SAB (BricsCAD's DWG 2007 and 2010): the transform record
    /// holds three rotation rows, the translation and the scale as typed
    /// fields after the same two-field prefix as in later versions, while
    /// the lump (which sets the prefix) has one more. Seen on BricsCAD's box
    /// at 60,50,50, which came out centred on the origin.
    #[test]
    fn acis7_binary_transform_is_read() {
        let rec = |index, kind: &str, fields| Record {
            index,
            kind: kind.into(),
            fields,
        };
        let (p, i) = (Field::Ptr, Field::Int);
        let model = Model {
            version: 700,
            units_mm: 1.0,
            text: false,
            records: vec![
                rec(0, "body", vec![p(-1), i(-1), p(-1), p(1), p(-1), p(2)]),
                rec(1, "lump", vec![p(-1), i(-1), p(-1), p(-1), p(-1), p(0)]),
                rec(
                    2,
                    "transform",
                    vec![
                        p(-1),
                        i(-1),
                        Field::Vec3([1.0, 0.0, 0.0]),
                        Field::Vec3([0.0, 1.0, 0.0]),
                        Field::Vec3([0.0, 0.0, 1.0]),
                        Field::Vec3([60.0, 50.0, 50.0]),
                        Field::Float(2.0),
                        Field::Bool(false),
                        Field::Bool(false),
                        Field::Bool(false),
                    ],
                ),
            ],
        };
        let bodies = Brep::new(&model).bodies();
        assert_eq!(
            bodies[0].transform,
            Some((
                [1.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0],
                [60.0, 50.0, 50.0],
                2.0
            ))
        );
    }

    /// Knot multiplicities asking for a 200 000 x 200 000 control net (960 GB
    /// before any point was read) in an 854-byte SAT: rejected, the face
    /// keeps its boundary.
    #[test]
    fn spline_net_larger_than_its_data_is_rejected() {
        let spline = "spline-surface $-1 forward { exactsur nubs 1 1 open open none none \
                      1 1 0 200000 0 200000 } I I I I #";
        let sat: String = SAT
            .lines()
            .map(|l| {
                if l.starts_with("plane-surface") {
                    spline
                } else {
                    l
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
        let model = parse(&AcisData::Sat(sat)).unwrap();
        let bodies = Brep::new(&model).bodies();
        assert_eq!(bodies.len(), 1);
        Tessellator::new(1e-4).body(&bodies[0]);
        // a knot vector longer than the data is refused before it is built
        let t = [
            Field::Int(0),
            Field::Int(999_999),
            Field::Int(1),
            Field::Int(999_999),
        ];
        let mut c = Cursor { t: &t, i: 0 };
        assert!(c.knots(2).is_err());
    }

    #[test]
    fn sat_region() {
        let model = parse(&AcisData::Sat(SAT.into())).unwrap();
        let bodies = Brep::new(&model).bodies();
        assert_eq!(bodies.len(), 1);
        let face = &bodies[0].faces[0];
        assert!(matches!(
            face.surface,
            Some(SurfaceDef::Analytic(Surface::Plane(_)))
        ));
        assert_eq!(face.loops[0].len(), 3);
        let tris = Tessellator::new(1e-4).body(&bodies[0]);
        let area: f64 = tris
            .iter()
            .map(|[a, b, c]| length(cross(sub(*b, *a), sub(*c, *a))) / 2.0)
            .sum();
        assert!((area - 2.0).abs() < 1e-12);
    }

    /// The same REGION in ACIS 1.x SAT (as in R13/R14 DWGs): no product and
    /// tolerance lines, senses as 0/1, no reverse_v on the plane.
    const SAT_1X: &str = "106 0 1 0
body $-1 $1 $-1 $-1 #
lump $-1 $-1 $2 $0 #
shell $-1 $-1 $-1 $3 $1 #
face $-1 $-1 $4 $2 $-1 $5 forward single #
loop $-1 $-1 $6 $3 #
plane-surface $-1 0 0 0 0 0 1 1 0 0 I I I I #
coedge $-1 $7 $8 $-1 $9 0 $4 $-1 #
coedge $-1 $8 $6 $-1 $10 0 $4 $-1 #
coedge $-1 $6 $7 $-1 $11 0 $4 $-1 #
edge $-1 $12 $13 $6 $15 0 #
edge $-1 $13 $14 $7 $16 0 #
edge $-1 $14 $12 $8 $17 0 #
vertex $-1 $9 $18 #
vertex $-1 $10 $19 #
vertex $-1 $11 $20 #
straight-curve $-1 0 0 0 1 0 0 I I #
straight-curve $-1 2 0 0 -0.70710678118654746 0.70710678118654746 0 I I #
straight-curve $-1 0 2 0 0 -1 0 I I #
point $-1 0 0 0 #
point $-1 2 0 0 #
point $-1 0 2 0 #
End-of-ACIS-data
";

    #[test]
    fn sat_region_acis_1x() {
        let model = parse(&AcisData::Sat(SAT_1X.into())).unwrap();
        let bodies = Brep::new(&model).bodies();
        assert_eq!(bodies.len(), 1);
        let face = &bodies[0].faces[0];
        assert_eq!(face.loops[0].len(), 3);
        let tris = Tessellator::new(1e-4).body(&bodies[0]);
        let area: f64 = tris
            .iter()
            .map(|[a, b, c]| length(cross(sub(*b, *a), sub(*c, *a))) / 2.0)
            .sum();
        assert!((area - 2.0).abs() < 1e-12);
    }

    /// ACIS 1.x to 7 SAT (R14 and 2000 DWGs) has no edge parameters. A
    /// closed edge has the same vertex at both ends, so projecting them gave
    /// an empty range: the hole a cylinder leaves in another, bounded by one
    /// closed intcurve each, was dropped (`trim_cylinder_hole`). A 2 x 2
    /// square bounded by one closed spline edge, run forwards and, by a
    /// reversed edge on a reversed coedge, backwards twice.
    #[test]
    fn old_sat_spline_edge_keeps_its_curve() {
        for (edge_sense, coedge_sense) in [(0, 0), (1, 1)] {
            let sat = format!(
                "106 0 1 0
body $-1 $1 $-1 $-1 #
lump $-1 $-1 $2 $0 #
shell $-1 $-1 $-1 $3 $1 #
face $-1 $-1 $4 $2 $-1 $5 forward single #
loop $-1 $-1 $6 $3 #
plane-surface $-1 0 0 0 0 0 1 1 0 0 I I I I #
coedge $-1 $6 $6 $-1 $7 {coedge_sense} $4 $-1 #
edge $-1 $8 $8 $6 $10 {edge_sense} #
vertex $-1 $7 $9 #
point $-1 0 0 0 #
intcurve-curve $-1 0 {{exactcur nubs 1 open 5 0 1 1 1 2 1 3 1 4 1 \
0 0 0 2 0 0 2 2 0 0 2 0 0 0 0 0 nullbs nullbs }} I I #
End-of-ACIS-data
"
            );
            let model = parse(&AcisData::Sat(sat)).unwrap();
            let bodies = Brep::new(&model).bodies();
            let edge = &bodies[0].faces[0].loops[0][0].0;
            assert!(matches!(edge.curve, Some(Curve::Spline(_))));
            let tris = Tessellator::new(1e-4).body(&bodies[0]);
            let normal = tris.iter().fold(ZERO, |n, [a, b, c]| {
                add(n, mul(cross(sub(*b, *a), sub(*c, *a)), 0.5))
            });
            assert!((normal[2] - 4.0).abs() < 1e-9, "{edge_sense}: {normal:?}");
        }
    }

    #[test]
    fn integer_senses() {
        assert!(flag(&Field::Int(1)));
        assert!(!flag(&Field::Int(0)));
    }
}
