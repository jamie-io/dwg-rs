// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Builds scaled synthetic inputs to measure where the converter runs out of
//! memory, time or stack; the measurements behind `src/limits.rs`.
//!
//! cargo build --release --example limit_probe
//! systemd-run --user --scope -p MemoryMax=8G timeout 120 \
//!     target/release/examples/limit_probe MODE ARGS
//!
//! It prints its time and peak RSS (VmHWM) when it gets to the end.
//!
//! Modes:
//!   fanout F L    blocks nested L deep, each inserting the next F times
//!   selfins K     a block with a face that inserts itself K times
//!   minsert C R   one MINSERT of a one-face block, C columns x R rows
//!   mesh N        one MESH of N triangles (bytes per triangle end to end)
//!   arc A0 A1     an ARC from angle A0 to A1
//!   ellipse T0 T1 an ELLIPSE from parameter T0 to T1
//!   spline D      a SPLINE of degree D (D + 1 control points)
//!   satnest D     SAT text with subtype blocks nested D deep
//!   sabnest D     the same as SAB
//!   satknots M    a nubs surface whose knot multiplicities sum to 2 M
//!   acis FILE     a SAT (.sat) or SAB file through parse, B-rep, tessellation
//!   survey DWG... the largest real values in DWG files (one line each)
//!
//! PROBE_STACK=<bytes> runs the probe on a thread with that stack size.

use std::time::Instant;

use dwg2gltf::acis::brep::Brep;
use dwg2gltf::acis::sab::{AcisData, Field, parse};
use dwg2gltf::acis::tessellate::Tessellator;
use dwg2gltf::dwg::{BlockHeader, Color, Data, Drawing, Entity, Handle, Layer, Object, Shape};
use dwg2gltf::{Converter, Document, Options};

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

    fn put(&mut self, block: Handle, kind: &str, shape: Shape) {
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
    }

    fn run(self) {
        let doc = Document::new(self.drawing, None);
        match Converter::new(&doc, Options::default()).run() {
            Ok((gltf, stats)) => {
                let out =
                    std::env::temp_dir().join(format!("limit_probe_{}.glb", std::process::id()));
                gltf.write(&out).unwrap();
                let size = std::fs::metadata(&out).map(|m| m.len()).unwrap_or(0);
                let _ = std::fs::remove_file(&out);
                println!("{stats}\nglb bytes: {size}");
            }
            Err(e) => println!("error: {e}"),
        }
    }
}

