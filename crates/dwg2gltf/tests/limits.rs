// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Crafted and damaged input: resource limits, and losses that must be
//! reported. Small synthetic drawings with small limits; the measurements
//! behind the real limits are in src/limits.rs.

use dwg2gltf::acis::sab::AcisData;
use dwg2gltf::dwg::{BlockHeader, Color, Data, Drawing, Entity, Handle, Layer, Object, Shape};
use dwg2gltf::{Converter, Document, Error, Limits, Options, Stats};

/// A drawing with layer "0" and a model space.
struct Doc {
    drawing: Drawing,
    next: Handle,
    layer0: Handle,
    model: Handle,
}

impl Doc {
    fn new() -> Doc {
        let mut doc = Doc {
            drawing: Drawing {
                insunits: 6,
                ..Drawing::default()
            },
            next: 0x100,
            layer0: 0,
            model: 0,
        };
        doc.layer0 = doc.add(
            "LAYER",
            Data::Layer(Layer {
                name: "0".into(),
                color: Color {
                    index: Some(7),
                    rgb: None,
                },
                material: 0,
                off: false,
                frozen: false,
            }),
        );
        doc.model = doc.block("*Model_Space");
        doc
    }

    fn add(&mut self, kind: &str, data: Data) -> Handle {
        let handle = self.next;
        self.next += 1;
        self.drawing.objects.push(Object {
            handle,
            kind: kind.into(),
            owner: 0,
            xdic: 0,
            data,
        });
        handle
    }

    fn block(&mut self, name: &str) -> Handle {
        self.add(
            "BLOCK_HEADER",
            Data::BlockHeader(BlockHeader {
                name: name.into(),
                base_pt: [0.0; 3],
                entities: Some(vec![]),
                xref: None,
            }),
        )
    }

    fn put(&mut self, block: Handle, kind: &str, shape: Shape) -> Handle {
        let ent = Entity {
            entmode: 0,
            invisible: false,
            layer: self.layer0,
            color: Color {
                index: Some(256),
                rgb: None,
            },
            material_flags: 0,
            material_refs: [0, 0],
            has_ds_data: false,
            thickness: 0.0,
            extrusion: Some([0.0, 0.0, 1.0]),
            material_mapper: None,
            shape,
        };
        let h = self.add(kind, Data::Entity(Box::new(ent)));
        let obj = self
            .drawing
            .objects
            .iter_mut()
            .find(|o| o.handle == block)
            .unwrap();
        match &mut obj.data {
            Data::BlockHeader(b) => b.entities.as_mut().unwrap().push(h),
            _ => unreachable!(),
        }
        h
    }

    fn stats(self, options: Options) -> Stats {
        let doc = Document::new(self.drawing, None);
        Converter::new(&doc, options).run().unwrap().1
    }
}

fn face(x: f64) -> Shape {
    Shape::Face3d([
        [x, 0.0, 0.0],
        [x + 1.0, 0.0, 0.0],
        [x + 1.0, 1.0, 0.0],
        [x, 1.0, 0.0],
    ])
}

fn line() -> Shape {
    Shape::Line {
        start: [0.0; 3],
        end: [1.0, 0.0, 0.0],
    }
}

fn insert(block: Handle, cols: u16, rows: u16) -> Shape {
    Shape::Insert {
        block,
        ins_pt: [0.0; 3],
        scale: [1.0; 3],
        rotation: 0.0,
        cols,
        rows,
        col_spacing: 2.0,
        row_spacing: 2.0,
    }
}

fn limits(instances: usize, triangles: usize, line_points: usize) -> Options {
    Options {
        limits: Limits {
            instances,
            triangles,
            line_points,
        },
        ..Options::default()
    }
}

fn skipped(stats: &Stats, what: &str) -> usize {
    stats.skipped.get(what).copied().unwrap_or(0)
}

fn warned(stats: &Stats, text: &str) -> bool {
    stats.warnings.iter().any(|w| w.contains(text))
}

// -- block instances ------------------------------------------------------------

