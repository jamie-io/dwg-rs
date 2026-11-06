// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Shapes from the BricsCAD test drawings (`tests/bricscad`) checked
//! against their right answers in `expected.json`, with the tolerances of
//! `tests/bricscad/measure.py`. All of them are measured by that script;
//! these are the files a fix was made for.

use std::path::{Path, PathBuf};

use dwg2gltf::Options;
use dwg2gltf::geom::{cross, dot};
use serde_json::Value;

mod common;
use common::{bounds, read_back, world_triangles};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/bricscad/dwg/v27")
}

/// Area (mm²), volume (mm³) and bounds (mm) of a converted file, in the
/// drawing's own units like measure.py (R14 has no INSUNITS: mm).
fn measure(file: &Path, version: &str) -> (f64, f64, ([f64; 3], [f64; 3])) {
    let out = std::env::temp_dir().join(format!(
        "dwg2gltf-bricscad-{}-{}.glb",
        std::process::id(),
        file.file_stem().unwrap().to_string_lossy()
    ));
    let options = Options {
        y_up: false,
        units: (version == "AC1014").then_some(4),
        ..Options::default()
    };
    let stats = dwg2gltf::convert(file, &out, &options).unwrap();
    assert!(
        stats.skipped.is_empty() && stats.repaired.is_empty(),
        "{}: {stats}",
        file.display()
    );
    let back = read_back(&out);
    let volume: f64 = world_triangles(&out)
        .iter()
        .map(|([a, b, c], _)| dot(*a, cross(*b, *c)) / 6.0)
        .sum();
    std::fs::remove_file(&out).unwrap();
    let (lo, hi) = bounds(&back.points);
    let mm = |p: [f64; 3]| p.map(|v| v * 1e3);
    (back.area * 1e6, volume * 1e9, (mm(lo), mm(hi)))
}

/// Every version of `shape` matches its right answer.
fn check(family: &str, shape: &str) {
    let expected: Value =
        serde_json::from_str(&std::fs::read_to_string(root().join("expected.json")).unwrap())
            .unwrap();
    let mut seen = 0;
    for (file, entry) in expected.as_object().unwrap() {
        if entry["family"] != family || entry["shape"] != shape || entry["status"] != "ok" {
            continue;
        }
        seen += 1;
        let version = entry["version"].as_str().unwrap();
        let (area, volume, (lo, hi)) = measure(&root().join(file), version);
        let want_area = entry["area"].as_f64().unwrap();
        let want_volume = entry["volume"].as_f64().unwrap();
        let want: Vec<f64> = entry["bounds"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|c| c.as_array().unwrap().iter().map(|v| v.as_f64().unwrap()))
            .collect();
        let diagonal = (0..3)
            .map(|k| (want[3 + k] - want[k]).powi(2))
            .sum::<f64>()
            .sqrt();
        let tol = 0.2 + 1e-4 * diagonal;
        let got: Vec<f64> = lo.iter().chain(&hi).copied().collect();
        assert!(
            got.iter().zip(&want).all(|(a, b)| (a - b).abs() <= tol),
            "{file}: bounds {got:?}, expected {want:?}"
        );
        assert!(
            (area - want_area).abs() <= 0.01 * want_area,
            "{file}: area {area:.1}, expected {want_area:.1}"
        );
        assert!(
            (volume - want_volume).abs() <= 0.01 * want_volume + 0.1 * want_area,
            "{file}: volume {volume:.1}, expected {want_volume:.1}"
        );
    }
    assert_eq!(seen, 7, "{shape}: one file per DWG version");
}

/// A sphere a cylinder runs through along x: its face has two holes and no
/// outer loop, one hole across the sphere's seam. Only a cap was left.
#[test]
fn trim_sphere_hole_seam() {
    check("trim", "trim_sphere_hole_seam");
}

/// A cylinder a smaller one runs through: the band's seam was cut open
/// inside a hole, which was lost; and in R14 and 2000 (old SAT) the hole's
/// spline edges were dropped.
#[test]
fn trim_cylinder_hole() {
    check("trim", "trim_cylinder_hole");
}

#[test]
fn trim_cylinder_hole_seam() {
    check("trim", "trim_cylinder_hole_seam");
}
