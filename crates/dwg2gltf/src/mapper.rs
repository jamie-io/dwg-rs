// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! AutoCAD's per-entity material mapper (AcGiMapper), stored as XDATA
//! under the APPID `ACAD_MATERIAL_MAPPER`: projection, tiling and
//! auto-transform (group 1070 each), then the mapper's origin and the ends
//! of its X, Y and Z axes (group 1011 each), in the coordinates of the
//! entity that carries it (for an INSERT: those of the inserted block).

use crate::dwg::XValue;
use crate::geom::{Mat, Vec3, sub};

/// The APPID the mapper's XDATA is registered under.
pub const APPID: &str = "ACAD_MATERIAL_MAPPER";

/// AcGiMapper::Projection.
pub const PLANAR: i16 = 1;
pub const BOX: i16 = 2;
pub const CYLINDER: i16 = 3;
pub const SPHERE: i16 = 4;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MaterialMapper {
    /// 1 planar, 2 box, 3 cylinder, 4 sphere.
    pub projection: i16,
    /// 0 inherit, 1 tile, 2 crop, 3 clamp, 4 mirror (not used).
    pub tiling: i16,
    /// 1 none, 2 object, 4 model (not used).
    pub auto_transform: i16,
    pub origin: Vec3,
    /// The ends of the X, Y and Z axes (not the axes themselves).
    pub axes_ends: [Vec3; 3],
}

impl MaterialMapper {
    /// From the XDATA items of the APPID (raw EED code, value): the first
    /// three 70s (int16) and the first four 11s (points). None when some
    /// are missing.
    pub fn from_eed(items: &[(u8, XValue)]) -> Option<Self> {
        let ints: Vec<i16> = items
            .iter()
            .filter_map(|(c, v)| match (c, v) {
                (70, XValue::Int(i)) => Some(*i as i16),
                _ => None,
            })
            .take(3)
            .collect();
        let pts: Vec<Vec3> = items
            .iter()
            .filter_map(|(c, v)| match (c, v) {
                (11, XValue::Point(p)) => Some(*p),
                _ => None,
            })
            .take(4)
            .collect();
        let (&[projection, tiling, auto_transform], &[origin, x, y, z]) =
            (ints.as_slice(), pts.as_slice())
        else {
            return None;
        };
        Some(MaterialMapper {
            projection,
            tiling,
            auto_transform,
            origin,
            axes_ends: [x, y, z],
        })
    }

    /// The axis vectors (end minus origin).
    pub fn axes(&self) -> [Vec3; 3] {
        self.axes_ends.map(|e| sub(e, self.origin))
    }

    /// Origin at 0 and unit axes along X, Y, Z: the entity's own
    /// coordinates, one repeat per drawing unit.
    pub fn is_identity(&self) -> bool {
        let near = |a: Vec3, b: Vec3| a.iter().zip(&b).all(|(p, q)| (p - q).abs() <= 1e-9);
        near(self.origin, [0.0; 3])
            && near(self.axes_ends[0], [1.0, 0.0, 0.0])
            && near(self.axes_ends[1], [0.0, 1.0, 0.0])
            && near(self.axes_ends[2], [0.0, 0.0, 1.0])
    }

    /// The map from the entity's coordinates into the mapper's unit frame,
    /// `p' = A⁻¹(p − O)` with the axes as the columns of A (row-major 4x4).
    /// None when the axes are degenerate or not finite.
    pub fn frame(&self) -> Option<Mat> {
        let [ax, ay, az] = self.axes();
        let a = [
            [ax[0], ay[0], az[0]],
            [ax[1], ay[1], az[1]],
            [ax[2], ay[2], az[2]],
        ];
        let inv = inverse3(a)?;
        let o = self.origin;
        let mut m = [0.0; 16];
        for r in 0..3 {
            m[r * 4..r * 4 + 3].copy_from_slice(&inv[r]);
            m[r * 4 + 3] = -(inv[r][0] * o[0] + inv[r][1] * o[1] + inv[r][2] * o[2]);
        }
        m[15] = 1.0;
        m.iter().all(|c| c.is_finite()).then_some(m)
    }
}