#[test]
fn block_inserting_itself_is_cut_off() {
    // each level would double the instances: 2^64 without the check
    let mut doc = Doc::new();
    let s = doc.block("S");
    doc.put(s, "3DFACE", face(0.0));
    doc.put(s, "INSERT", insert(s, 0, 0));
    doc.put(s, "INSERT", insert(s, 0, 0));
    let model = doc.model;
    doc.put(model, "INSERT", insert(s, 0, 0));
    let stats = doc.stats(Options::default());
    assert_eq!(stats.instances, 1);
    assert_eq!(skipped(&stats, "INSERT (block inserts itself)"), 2);
    assert_eq!(stats.converted.get("INSERT"), Some(&1));
}

#[test]
fn indirect_recursion_is_cut_off() {
    let mut doc = Doc::new();
    let (a, b) = (doc.block("A"), doc.block("B"));
    doc.put(a, "3DFACE", face(0.0));
    doc.put(b, "3DFACE", face(0.0));
    doc.put(a, "INSERT", insert(b, 0, 0));
    doc.put(b, "INSERT", insert(a, 0, 0));
    let model = doc.model;
    doc.put(model, "INSERT", insert(a, 0, 0));
    let stats = doc.stats(Options::default());
    assert_eq!(stats.instances, 2);
    assert_eq!(skipped(&stats, "INSERT (block inserts itself)"), 1);
}

/// Runs on cargo test's 2 MiB thread: the deepest nesting allowed must fit
/// (converting recurses once per level).
#[test]
fn nesting_limit_cuts_deep_chains() {
    use dwg2gltf::limits::MAX_NESTING;
    let mut doc = Doc::new();
    let n = MAX_NESTING + 5;
    let blocks: Vec<Handle> = (0..n).map(|i| doc.block(&format!("B{i}"))).collect();
    for i in 0..n {
        doc.put(blocks[i], "3DFACE", face(0.0));
        if i + 1 < n {
            doc.put(blocks[i], "INSERT", insert(blocks[i + 1], 0, 0));
        }
    }
    let model = doc.model;
    doc.put(model, "INSERT", insert(blocks[0], 0, 0));
    let stats = doc.stats(Options::default());
    assert_eq!(stats.instances, MAX_NESTING);
    assert_eq!(skipped(&stats, "INSERT (nesting too deep)"), 1);
}

#[test]
fn instance_limit_stops_fan_out() {
    // 8 levels of 2 INSERTs each: 510 instances, limit 100
    let mut doc = Doc::new();
    let blocks: Vec<Handle> = (0..9).map(|i| doc.block(&format!("B{i}"))).collect();
    doc.put(blocks[8], "3DFACE", face(0.0));
    for i in 0..8 {
        doc.put(blocks[i], "INSERT", insert(blocks[i + 1], 0, 0));
        doc.put(blocks[i], "INSERT", insert(blocks[i + 1], 0, 0));
    }
    let model = doc.model;
    doc.put(model, "INSERT", insert(blocks[0], 0, 0));
    let stats = doc.stats(limits(100, usize::MAX, usize::MAX));
    assert!(stats.instances <= 100, "{}", stats.instances);
    assert!(skipped(&stats, "INSERT (instance limit reached)") > 0);
    assert!(warned(
        &stats,
        "the drawing needs more than 100 block instances (the limit); the rest is left out"
    ));
}

#[test]
fn minsert_cells_count_against_the_instance_limit() {
    let mut doc = Doc::new();
    let m = doc.block("M");
    doc.put(m, "3DFACE", face(0.0));
    let model = doc.model;
    // 65535 x 65535 cells (4.3 billion): refused before anything is made
    doc.put(model, "MINSERT", insert(m, u16::MAX, u16::MAX));
    doc.put(model, "MINSERT", insert(m, 3, 3));
    let stats = doc.stats(Options::default());
    assert_eq!(skipped(&stats, "MINSERT (instance limit reached)"), 1);
    assert_eq!(stats.converted.get("MINSERT"), Some(&1));
    assert_eq!(stats.instances, 9);
    assert!(warned(&stats, "block instances (the limit)"));
    // a small limit: 3 x 3 does not fit into 5
    let mut doc = Doc::new();
    let m = doc.block("M");
    doc.put(m, "3DFACE", face(0.0));
    let model = doc.model;
    doc.put(model, "MINSERT", insert(m, 3, 3));
    let stats = doc.stats(limits(5, usize::MAX, usize::MAX));
    assert_eq!(skipped(&stats, "MINSERT (instance limit reached)"), 1);
    assert_eq!(stats.instances, 0);
}

// -- triangles and lines --------------------------------------------------------