fn face() -> Shape {
    Shape::Face3d([
        [0.0, 0.0, 0.0],
        [1.0, 0.0, 0.0],
        [1.0, 1.0, 0.0],
        [0.0, 1.0, 0.0],
    ])
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

fn acis(data: &AcisData) {
    let t = Instant::now();
    match parse(data) {
        Err(e) => println!("parse error: {e}"),
        Ok(model) => {
            let records = model.records.len();
            let bodies = Brep::new(&model).bodies();
            let mut tess = Tessellator::new(1e-4);
            let n: usize = bodies.iter().map(|b| tess.body(b).len()).sum();
            println!(
                "records={records} bodies={} tris={n} failed={} capped={}",
                bodies.len(),
                tess.failed_faces,
                tess.capped_faces
            );
            drop(model);
        }
    }
    println!("acis in {:?}", t.elapsed());
}

const SAT_HEADER: &str = "400 0 1 0\n16 Autodesk AutoCAD 19 ASM 0.0.0 test 24 Thu Jan 01 00:00:00 2026\n1 9.9999999999999995e-07 1e-10\n";

fn main() {
    // PROBE_STACK=<bytes>: run on a thread with that stack (cargo test: 2 MiB)
    match std::env::var("PROBE_STACK")
        .ok()
        .and_then(|s| s.parse().ok())
    {
        Some(size) => std::thread::Builder::new()
            .stack_size(size)
            .spawn(probe)
            .unwrap()
            .join()
            .unwrap(),
        None => probe(),
    }
}

/// Deepest nesting of subtype blocks in a field list.
fn depth(fields: &[Field]) -> usize {
    fields
        .iter()
        .map(|f| match f {
            Field::List(inner) => 1 + depth(inner),
            _ => 0,
        })
        .max()
        .unwrap_or(0)
}

/// How deep INSERTs nest below `block` (cycles cut, at most 64).
fn insert_depth(doc: &Document, block: usize, path: &mut Vec<usize>) -> usize {
    if path.contains(&block) || path.len() > 64 {
        return 0;
    }
    path.push(block);
    let mut deepest = 0;
    for i in doc.block_entities(block) {
        if let Data::Entity(e) = &doc.objects[i].data {
            if let Shape::Insert { block: b, .. } = e.shape {
                if let Some(bi) = doc.index(b) {
                    deepest = deepest.max(1 + insert_depth(doc, bi, path));
                }
            }
        }
    }
    path.pop();
    deepest
}

/// Real values in DWG files: ACIS nesting, triangles, MINSERT cells, ...
fn survey(paths: &[String]) {
    for path in paths {
        let Ok(drawing) = dwg2gltf::dwg::read(std::path::Path::new(path)) else {
            println!("{path}\tunreadable");
            continue;
        };
        let doc = Document::new(drawing, None);
        let (mut nest, mut body_tris, mut cells, mut arc_turns, mut degree) = (0, 0, 0, 0f64, 0);
        let inserts = doc
            .model_space
            .map_or(0, |ms| insert_depth(&doc, ms, &mut vec![]));
        for obj in &doc.objects {
            let Data::Entity(ent) = &obj.data else {
                continue;
            };
            match &ent.shape {
                Shape::Insert { cols, rows, .. } => {
                    cells = cells.max(*cols.max(&1) as usize * *rows.max(&1) as usize)
                }
                Shape::Arc {
                    start_angle,
                    end_angle,
                    ..
                } => arc_turns = arc_turns.max((end_angle - start_angle) / std::f64::consts::TAU),
                Shape::Spline { degree: d, .. } => degree = degree.max(*d),
                _ => {}
            }
            let Some(data) = doc.acis_data(obj) else {
                continue;
            };
            let Ok(model) = parse(&data) else { continue };
            for r in &model.records {
                nest = nest.max(depth(&r.fields));
            }
            let mut tess = Tessellator::new(1e-4);
            for b in Brep::new(&model).bodies() {
                body_tris = body_tris.max(tess.body(&b).len());
            }
        }
        println!(
            "{path}\tnest={nest}\tinsert_depth={inserts}\tbody_tris={body_tris}\tminsert_cells={cells}\tarc_turns={arc_turns:.3}\tspline_degree={degree}"
        );
    }
}

fn probe() {
    let a: Vec<String> = std::env::args().collect();
    let arg = |i: usize| -> f64 { a[i].parse().unwrap() };
    let t = Instant::now();
    match a[1].as_str() {
        "fanout" => {
            let (f, l) = (arg(2) as usize, arg(3) as usize);
            let mut doc = Doc::new();
            let blocks: Vec<Handle> = (0..=l).map(|i| doc.block(&format!("B{i}"))).collect();
            doc.put(blocks[l], "3DFACE", face());
            for i in 0..l {
                for _ in 0..f {
                    doc.put(blocks[i], "INSERT", insert(blocks[i + 1], 0, 0));
                }
            }
            let model = doc.model;
            doc.put(model, "INSERT", insert(blocks[0], 0, 0));
            doc.run();
        }
        "selfins" => {
            let k = arg(2) as usize;
            let mut doc = Doc::new();
            let b = doc.block("S");
            doc.put(b, "3DFACE", face());
            for _ in 0..k {
                doc.put(b, "INSERT", insert(b, 0, 0));
            }
            let model = doc.model;
            doc.put(model, "INSERT", insert(b, 0, 0));
            doc.run();
        }
        "minsert" => {
            let (c, r) = (arg(2) as u16, arg(3) as u16);
            let mut doc = Doc::new();
            let b = doc.block("M");
            doc.put(b, "3DFACE", face());
            let model = doc.model;
            doc.put(model, "MINSERT", insert(b, c, r));
            doc.run();
        }
        "mesh" => {
            // a k x k grid of quads, two triangles each
            let n = arg(2) as usize;
            let k = ((n / 2) as f64).sqrt().ceil() as usize;
            let mut vertices = vec![];
            for i in 0..=k {
                for j in 0..=k {
                    vertices.push([i as f64, j as f64, ((i * 7 + j * 13) % 5) as f64 * 0.1]);
                }
            }
            let mut faces = vec![];
            for i in 0..k {
                for j in 0..k {
                    let v = |i: usize, j: usize| (i * (k + 1) + j) as u32;
                    faces.extend([4, v(i, j), v(i + 1, j), v(i + 1, j + 1), v(i, j + 1)]);
                }
            }
            let mut doc = Doc::new();
            let model = doc.model;
            doc.put(model, "MESH", Shape::Mesh { vertices, faces });
            doc.run();
        }
        "arc" => {
            let mut doc = Doc::new();
            let model = doc.model;
            doc.put(
                model,
                "ARC",
                Shape::Arc {
                    center: [0.0; 3],
                    radius: 1000.0,
                    start_angle: arg(2),
                    end_angle: arg(3),
                },
            );
            doc.run();
        }
        "ellipse" => {
            let mut doc = Doc::new();
            let model = doc.model;
            doc.put(
                model,
                "ELLIPSE",
                Shape::Ellipse {
                    center: [0.0; 3],
                    major: [1000.0, 0.0, 0.0],
                    ratio: 0.5,
                    start_angle: arg(2),
                    end_angle: arg(3),
                },
            );
            doc.run();
        }
        "spline" => {
            // degree D through D+1 control points (a clamped Bezier)
            let d = arg(2) as usize;
            let ctrl: Vec<[f64; 3]> = (0..=d).map(|i| [i as f64, (i % 2) as f64, 0.0]).collect();
            let mut knots = vec![0.0; d + 1];
            knots.extend(vec![1.0; d + 1]);
            let mut doc = Doc::new();
            let model = doc.model;
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
            doc.run();
        }
        "satnest" => {
            let d = arg(2) as usize;
            let text = format!(
                "{SAT_HEADER}body $-1 {}#\nEnd-of-ACIS-data\n",
                "{ ".repeat(d)
            );
            acis(&AcisData::Sat(text));
        }
        "sabnest" => {
            let d = arg(2) as usize;
            let mut sab = b"ACIS BinaryFile".to_vec();
            sab.extend(21500i32.to_le_bytes());
            sab.extend(b"\x07\x04test");
            for _ in 0..3 {
                sab.push(6);
                sab.extend(1f64.to_le_bytes());
            }
            sab.extend(b"\x0d\x04body");
            sab.extend(std::iter::repeat_n(0x0f, d));
            sab.push(0x11);
            acis(&AcisData::Sab(sab));
        }
        "satknots" => {
            let m = arg(2) as usize;
            let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("examples/limit_probe_region.sat");
            let region = std::fs::read_to_string(path).unwrap();
            let spl = format!(
                "spline-surface $-1 forward {{ exactsur nubs 1 1 open open none none 1 1 0 {m} 0 {m} }} I I I I #"
            );
            let text: Vec<String> = region
                .lines()
                .map(|l| {
                    if l.starts_with("plane-surface") {
                        spl.clone()
                    } else {
                        l.to_string()
                    }
                })
                .collect();
            acis(&AcisData::Sat(text.join("\n")));
        }
        "acis" => {
            let bytes = std::fs::read(&a[2]).unwrap();
            if a[2].ends_with(".sat") {
                acis(&AcisData::Sat(bytes.iter().map(|&b| b as char).collect()));
            } else {
                acis(&AcisData::Sab(bytes));
            }
        }
        "survey" => survey(&a[2..]),
        m => panic!("unknown mode {m}"),
    }
    let peak = std::fs::read_to_string("/proc/self/status")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("VmHWM"))
                .map(str::to_string)
        })
        .unwrap_or_default();
    println!("total {:?} {peak}", t.elapsed());
}