/// The inverse of a 3x3 matrix (rows), None when (nearly) singular.
pub fn inverse3(a: [[f64; 3]; 3]) -> Option<[[f64; 3]; 3]> {
    let cof = |r: usize, c: usize| {
        let (r1, r2) = ((r + 1) % 3, (r + 2) % 3);
        let (c1, c2) = ((c + 1) % 3, (c + 2) % 3);
        a[r1][c1] * a[r2][c2] - a[r1][c2] * a[r2][c1]
    };
    let det = a[0][0] * cof(0, 0) + a[0][1] * cof(0, 1) + a[0][2] * cof(0, 2);
    let scale = a.iter().flatten().fold(0.0f64, |m, c| m.max(c.abs()));
    if !det.is_finite() || det.abs() <= 1e-12 * scale.powi(3) || det == 0.0 {
        return None;
    }
    // the inverse is the transposed cofactor matrix over the determinant
    Some(std::array::from_fn(|r| {
        std::array::from_fn(|c| cof(c, r) / det)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geom::apply;

    fn items(ints: [i64; 3], pts: [Vec3; 4]) -> Vec<(u8, XValue)> {
        ints.iter()
            .map(|&i| (70, XValue::Int(i)))
            .chain(pts.iter().map(|&p| (11, XValue::Point(p))))
            .collect()
    }

    #[test]
    fn parses_a_rug_mapper() {
        // a 170x240 rug: box, tile, no auto-transform; spans the rug's
        // bounding box once
        let eed = items(
            [2, 1, 1],
            [
                [-85.0, -120.0, 0.0],
                [84.96, -120.0, 0.0],
                [-85.0, 119.96, 0.0],
                [-85.0, -120.0, 100.0],
            ],
        );
        let m = MaterialMapper::from_eed(&eed).unwrap();
        assert_eq!((m.projection, m.tiling, m.auto_transform), (BOX, 1, 1));
        assert!(!m.is_identity());
        let f = m.frame().unwrap();
        let near = |a: Vec3, b: Vec3| a.iter().zip(&b).all(|(p, q)| (p - q).abs() < 1e-9);
        assert!(near(apply(&f, [-85.0, -120.0, 0.0]), [0.0; 3]));
        assert!(near(apply(&f, [84.96, 119.96, 100.0]), [1.0; 3]));
        assert!(near(apply(&f, [-0.02, -0.02, 50.0]), [0.5; 3]));
    }

    #[test]
    fn incomplete_or_degenerate_mappers() {
        let pts = [[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [0.0, 0.0, 1.0]];
        let mut eed = items([1, 1, 2], pts);
        let m = MaterialMapper::from_eed(&eed).unwrap();
        assert!(m.is_identity());
        assert_eq!(m.frame().unwrap(), crate::geom::IDENTITY);
        eed.pop();
        assert_eq!(MaterialMapper::from_eed(&eed), None);
        // other codes are ignored
        let mut eed = items([1, 1, 2], pts);
        eed.insert(1, (0, XValue::Text("x".into())));
        eed.insert(5, (10, XValue::Point([9.0; 3])));
        assert_eq!(MaterialMapper::from_eed(&eed), Some(m));
        // flat axes have no frame
        let flat = items(
            [2, 1, 1],
            [[0.0; 3], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [1.0, 1.0, 0.0]],
        );
        assert_eq!(MaterialMapper::from_eed(&flat).unwrap().frame(), None);
    }

    #[test]
    fn inverse_of_rotated_scaled_axes() {
        let a = [[0.0, -2.0, 0.0], [3.0, 0.0, 0.0], [0.0, 0.0, 0.5]];
        let inv = inverse3(a).unwrap();
        for r in 0..3 {
            for c in 0..3 {
                let p: f64 = (0..3).map(|k| a[r][k] * inv[k][c]).sum();
                assert!((p - if r == c { 1.0 } else { 0.0 }).abs() < 1e-12);
            }
        }
    }
}