#[test]
fn triangle_limit_skips_what_does_not_fit() {
    let mut doc = Doc::new();
    let model = doc.model;
    for i in 0..3 {
        doc.put(model, "3DFACE", face(2.0 * i as f64));
    }
    // two triangles each: the third face does not fit into 5
    let stats = doc.stats(limits(usize::MAX, 5, usize::MAX));
    assert_eq!(stats.triangles, 4);
    assert_eq!(stats.converted.get("3DFACE"), Some(&2));
    assert_eq!(skipped(&stats, "3DFACE (triangle limit reached)"), 1);
    assert!(warned(
        &stats,
        "the drawing needs more than 5 triangles (the limit); the rest is left out"
    ));
}

#[test]
fn line_limit_skips_what_does_not_fit() {
    let mut doc = Doc::new();
    let model = doc.model;
    for _ in 0..3 {
        doc.put(model, "LINE", line());
    }
    let stats = doc.stats(limits(usize::MAX, usize::MAX, 5));
    assert_eq!(stats.lines, 2);
    assert_eq!(skipped(&stats, "LINE (line limit reached)"), 1);
    assert!(warned(&stats, "more than 5 line points (the limit)"));
}

/// A planar triangular REGION (the one of the brep.rs tests).
const REGION_SAT: &str = include_str!("../examples/limit_probe_region.sat");

fn solid(sat: &str) -> Shape {
    Shape::Acis {
        data: Some(AcisData::Sat(sat.into())),
        wires: vec![],
    }
}

#[test]
fn acis_bodies_count_against_the_triangle_limit() {
    let mut doc = Doc::new();
    let model = doc.model;
    doc.put(model, "REGION", solid(REGION_SAT));
    let stats = doc.stats(Options::default());
    assert_eq!(stats.converted.get("REGION"), Some(&1));
    let n = stats.triangles;
    assert!(n > 0);
    let mut doc = Doc::new();
    let model = doc.model;
    doc.put(model, "REGION", solid(REGION_SAT));
    let stats = doc.stats(limits(usize::MAX, n - 1, usize::MAX));
    assert_eq!(stats.triangles, 0);
    assert_eq!(skipped(&stats, "REGION (triangle limit reached)"), 1);
    assert!(warned(&stats, "triangles (the limit)"));
}

#[test]
fn acis_reasons_are_told_apart() {
    let mut doc = Doc::new();
    let model = doc.model;
    // no data; a bad pointer; a cut string; a body without faces
    doc.put(
        model,
        "3DSOLID",
        Shape::Acis {
            data: None,
            wires: vec![],
        },
    );
    doc.put(
        model,
        "3DSOLID",
        solid("400 0 1 0\nheader\n1 1e-06 1e-10\nbody $x #\n"),
    );
    let mut garbage = REGION_SAT.to_string();
    garbage = garbage.replace("body $-1 ", "body $-1 @18446744073709551615 ");
    doc.put(model, "BODY", solid(&garbage));
    let empty = "400 0 1 0\nheader\n1 1e-06 1e-10\nbody $-1 $-1 $-1 $-1 #\nEnd-of-ACIS-data\n";
    doc.put(model, "REGION", solid(empty));
    doc.put(model, "LINE", line());
    let stats = doc.stats(Options::default());
    assert_eq!(skipped(&stats, "3DSOLID (no ACIS data)"), 1, "{stats}");
    assert_eq!(
        skipped(&stats, "3DSOLID (unreadable ACIS data)"),
        1,
        "{stats}"
    );
    // a string length past the end (used to panic) leaves no faces
    assert_eq!(
        skipped(&stats, "BODY (ACIS data gave no triangles)"),
        1,
        "{stats}"
    );
    assert_eq!(
        skipped(&stats, "REGION (ACIS data gave no triangles)"),
        1,
        "{stats}"
    );
    // each skipped once, with one reason
    let total: usize = stats.skipped.values().sum();
    assert_eq!(total, 4, "{stats}");
}

// -- curves -------------------------------------------------------------------

