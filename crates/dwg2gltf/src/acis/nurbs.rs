// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! (Rational) B-spline curve and surface evaluation.

use crate::geom::Vec3;

pub fn find_span(degree: usize, knots: &[f64], n_ctrl: usize, t: f64) -> usize {
    let (lo, hi) = (degree, n_ctrl - 1);
    if t >= knots[hi + 1] {
        return hi;
    }
    if t <= knots[lo] {
        return lo;
    }
    // bisect_right - 1
    let right = knots.partition_point(|&k| k <= t);
    right.saturating_sub(1).clamp(lo, hi)
}

/// Non-zero basis functions N[span-degree .. span] at t (The NURBS Book A2.2).
pub fn basis(degree: usize, knots: &[f64], span: usize, t: f64) -> Vec<f64> {
    let mut n = vec![0.0; degree + 1];
    n[0] = 1.0;
    let mut left = vec![0.0; degree + 1];
    let mut right = vec![0.0; degree + 1];
    for j in 1..=degree {
        left[j] = t - knots[span + 1 - j];
        right[j] = knots[span + j] - t;
        let mut saved = 0.0;
        for r in 0..j {
            let denom = right[r + 1] + left[j - r];
            let tmp = if denom != 0.0 { n[r] / denom } else { 0.0 };
            n[r] = saved + right[r + 1] * tmp;
            saved = left[j - r] * tmp;
        }
        n[j] = saved;
    }
    n
}

#[derive(Debug, Clone)]
pub struct BSplineCurve {
    pub degree: usize,
    pub knots: Vec<f64>,
    pub ctrl: Vec<Vec3>,
    pub weights: Option<Vec<f64>>,
    pub t0: f64,
    pub t1: f64,
}

impl BSplineCurve {
    pub fn new(degree: usize, knots: Vec<f64>, ctrl: Vec<Vec3>, weights: Option<Vec<f64>>) -> Self {
        let (t0, t1) = (knots[degree], knots[ctrl.len()]);
        BSplineCurve {
            degree,
            knots,
            ctrl,
            weights,
            t0,
            t1,
        }
    }

    pub fn eval(&self, t: f64) -> Vec3 {
        let p = self.degree;
        let span = find_span(p, &self.knots, self.ctrl.len(), t);
        let nb = basis(p, &self.knots, span, t);
        let (mut x, mut y, mut z, mut ws) = (0.0, 0.0, 0.0, 0.0);
        for (i, &nbi) in nb.iter().enumerate() {
            let k = span - p + i;
            let c = self.ctrl[k];
            let b = nbi * self.weights.as_ref().map_or(1.0, |w| w[k]);
            x += b * c[0];
            y += b * c[1];
            z += b * c[2];
            ws += b;
        }
        if ws != 0.0 {
            [x / ws, y / ws, z / ws]
        } else {
            [x, y, z]
        }
    }
}

/// `ctrl[i][j]`: i along u, j along v.
#[derive(Debug, Clone)]
pub struct BSplineSurface {
    pub pu: usize,
    pub pv: usize,
    pub ku: Vec<f64>,
    pub kv: Vec<f64>,
    pub ctrl: Vec<Vec<Vec3>>,
    pub weights: Option<Vec<Vec<f64>>>,
    pub nu: usize,
    pub nv: usize,
    pub u0: f64,
    pub u1: f64,
    pub v0: f64,
    pub v1: f64,
    /// The surface closes on itself in u / v (ACIS "closed" or "periodic").
    pub closed_u: bool,
    pub closed_v: bool,
}

impl BSplineSurface {
    pub fn new(
        pu: usize,
        pv: usize,
        ku: Vec<f64>,
        kv: Vec<f64>,
        ctrl: Vec<Vec<Vec3>>,
        weights: Option<Vec<Vec<f64>>>,
    ) -> Self {
        let (nu, nv) = (ctrl.len(), ctrl[0].len());
        let (u0, u1, v0, v1) = (ku[pu], ku[nu], kv[pv], kv[nv]);
        BSplineSurface {
            pu,
            pv,
            ku,
            kv,
            ctrl,
            weights,
            nu,
            nv,
            u0,
            u1,
            v0,
            v1,
            closed_u: false,
            closed_v: false,
        }
    }

    pub fn eval(&self, u: f64, v: f64) -> Vec3 {
        let (pu, pv) = (self.pu, self.pv);
        let su = find_span(pu, &self.ku, self.nu, u);
        let sv = find_span(pv, &self.kv, self.nv, v);
        let bu = basis(pu, &self.ku, su, u);
        let bv = basis(pv, &self.kv, sv, v);
        let (mut x, mut y, mut z, mut ws) = (0.0, 0.0, 0.0, 0.0);
        for (i, &bui) in bu.iter().enumerate() {
            let row = &self.ctrl[su - pu + i];
            let wrow = self.weights.as_ref().map(|w| &w[su - pu + i]);
            for (j, &bvj) in bv.iter().enumerate() {
                let mut b = bui * bvj;
                if let Some(w) = wrow {
                    b *= w[sv - pv + j];
                }
                let c = row[sv - pv + j];
                x += b * c[0];
                y += b * c[1];
                z += b * c[2];
                ws += b;
            }
        }
        if ws != 0.0 {
            [x / ws, y / ws, z / ws]
        } else {
            [x, y, z]
        }
    }
}

/// ACIS stores end multiplicities as `degree`; standard clamped vectors need degree+1.
pub fn clamp_knots(pairs: &[(f64, usize)]) -> Vec<f64> {
    let mut knots: Vec<f64> = pairs
        .iter()
        .flat_map(|&(value, mult)| std::iter::repeat_n(value, mult))
        .collect();
    if let (Some(&first), Some(&last)) = (knots.first(), knots.last()) {
        knots.insert(0, first);
        knots.push(last);
    }
    knots
}