#[test]
fn damaged_arc_angles_stay_bounded() {
    let mut doc = Doc::new();
    let model = doc.model;
    let arc = |a0: f64, a1: f64| Shape::Arc {
        center: [0.0; 3],
        radius: 1000.0,
        start_angle: a0,
        end_angle: a1,
    };
    // "capacity overflow" before: billions of segments
    doc.put(model, "ARC", arc(0.0, 1e300));
    doc.put(model, "ARC", arc(-1e300, 1e300));
    doc.put(model, "ARC", arc(0.0, f64::INFINITY));
    doc.put(model, "ARC", arc(f64::NAN, 1.0));
    doc.put(
        model,
        "ELLIPSE",
        Shape::Ellipse {
            center: [0.0; 3],
            major: [1000.0, 0.0, 0.0],
            ratio: 0.5,
            start_angle: -1e300,
            end_angle: 1e300,
        },
    );
    let stats = doc.stats(Options::default());
    assert_eq!(skipped(&stats, "ARC (no geometry)"), 2);
    // at most one full turn each: 256 segments
    assert!(stats.lines <= 3 * 256, "{}", stats.lines);
}

#[test]
fn spline_of_absurd_degree_is_skipped() {
    let mut doc = Doc::new();
    let model = doc.model;
    let d = 101;
    let ctrl: Vec<[f64; 3]> = (0..=d).map(|i| [i as f64, 0.0, 0.0]).collect();
    let mut knots = vec![0.0; d + 1];
    knots.extend(vec![1.0; d + 1]);
    doc.put(
        model,
        "SPLINE",
        Shape::Spline {
            flag: 0,
            degree: d,
            weights: vec![1.0; ctrl.len()],
            ctrl,
            knots,
            fit: vec![],
        },
    );
    let stats = doc.stats(Options::default());
    assert_eq!(skipped(&stats, "SPLINE (degree too high)"), 1);
}

// -- losses that must be reported -----------------------------------------------

#[test]
fn xref_inserts_are_reported() {
    let mut doc = Doc::new();
    let x = doc.block("SITE");
    if let Data::BlockHeader(b) = &mut doc.drawing.objects.last_mut().unwrap().data {
        b.xref = Some("..\\site.dwg".into());
    }
    let model = doc.model;
    doc.put(model, "INSERT", insert(x, 0, 0));
    doc.put(model, "INSERT", insert(x, 0, 0));
    doc.put(model, "3DFACE", face(0.0));
    let stats = doc.stats(Options::default());
    assert_eq!(skipped(&stats, "INSERT (xref)"), 2);
    assert_eq!(stats.converted.get("INSERT"), None);
    let w: Vec<_> = stats
        .warnings
        .iter()
        .filter(|w| w.contains("external reference"))
        .collect();
    assert_eq!(w, ["INSERT of external reference SITE (not resolved)"]);
}

#[test]
fn missing_and_zero_scale_inserts_are_not_converted() {
    let mut doc = Doc::new();
    let b = doc.block("B");
    doc.put(b, "3DFACE", face(0.0));
    let model = doc.model;
    doc.put(model, "INSERT", insert(0xDEAD, 0, 0));
    let mut zero = insert(b, 0, 0);
    if let Shape::Insert { scale, .. } = &mut zero {
        *scale = [1.0, 0.0, 1.0];
    }
    doc.put(model, "INSERT", zero);
    doc.put(model, "INSERT", insert(b, 0, 0));
    let stats = doc.stats(Options::default());
    assert_eq!(stats.converted.get("INSERT"), Some(&1));
    assert_eq!(skipped(&stats, "INSERT (missing block)"), 1);
    assert_eq!(skipped(&stats, "INSERT (zero scale)"), 1);
}

#[test]
fn recoverable_decode_errors_are_reported_once() {
    let mut doc = Doc::new();
    doc.drawing.decode_error = 0x40;
    let model = doc.model;
    doc.put(model, "3DFACE", face(0.0));
    let stats = doc.stats(Options::default());
    let w: Vec<_> = stats
        .warnings
        .iter()
        .filter(|w| w.contains("LibreDWG"))
        .collect();
    assert_eq!(
        w,
        ["LibreDWG reported recoverable decode errors (0x40): output may be incomplete"]
    );
    let mut doc = Doc::new();
    let model = doc.model;
    doc.put(model, "3DFACE", face(0.0));
    assert!(!warned(&doc.stats(Options::default()), "LibreDWG"));
}

#[test]
fn paper_space_content_is_reported() {
    let mut doc = Doc::new();
    let paper = doc.block("*Paper_Space");
    doc.put(paper, "3DFACE", face(0.0));
    doc.put(paper, "LINE", line());
    doc.put(paper, "VIEWPORT", Shape::Other);
    doc.put(paper, "TEXT", Shape::Other);
    let model = doc.model;
    doc.put(model, "3DFACE", face(0.0));
    let stats = doc.stats(Options::default());
    assert!(warned(
        &stats,
        "paper space has 2 entities that are not converted"
    ));
    // nothing in model space: said more strongly
    let mut doc = Doc::new();
    let paper = doc.block("*Paper_Space0");
    doc.put(paper, "3DFACE", face(0.0));
    let stats = doc.stats(Options::default());
    assert!(warned(
        &stats,
        "model space is empty, but paper space has 1 entities, which are not converted"
    ));
    // pre-R2004: paper space entities are marked, not listed
    let mut doc = Doc::new();
    let model = doc.model;
    let h = doc.put(model, "3DFACE", face(0.0));
    doc.put(model, "3DFACE", face(2.0));
    for o in &mut doc.drawing.objects {
        if let (true, Data::Entity(e)) = (o.handle == h, &mut o.data) {
            e.entmode = 1;
        }
    }
    let stats = doc.stats(Options::default());
    assert!(warned(&stats, "paper space has 1 entities"));
    // no paper space: no warning
    let mut doc = Doc::new();
    let model = doc.model;
    doc.put(model, "3DFACE", face(0.0));
    assert!(!warned(&doc.stats(Options::default()), "paper space"));
}

fn tempdir(name: &str) -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("dwg2gltf-limits-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

#[test]
fn empty_output_is_an_error_unless_allowed() {
    let dir = tempdir("empty");
    let mut doc = Doc::new();
    let model = doc.model;
    doc.put(model, "TEXT", Shape::Other);
    let doc = Document::new(doc.drawing, None);
    let dst = dir.join("empty.glb");
    let err = dwg2gltf::write_document(&doc, &dst, &Options::default()).unwrap_err();
    assert!(matches!(err, Error::Empty(_)), "{err}");
    assert!(err.to_string().contains("nothing to convert"), "{err}");
    assert!(err.to_string().contains("TEXT (annotation)=1"), "{err}");
    assert!(!dst.exists());
    let allow = Options {
        allow_empty: true,
        ..Options::default()
    };
    let stats = dwg2gltf::write_document(&doc, &dst, &allow).unwrap();
    assert!(stats.is_empty());
    assert!(dst.exists());
    // lines alone are output
    let mut lines = Doc::new();
    let model = lines.model;
    lines.put(model, "LINE", line());
    let lines = Document::new(lines.drawing, None);
    assert!(dwg2gltf::write_document(&lines, &dst, &Options::default()).is_ok());
    // a series DWG whose blocks have nothing to show
    let mut series = Doc::new();
    let b = series.block("ONLY TEXT");
    series.put(b, "TEXT", Shape::Other);
    let series = Document::new(series.drawing, None);
    // (TEXT is content, so the block is a series block, but it is empty)
    let err =
        dwg2gltf::write_series(&series, &dir.join("s"), "glb", &Options::default()).unwrap_err();
    assert!(matches!(err, Error::Empty(_)), "{err}");
    assert!(!dir.join("s").exists());
    assert!(dwg2gltf::write_series(&series, &dir.join("s"), "glb", &allow).is_ok());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn arc_end_is_at_most_one_turn_on() {
    use dwg2gltf::geom::{arc_end, arc_segments};
    use std::f64::consts::TAU;
    assert_eq!(arc_end(0.0, 1.0), Some(1.0));
    assert_eq!(arc_end(1.0, 0.0), Some(TAU));
    assert_eq!(arc_end(0.0, 0.0), Some(TAU));
    assert_eq!(arc_end(0.0, 100.0), Some(TAU));
    assert_eq!(arc_end(1e300, 0.0), Some(1e300 + TAU));
    assert_eq!(arc_end(f64::NAN, 0.0), None);
    assert_eq!(arc_end(0.0, f64::INFINITY), None);
    // garbage never asks for more points than the limit
    assert_eq!(arc_segments(1.0, f64::INFINITY, 1e-9), 1);
    assert_eq!(arc_segments(f64::NAN, 1.0, 1e-9), 1);
    assert!(arc_segments(1e300, 1e300, 1e-9) <= dwg2gltf::limits::MAX_LINE_POINTS);
}
