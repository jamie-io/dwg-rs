// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Conversion of small synthetic drawings.

use std::cell::Cell;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;

use dwg2gltf::dwg::{
    BlockHeader, Color, Data, Drawing, Entity, Handle, Layer, Material, Object, Shape, XValue,
};
use dwg2gltf::geom::{bulge_points, triangulate};
use dwg2gltf::{Converter, Document, Error, Options, Stats};
use serde_json::{Map, Value};

mod common;
use common::{Corners, bounds, orientation_faults, read_back, world_triangles};

const BYLAYER: Color = Color {
    index: Some(256),
    rgb: None,
};

/// Builds a minimal drawing: layers "0" and "RED", and a model space.
struct Doc {
    drawing: Drawing,
    next: Handle,
    layer0: Handle,
    red: Handle,
    model: Handle,
}

impl Doc {
    fn new(insunits: i32) -> Doc {
        let mut doc = Doc {
            drawing: Drawing {
                insunits,
                ..Drawing::default()
            },
            next: 0x100,
            layer0: 0,
            red: 0,
            model: 0,
        };
        doc.layer0 = doc.add("LAYER", Data::Layer(layer("0", 0xC3000007)));
        doc.red = doc.add("LAYER", Data::Layer(layer("RED", 0xC3000001)));
        doc.model = doc.block("*Model_Space", [0.0; 3]);
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

    fn block(&mut self, name: &str, base_pt: [f64; 3]) -> Handle {
        self.add(
            "BLOCK_HEADER",
            Data::BlockHeader(BlockHeader {
                name: name.into(),
                base_pt,
                entities: Some(vec![]),
                xref: None,
            }),
        )
    }

    fn get(&mut self, h: Handle) -> &mut Object {
        self.drawing
            .objects
            .iter_mut()
            .find(|o| o.handle == h)
            .unwrap()
    }

    /// Add an entity on layer 0 with ByLayer color to a block.
    fn put(&mut self, block: Handle, kind: &str, shape: Shape) -> Handle {
        let ent = entity(self.layer0, shape);
        self.put_entity(block, kind, ent)
    }

    fn put_entity(&mut self, block: Handle, kind: &str, ent: Entity) -> Handle {
        let h = self.add(kind, Data::Entity(Box::new(ent)));
        match &mut self.get(block).data {
            Data::BlockHeader(b) => b.entities.as_mut().unwrap().push(h),
            _ => unreachable!(),
        }
        h
    }

    fn entity_mut(&mut self, h: Handle) -> &mut Entity {
        match &mut self.get(h).data {
            Data::Entity(e) => e,
            _ => unreachable!(),
        }
    }

    fn run(self, options: Options) -> (Map<String, Value>, Stats) {
        let doc = Document::new(self.drawing, None);
        let (gltf, stats) = Converter::new(&doc, options).run().unwrap();
        (gltf.document().clone(), stats)
    }
}

fn layer(name: &str, rgb: u32) -> Layer {
    Layer {
        name: name.into(),
        color: Color {
            index: Some(256),
            rgb: Some(rgb),
        },
        material: 0,
        off: false,
        frozen: false,
    }
}

fn entity(layer: Handle, shape: Shape) -> Entity {
    Entity {
        entmode: 0,
        invisible: false,
        layer,
        color: BYLAYER,
        material_flags: 0,
        material_refs: [0, 0],
        has_ds_data: false,
        thickness: 0.0,
        extrusion: Some([0.0, 0.0, 1.0]),
        material_mapper: None,
        shape,
    }
}

fn square_face() -> Shape {
    Shape::Face3d([
        [0.0, 0.0, 0.0],
        [1000.0, 0.0, 0.0],
        [1000.0, 1000.0, 0.0],
        [0.0, 1000.0, 0.0],
    ])
}

fn insert(block: Handle, x: f64) -> Shape {
    Shape::Insert {
        block,
        ins_pt: [x, 0.0, 0.0],
        scale: [1.0; 3],
        rotation: 0.0,
        cols: 0,
        rows: 0,
        col_spacing: 0.0,
        row_spacing: 0.0,
    }
}

#[test]
fn triangulate_concave_keeps_area() {
    // L-shape, area 3
    let pts = [
        [0.0, 0.0, 0.0],
        [2.0, 0.0, 0.0],
        [2.0, 1.0, 0.0],
        [1.0, 1.0, 0.0],
        [1.0, 2.0, 0.0],
        [0.0, 2.0, 0.0],
    ];
    let tris = triangulate(&pts).tris;
    let area: f64 = tris
        .iter()
        .map(|&[a, b, c]| {
            ((pts[b][0] - pts[a][0]) * (pts[c][1] - pts[a][1])
                - (pts[c][0] - pts[a][0]) * (pts[b][1] - pts[a][1]))
                .abs()
                / 2.0
        })
        .sum();
    assert_eq!(tris.len(), 4);
    assert!((area - 3.0).abs() < 1e-12);
}

#[test]
fn bulge_one_is_semicircle() {
    let pts = bulge_points([0.0; 3], [2.0, 0.0, 0.0], 1.0, 1e-3);
    assert_eq!(*pts.last().unwrap(), [2.0, 0.0, 0.0]);
    for p in &pts {
        assert!(((p[0] - 1.0).hypot(p[1]) - 1.0).abs() < 1e-9);
        assert!(p[1] <= 1e-12); // positive bulge = counter-clockwise, below the chord
    }
}

#[test]
fn units_axes_and_bounds() {
    let mut doc = Doc::new(4); // mm
    let model = doc.model;
    doc.put(model, "3DFACE", square_face());
    let (gl, stats) = doc.run(Options::default());
    let root = &gl["nodes"][gl["scenes"][0]["nodes"][0].as_u64().unwrap() as usize];
    let m = &root["matrix"]; // column-major: scale 0.001 and Z-up -> Y-up
    assert_eq!(m[0], 0.001);
    assert_eq!(m[6], -0.001); // y -> -z
    assert_eq!(m[9], 0.001); // z -> y
    let pos = &gl["accessors"][gl["meshes"][0]["primitives"][0]["attributes"]["POSITION"]
        .as_u64()
        .unwrap() as usize];
    assert_eq!(pos["max"], serde_json::json!([1000.0, 1000.0, 0.0]));
    assert_eq!(stats.triangles, 2);
}

#[test]
fn blocks_are_instanced_and_inherit_byblock() {
    let mut doc = Doc::new(4);
    let blk = doc.block("CHAIR", [0.0; 3]);
    let face = doc.put(blk, "3DFACE", square_face());
    doc.entity_mut(face).color = Color {
        index: None,
        rgb: None,
    }; // ByBlock, layer 0
    let model = doc.model;
    for x in [0.0, 2000.0] {
        let mut ent = entity(doc.red, insert(blk, x));
        ent.color = BYLAYER;
        doc.put_entity(model, "INSERT", ent);
    }
    let (gl, _) = doc.run(Options::default());
    assert_eq!(gl["meshes"].as_array().unwrap().len(), 1); // shared by both INSERT nodes
    let meshed: Vec<&Value> = gl["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|n| n.get("mesh").is_some())
        .collect();
    assert_eq!(meshed.len(), 2);
    assert_eq!(meshed[1]["matrix"][12], 2000.0);
    let mat = &gl["materials"][gl["meshes"][0]["primitives"][0]["material"]
        .as_u64()
        .unwrap() as usize];
    // red from the INSERT's layer
    assert_eq!(
        mat["pbrMetallicRoughness"]["baseColorFactor"],
        serde_json::json!([1.0, 0.0, 0.0, 1.0])
    );
    assert_eq!(mat["doubleSided"], true);
}

fn material_archive(png: &[u8]) -> Vec<u8> {
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts =
        zip::write::SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
    zip.start_file("material.mat", opts).unwrap();
    zip.write_all(b"dif 0.5 0.5 0.5\nmetallic 0\nroughness 0.6\ntex image png oak\nprjz\n")
        .unwrap();
    zip.start_file("oak.png", opts).unwrap();
    zip.write_all(png).unwrap();
    zip.finish().unwrap().into_inner()
}

/// An OFML MESH quad with UVs and an archive material textured with `png`.
fn textured_mesh_doc(png: &[u8]) -> Doc {
    let mut doc = Doc::new(6);
    let rec = doc.add(
        "XRECORD",
        Data::XRecord(vec![(310, XValue::Binary(material_archive(png)))]),
    );
    let matdict = doc.add("DICTIONARY", Data::Dictionary(vec![("oak01".into(), rec)]));
    doc.add(
        "DICTIONARY",
        Data::Dictionary(vec![("EDWG_MATERIAL_DICT".into(), matdict)]),
    );
    let mut transmatrix = [0.0; 16];
    for (i, v) in [
        2.0, 0.0, 0.0, 0.0, 0.0, 2.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
    ]
    .into_iter()
    .enumerate()
    {
        transmatrix[i] = v;
    }
    let mat = doc.add(
        "MATERIAL",
        Data::Material(Material {
            name: "oak01".into(),
            diffuse: Some(0xC2808080),
            opacity: 1.0,
            transmatrix: Some(transmatrix),
            filename: String::new(),
        }),
    );
    let uvs: Vec<u8> = [0.0, 0.0, 1.0, 0.0, 1.0, 1.0, 0.0, f64::NAN]
        .iter()
        .flat_map(|v: &f64| v.to_le_bytes())
        .collect();
    let uv_rec = doc.add("XRECORD", Data::XRecord(vec![(310, XValue::Binary(uvs))]));
    let xdic = doc.add(
        "DICTIONARY",
        Data::Dictionary(vec![("EDWG_TEX_COORDS_2D".into(), uv_rec)]),
    );
    let model = doc.model;
    let mesh = Shape::Mesh {
        vertices: vec![
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ],
        faces: vec![4, 0, 1, 2, 3],
    };
    let h = doc.put(model, "MESH", mesh);
    doc.get(h).xdic = xdic;
    // the material handle sits in LibreDWG's swapped "plotstyle" slot
    doc.entity_mut(h).material_refs = [0, mat];
    doc
}

#[test]
fn ofml_mesh_with_uvs_and_archive_material() {
    let doc = textured_mesh_doc(b"\x89PNG\r\n\x1a\nfake");
    let (gl, _) = doc.run(Options::default());
    let prim = &gl["meshes"][0]["primitives"][0];
    let material = &gl["materials"][prim["material"].as_u64().unwrap() as usize];
    assert_eq!(material["name"], "oak01");
    assert_eq!(
        material["pbrMetallicRoughness"]["baseColorTexture"]["index"],
        0
    );
    assert_eq!(gl["images"][0]["mimeType"], "image/png");
    assert_eq!(
        gl["accessors"][prim["attributes"]["TEXCOORD_0"].as_u64().unwrap() as usize]["count"],
        4
    );
}

/// The texture coordinates of `doc` with the MESH's extension dictionary
/// replaced by one holding `records`.
fn mesh_uvs_with(records: Vec<(&str, Data)>) -> Vec<[f64; 2]> {
    let mut doc = textured_mesh_doc(&noise_png());
    let mut entries = vec![];
    for (key, data) in records {
        let h = doc.add("XRECORD", data);
        entries.push((key.to_string(), h));
    }
    let xdic = doc.add("DICTIONARY", Data::Dictionary(entries));
    let mesh = doc
        .drawing
        .objects
        .iter()
        .find(|o| o.kind == "MESH")
        .unwrap()
        .handle;
    doc.get(mesh).xdic = xdic;
    let (path, _) = glb_of(doc, Options::default());
    read_back(&path).uvs
}

/// Many OFML meshes store their texture coordinates as
/// AutoCAD's ADSK_XREC_SUBDVERTEXTEXCOORDS (43/44/45 per vertex) instead
/// of EDWG_TEX_COORDS_2D; they are used the same way.
#[test]
fn adsk_mesh_texture_coordinates() {
    let uv = [[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.25, 0.75]];
    let edwg: Vec<u8> = uv
        .iter()
        .flatten()
        .flat_map(|v: &f64| v.to_le_bytes())
        .collect();
    let adsk = |n: usize| {
        let mut items = vec![(90, XValue::Int(n as i64))];
        for p in &uv[..n] {
            items.extend([
                (43, XValue::Real(p[0])),
                (44, XValue::Real(p[1])),
                (45, XValue::Real(0.0)),
            ]);
        }
        Data::XRecord(items)
    };
    let stored = mesh_uvs_with(vec![(
        "EDWG_TEX_COORDS_2D",
        Data::XRecord(vec![(310, XValue::Binary(edwg))]),
    )]);
    let from_adsk = mesh_uvs_with(vec![("ADSK_XREC_SUBDVERTEXTEXCOORDS", adsk(4))]);
    let projected = mesh_uvs_with(vec![]);
    assert_eq!(stored.len(), 4);
    assert_eq!(from_adsk, stored);
    assert_ne!(projected, stored);
    // a record for another vertex count is not used
    let short = mesh_uvs_with(vec![("ADSK_XREC_SUBDVERTEXTEXCOORDS", adsk(3))]);
    assert_eq!(short, projected);
}

/// An archive's normal map (`bumps`) becomes the material's normalTexture, also
/// without a base color texture (the faces then get UVs for it).
#[test]
fn archive_normal_map_is_written() {
    let png = noise_png();
    for with_color in [true, false] {
        let mut doc = textured_mesh_doc(&png);
        let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        let opts = zip::write::SimpleFileOptions::default();
        let spec = if with_color {
            "dif 0.5 0.5 0.5\ntex image png oak\nbumps png oakn\nprjz\n"
        } else {
            "dif 0.5 0.5 0.5\nbumps png oakn\nprjz\n"
        };
        zip.start_file("material.mat", opts).unwrap();
        zip.write_all(spec.as_bytes()).unwrap();
        zip.start_file("oak.png", opts).unwrap();
        zip.write_all(&png).unwrap();
        zip.start_file("oakn.png", opts).unwrap();
        zip.write_all(&png[..png.len()]).unwrap();
        let archive = zip.finish().unwrap().into_inner();
        for o in &mut doc.drawing.objects {
            if let Data::XRecord(items) = &mut o.data {
                if let Some((310, XValue::Binary(b))) = items.first_mut() {
                    if b.starts_with(b"PK") {
                        *b = archive.clone();
                    }
                }
            }
        }
        let (gl, _) = doc.run(Options::default());
        let prim = &gl["meshes"][0]["primitives"][0];
        let material = &gl["materials"][prim["material"].as_u64().unwrap() as usize];
        assert!(
            material["normalTexture"]["index"].is_u64(),
            "{with_color}: {material}"
        );
        assert!(prim["attributes"]["TEXCOORD_0"].is_u64(), "{with_color}");
    }
}

#[test]
fn hidden_layers_and_lines() {
    let mut doc = Doc::new(4);
    let mut hidden = layer("OFF", 0xC3000001);
    hidden.off = true;
    let hidden = doc.add("LAYER", Data::Layer(hidden));
    let model = doc.model;
    let face = doc.put(model, "3DFACE", square_face());
    doc.entity_mut(face).layer = hidden;
    doc.put(
        model,
        "CIRCLE",
        Shape::Circle {
            center: [0.0; 3],
            radius: 10.0,
        },
    );
    let drawing = std::mem::take(&mut doc.drawing);

    let document = Document::new(drawing, None);
    let (gltf, stats) = Converter::new(&document, Options::default()).run().unwrap();
    assert_eq!(stats.skipped["3DFACE (hidden layer)"], 1);
    let modes: Vec<&Value> = gltf.document()["meshes"][0]["primitives"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| &p["mode"])
        .collect();
    assert_eq!(modes, [1]);
    let (gltf, _) = Converter::new(
        &document,
        Options {
            lines: false,
            ..Options::default()
        },
    )
    .run()
    .unwrap();
    assert_eq!(gltf.document()["scenes"][0]["nodes"], serde_json::json!([]));
}

/// OFML 2D plan symbols (layers 72_*_D2*, blocks PEC_GFJ_GEO*) are not
/// part of the 3D model: their wire geometry is left out, the rest stays.
#[test]
fn ofml_plan_symbols_have_no_lines() {
    let mut doc = Doc::new(6);
    let d2 = doc.add("LAYER", Data::Layer(layer("72_ABC_D2_GRAFIK", 0xC3000001)));
    let d2detail = doc.add(
        "LAYER",
        Data::Layer(layer("72_ABC_D2DETAIL_ANY", 0xC3000001)),
    );
    let d3 = doc.add("LAYER", Data::Layer(layer("72_ABC_D3_ANY", 0xC3000001)));
    let line = |x: f64| Shape::Line {
        start: [x, 0.0, 0.0],
        end: [x, 1.0, 0.0],
    };
    let model = doc.model;
    for (layer, x) in [(d2, 0.0), (d2detail, 1.0), (d3, 2.0)] {
        let h = doc.put(model, "LINE", line(x));
        doc.entity_mut(h).layer = layer;
    }
    // a face on a 2D layer stays (only wire geometry goes)
    let face = doc.put(model, "3DFACE", square_face());
    doc.entity_mut(face).layer = d2;
    // everything in a PEC_GFJ_GEO block goes, also in blocks nested in it
    let inner = doc.block("INNER", [0.0; 3]);
    doc.put(inner, "LINE", line(3.0));
    let symbol = doc.block("PEC_GFJ_GEO_1", [0.0; 3]);
    doc.put(
        symbol,
        "CIRCLE",
        Shape::Circle {
            center: [0.0; 3],
            radius: 1.0,
        },
    );
    doc.put(symbol, "INSERT", insert(inner, 0.0));
    doc.put(model, "INSERT", insert(symbol, 0.0));
    // the same inner block outside the symbol keeps its line
    doc.put(model, "INSERT", insert(inner, 10.0));
    let (path, stats) = glb_of(doc, Options::default());
    let r = read_back(&path);
    assert_eq!(r.lines, 2, "the D3 line and the inner block's own instance");
    assert_eq!(r.faces, 2, "the square on the 2D layer");
    assert_eq!(stats.skipped["LINE (2D plan symbol)"], 3);
    assert_eq!(stats.skipped["CIRCLE (2D plan symbol)"], 1);
}

#[test]
fn ofml_plan_symbols_only_on_request() {
    // an article's 2D plan symbol is on OFML "..._D2_..." layers and its 3D
    // geometry on "..._D3_..." layers
    let mut doc = Doc::new(4);
    let d2 = doc.add("LAYER", Data::Layer(layer("72_XY_AB_D2_ANY", 0xC3000001)));
    let d3 = doc.add(
        "LAYER",
        Data::Layer(layer("72_XYZ_AB_D3_CH_ANY", 0xC3000001)),
    );
    let model = doc.model;
    let circle = doc.put(
        model,
        "CIRCLE",
        Shape::Circle {
            center: [0.0; 3],
            radius: 10.0,
        },
    );
    doc.entity_mut(circle).layer = d2;
    let face = doc.put(model, "3DFACE", square_face());
    doc.entity_mut(face).layer = d3;
    let drawing = std::mem::take(&mut doc.drawing);
    let document = Document::new(drawing, None);

    let modes = |options: Options| {
        let (gltf, stats) = Converter::new(&document, options).run().unwrap();
        let mut modes: Vec<i64> = gltf.document()["meshes"]
            .as_array()
            .unwrap()
            .iter()
            .flat_map(|m| m["primitives"].as_array().unwrap())
            .map(|p| p["mode"].as_i64().unwrap_or(4))
            .collect();
        modes.sort();
        (modes, stats)
    };
    let (default, stats) = modes(Options::default());
    assert_eq!(default, [4]);
    assert_eq!(stats.skipped["CIRCLE (2D plan symbol)"], 1);
    let (with_plan, _) = modes(Options {
        plan_symbols: true,
        ..Options::default()
    });
    assert_eq!(with_plan, [1, 4]);
}

#[test]
fn glb_layout() {
    let mut doc = Doc::new(4);
    let model = doc.model;
    doc.put(model, "3DFACE", square_face());
    let document = Document::new(doc.drawing, None);
    let (gltf, _) = Converter::new(&document, Options::default()).run().unwrap();
    let dir = tempdir();
    let out = dir.join("x.glb");
    gltf.write(&out).unwrap();
    let data = std::fs::read(&out).unwrap();
    let word = |at: usize| u32::from_le_bytes(data[at..at + 4].try_into().unwrap());
    assert_eq!(
        (word(0), word(4), word(8)),
        (0x46546C67, 2, data.len() as u32)
    );
    let jlen = word(12) as usize;
    assert_eq!(word(16), 0x4E4F534A);
    let js: Value = serde_json::from_slice(&data[20..20 + jlen]).unwrap();
    assert_eq!(word(24 + jlen), 0x004E4942);
    assert_eq!(js["buffers"][0]["byteLength"], word(20 + jlen));
    std::fs::remove_dir_all(dir).unwrap();
}

fn tempdir() -> PathBuf {
    let dir = std::env::temp_dir().join(format!(
        "dwg2gltf-test-{}-{:?}",
        std::process::id(),
        std::thread::current().id()
    ));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A 64x64 noise PNG, which WebP stores in fewer bytes.
fn noise_png() -> Vec<u8> {
    let mut seed = 1u32;
    let img = image::RgbImage::from_fn(64, 64, |_, _| {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        image::Rgb([(seed >> 24) as u8, (seed >> 16) as u8, (seed >> 8) as u8])
    });
    let mut png = vec![];
    img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .unwrap();
    png
}

/// --compress writes quantized meshopt geometry and WebP textures that the
/// glTF reader (and so gltf2dwg) takes back.
#[test]
fn compressed_glb_reads_back() {
    let png = noise_png();
    let build = || {
        let mut doc = textured_mesh_doc(&png);
        let blk = doc.block("CHAIR", [0.0; 3]);
        doc.put(blk, "3DFACE", square_face());
        let model = doc.model;
        for x in [0.0, 2000.0] {
            doc.put(model, "INSERT", insert(blk, x));
        }
        doc.put(
            model,
            "CIRCLE",
            Shape::Circle {
                center: [0.0; 3],
                radius: 10.0,
            },
        );
        Document::new(doc.drawing, None)
    };
    let dir = tempdir();
    let (plain, packed) = (dir.join("plain.glb"), dir.join("packed.glb"));
    let doc = build();
    dwg2gltf::Converter::new(&doc, Options::default())
        .run()
        .unwrap()
        .0
        .write(&plain)
        .unwrap();
    let (mut gltf, _) = Converter::new(&build(), Options::default()).run().unwrap();
    gltf.compress();
    gltf.write(&packed).unwrap();

    let data = std::fs::read(&packed).unwrap();
    let jlen = u32::from_le_bytes(data[12..16].try_into().unwrap()) as usize;
    let js: Value = serde_json::from_slice(&data[20..20 + jlen]).unwrap();
    #[cfg(feature = "webp")]
    {
        assert_eq!(
            js["extensionsRequired"],
            serde_json::json!([
                "EXT_meshopt_compression",
                "EXT_texture_webp",
                "KHR_mesh_quantization"
            ])
        );
        assert_eq!(js["images"][0]["mimeType"], "image/webp");
    }
    #[cfg(not(feature = "webp"))]
    assert_eq!(
        js["extensionsRequired"],
        serde_json::json!(["EXT_meshopt_compression", "KHR_mesh_quantization"])
    );
    assert!(data.len() < std::fs::metadata(&plain).unwrap().len() as usize);

    let (a, b) = (read_back(&plain), read_back(&packed));
    assert_eq!((a.faces, a.lines), (b.faces, b.lines));
    assert!(a.faces >= 3 && a.lines > 0);
    let (lo, hi) = bounds(&a.points);
    let step = (0..3).map(|k| hi[k] - lo[k]).fold(0.0, f64::max) / 65535.0;
    assert_eq!(a.points.len(), b.points.len());
    for p in &a.points {
        // every vertex has a quantized twin within one step per axis
        assert!(
            b.points
                .iter()
                .any(|q| (0..3).all(|k| (p[k] - q[k]).abs() <= step)),
            "{p:?}"
        );
    }
    let sorted_uvs = |mut v: Vec<[f64; 2]>| {
        v.sort_by(|x, y| x.partial_cmp(y).unwrap());
        v
    };
    assert_eq!(sorted_uvs(a.uvs), sorted_uvs(b.uvs));
    // the (WebP) texture comes back as PNG, for OFML material archives
    let scene = dwg2gltf::input::load(&packed).unwrap();
    let tex = scene
        .materials
        .iter()
        .find_map(|m| m.texture.as_ref())
        .unwrap();
    assert_eq!(tex.ext, "png");
    let img = image::load_from_memory(&tex.data).unwrap();
    assert_eq!((img.width(), img.height()), (64, 64));
    std::fs::remove_dir_all(dir).unwrap();
}

// -- orientation ---------------------------------------------------------------

fn glb_of(doc: Doc, options: Options) -> (PathBuf, Stats) {
    let path = tempdir().join(format!("{:?}.glb", std::time::Instant::now()));
    let document = Document::new(doc.drawing, None);
    let (gltf, stats) = Converter::new(&document, options).run().unwrap();
    gltf.write(&path).unwrap();
    (path, stats)
}

fn solid(corners: [[f64; 2]; 4]) -> Shape {
    Shape::Solid {
        corners: corners.map(|c| [c[0], c[1], 0.0]),
        elevation: 0.0,
    }
}

#[test]
fn solids_and_thick_circles_are_closed_and_outward() {
    let mut doc = Doc::new(4);
    let model = doc.model;
    // corners 1-2-4-3: a square, 5 thick
    let s = doc.put(
        model,
        "SOLID",
        solid([[0.0, 0.0], [10.0, 0.0], [0.0, 10.0], [10.0, 10.0]]),
    );
    doc.entity_mut(s).thickness = 5.0;
    // a circle going down
    let c = doc.put(
        model,
        "CIRCLE",
        Shape::Circle {
            center: [50.0, 0.0, 0.0],
            radius: 3.0,
        },
    );
    doc.entity_mut(c).thickness = -4.0;
    let (path, _) = glb_of(doc, Options::default());
    let (flipped, inside_out, volume) = orientation_faults(&path);
    assert_eq!((flipped, inside_out), (0, 0));
    // 10 x 10 x 5 mm, plus the cylinder: a 13-gon at 0.1 mm deviation
    // (r = 3 mm) covers 96 % of the circle
    let mm3 = volume * 1e9;
    let cylinder = std::f64::consts::PI * 9.0 * 4.0;
    assert!(
        mm3 > 500.0 + 0.95 * cylinder && mm3 < 500.0 + cylinder,
        "{mm3}"
    );
}

#[test]
fn crossed_solid_is_two_lobes_facing_up() {
    let mut doc = Doc::new(6);
    let model = doc.model;
    // corners given around the perimeter: the outline 1-2-4-3 crosses itself
    doc.put(
        model,
        "SOLID",
        solid([[0.0, 0.0], [1.0, 0.0], [1.0, 1.0], [0.0, 1.0]]),
    );
    let (path, _) = glb_of(doc, Options::default());
    let tris = world_triangles(&path);
    assert_eq!(tris.len(), 2);
    let area: f64 = tris
        .iter()
        .map(|(t, _)| {
            let g = dwg2gltf::geom::cross(
                dwg2gltf::geom::sub(t[1], t[0]),
                dwg2gltf::geom::sub(t[2], t[0]),
            );
            // DWG +z is glTF +y
            assert!(g[1] > 0.0);
            dwg2gltf::geom::length(g) / 2.0
        })
        .sum();
    assert!((area - 0.5).abs() < 1e-6, "{area}");
}

#[test]
fn inside_out_faces_are_turned_and_reported() {
    let mut doc = Doc::new(6);
    let model = doc.model;
    // a unit cube of 3DFACEs, every face wound inwards
    let p = |x: f64, y: f64, z: f64| [x, y, z];
    let quads = [
        [
            p(0.0, 0.0, 0.0),
            p(1.0, 0.0, 0.0),
            p(1.0, 1.0, 0.0),
            p(0.0, 1.0, 0.0),
        ],
        [
            p(0.0, 0.0, 1.0),
            p(0.0, 1.0, 1.0),
            p(1.0, 1.0, 1.0),
            p(1.0, 0.0, 1.0),
        ],
        [
            p(0.0, 0.0, 0.0),
            p(0.0, 0.0, 1.0),
            p(1.0, 0.0, 1.0),
            p(1.0, 0.0, 0.0),
        ],
        [
            p(0.0, 1.0, 0.0),
            p(1.0, 1.0, 0.0),
            p(1.0, 1.0, 1.0),
            p(0.0, 1.0, 1.0),
        ],
        [
            p(0.0, 0.0, 0.0),
            p(0.0, 1.0, 0.0),
            p(0.0, 1.0, 1.0),
            p(0.0, 0.0, 1.0),
        ],
        [
            p(1.0, 0.0, 0.0),
            p(1.0, 0.0, 1.0),
            p(1.0, 1.0, 1.0),
            p(1.0, 1.0, 0.0),
        ],
    ];
    for q in quads {
        doc.put(model, "3DFACE", Shape::Face3d(q));
    }
    // a MESH quad whose stored normals point against its winding
    let m = doc.put(
        model,
        "MESH",
        Shape::Mesh {
            vertices: vec![
                p(5.0, 0.0, 0.0),
                p(6.0, 0.0, 0.0),
                p(6.0, 1.0, 0.0),
                p(5.0, 1.0, 0.0),
            ],
            faces: vec![4, 0, 1, 2, 3],
        },
    );
    let normals = doc.add(
        "XRECORD",
        Data::XRecord(
            (0..4)
                .flat_map(|_| {
                    [
                        (10, XValue::Real(0.0)),
                        (20, XValue::Real(0.0)),
                        (30, XValue::Real(-1.0)),
                    ]
                })
                .collect(),
        ),
    );
    let xdic = doc.add(
        "DICTIONARY",
        Data::Dictionary(vec![("ADSK_XREC_SUBDVERTEXNORMALS".into(), normals)]),
    );
    doc.get(m).xdic = xdic;
    let (path, stats) = glb_of(doc, Options::default());
    let (flipped, inside_out, volume) = orientation_faults(&path);
    assert_eq!((flipped, inside_out), (0, 0));
    assert!((volume - 1.0).abs() < 1e-9, "{volume}");
    assert_eq!(
        stats
            .repaired
            .get("triangle turned to match its neighbours"),
        Some(&12)
    );
    assert_eq!(
        stats.repaired.get("triangle turned to its stored normals"),
        Some(&2)
    );
    // the MESH quad now faces down (DWG -z, glTF -y)
    let mesh_tris: Vec<_> = world_triangles(&path)
        .into_iter()
        .filter(|(t, _)| t[0][0] > 4.0)
        .collect();
    assert_eq!(mesh_tris.len(), 2);
    for (t, _) in mesh_tris {
        let g = dwg2gltf::geom::cross(
            dwg2gltf::geom::sub(t[1], t[0]),
            dwg2gltf::geom::sub(t[2], t[0]),
        );
        assert!(g[1] < 0.0);
    }
}

#[test]
fn zero_scale_insert_and_off_layer_insert() {
    let mut doc = Doc::new(6);
    let model = doc.model;
    let blk = doc.block("B", [0.0; 3]);
    let red = doc.red;
    // in the block: one face on layer 0 (takes the INSERT's layer), one on RED
    doc.put(blk, "3DFACE", square_face());
    let on_red = entity(red, square_face());
    doc.put_entity(blk, "3DFACE", on_red);
    let mut zero = insert(blk, 0.0);
    if let Shape::Insert { scale, .. } = &mut zero {
        *scale = [1.0, 0.0, 1.0];
    }
    doc.put(model, "INSERT", zero);
    // an INSERT on an off layer: its layer-0 content hides, RED's shows
    let off = doc.add(
        "LAYER",
        Data::Layer(Layer {
            off: true,
            ..layer("OFF", 0xC3000002)
        }),
    );
    let ins = entity(off, insert(blk, 5000.0));
    doc.put_entity(model, "INSERT", ins);
    let (path, stats) = glb_of(doc, Options::default());
    assert_eq!(stats.skipped.get("INSERT (zero scale)"), Some(&1));
    assert_eq!(world_triangles(&path).len(), 2);
}

/// A GLB of `tris` (one primitive, flat normals) without the converter,
/// so nothing repairs their orientation.
fn raw_glb(tris: &[[[f64; 3]; 3]]) -> PathBuf {
    use dwg2gltf::geom::{cross, normalize, sub};
    let mut w = dwg2gltf::GltfWriter::new("test");
    let positions: Vec<[f64; 3]> = tris.iter().flatten().copied().collect();
    let normals: Vec<[f64; 3]> = tris
        .iter()
        .flat_map(|t| [normalize(cross(sub(t[1], t[0]), sub(t[2], t[0]))); 3])
        .collect();
    let indices: Vec<u32> = (0..positions.len() as u32).collect();
    let prim = serde_json::json!({
        "attributes": {
            "POSITION": w.vec_accessor(&positions, true),
            "NORMAL": w.vec_accessor(&normals, false),
        },
        "indices": w.index_accessor(&indices, positions.len()),
        "mode": 4,
    });
    let mesh = w.add_mesh("raw", vec![prim]);
    let node = w.add_node("raw", None, Some(mesh), &[], None);
    w.add_root(node);
    let path = tempdir().join(format!("raw-{:?}.glb", std::time::Instant::now()));
    w.write(&path).unwrap();
    path
}

/// The 12 triangles of the box `lo`..`hi`, facing out (or in).
fn box_tris(lo: [f64; 3], hi: [f64; 3], outwards: bool) -> Vec<[[f64; 3]; 3]> {
    let c = |i: usize| -> [f64; 3] {
        std::array::from_fn(|k| if i >> k & 1 == 0 { lo[k] } else { hi[k] })
    };
    // corners by bits (x, y, z); each side counter-clockwise seen from outside
    let quads = [
        [0, 2, 3, 1],
        [4, 5, 7, 6],
        [0, 1, 5, 4],
        [2, 6, 7, 3],
        [0, 4, 6, 2],
        [1, 3, 7, 5],
    ];
    let mut out = vec![];
    for q in quads {
        for t in [[q[0], q[1], q[2]], [q[0], q[2], q[3]]] {
            let mut t = t.map(c);
            if !outwards {
                t.swap(1, 2);
            }
            out.push(t);
        }
    }
    out
}

/// A cavity's wall faces into the cavity, and two triangles back to back
/// enclose nothing: neither is an inside-out shell. A lone box facing in is.
#[test]
fn inside_out_check_knows_cavities() {
    let outer = box_tris([0.0; 3], [1.0; 3], true);
    let (flipped, inside_out, volume) = orientation_faults(&raw_glb(&outer));
    assert_eq!((flipped, inside_out), (0, 0));
    assert!((volume - 1.0).abs() < 1e-9, "{volume}");
    let lone = box_tris([0.0; 3], [1.0; 3], false);
    assert_eq!(orientation_faults(&raw_glb(&lone)).1, 1);

    // a hollow box: the inner wall faces into the cavity
    let mut hollow = outer.clone();
    hollow.extend(box_tris([0.25; 3], [0.75; 3], false));
    assert_eq!(orientation_faults(&raw_glb(&hollow)).1, 0);
    // ... but a box facing in next to another one is inside out
    let mut beside = outer.clone();
    beside.extend(box_tris([2.0, 0.25, 0.25], [2.5, 0.75, 0.75], false));
    assert_eq!(orientation_faults(&raw_glb(&beside)).1, 1);

    // two triangles back to back (a zero-thickness double face), whose
    // volume rounds to slightly below 0, as such pairs in real drawings do
    use dwg2gltf::geom::{cross, dot};
    let f32s = |p: [f64; 3]| p.map(|c| c as f32 as f64);
    let pair = (1..200)
        .map(|k| {
            let a = f32s([3.0 + 0.001 * k as f64, 0.1, 0.2]);
            let (b, c) = (f32s([3.3, 0.17, 0.25]), f32s([3.1, 0.71, 0.33]));
            [[a, b, c], [c, b, a]]
        })
        .find(|pair| {
            pair.iter()
                .map(|t| dot(t[0], cross(t[1], t[2])))
                .sum::<f64>()
                < 0.0
        })
        .expect("a pair whose volume rounds below 0");
    let mut with_pair = outer.clone();
    with_pair.extend(pair);
    assert_eq!(orientation_faults(&raw_glb(&with_pair)).1, 0);
}

/// A series DWG: empty model space, one block per article that nothing
/// inserts; the articles share an inserted PART.
fn series_doc() -> Doc {
    let mut doc = Doc::new(4);
    let part = doc.block("PART", [0.0; 3]);
    doc.put(part, "3DFACE", square_face());
    let a = doc.block("Chair A", [0.0; 3]);
    doc.put(a, "3DFACE", square_face());
    doc.put(a, "INSERT", insert(part, 2000.0));
    // the same file name as "Chair A" on case-insensitive file systems
    let b = doc.block("chair a", [0.0; 3]);
    doc.put(b, "3DFACE", square_face());
    doc.block("EMPTY", [0.0; 3]);
    let anon = doc.block("*U1", [0.0; 3]);
    doc.put(anon, "3DFACE", square_face());
    doc
}

fn block_names(doc: &Document, blocks: &[usize]) -> Vec<String> {
    blocks
        .iter()
        .map(|&i| match &doc.objects[i].data {
            Data::BlockHeader(b) => b.name.clone(),
            _ => unreachable!(),
        })
        .collect()
}

#[test]
fn series_blocks_are_the_uninserted_named_ones() {
    let doc = Document::new(series_doc().drawing, None);
    assert_eq!(
        block_names(&doc, &doc.series_blocks()),
        ["Chair A", "chair a"]
    );
}

#[test]
fn empty_model_space_points_to_series_mode() {
    let (_, stats) = series_doc().run(Options::default());
    assert_eq!(stats.triangles, 0);
    assert!(
        stats.warnings.iter().any(|w| w.contains("--seriesdwg")),
        "{:?}",
        stats.warnings
    );
    // a drawing with content in model space is not a series DWG
    let mut doc = series_doc();
    let model = doc.model;
    doc.put(model, "3DFACE", square_face());
    let (_, stats) = doc.run(Options::default());
    assert!(!stats.warnings.iter().any(|w| w.contains("--seriesdwg")));
}

#[test]
fn series_writes_one_glb_per_block() {
    let doc = Document::new(series_doc().drawing, None);
    let dir = tempdir().join("series");
    let stats = dwg2gltf::write_series(&doc, &dir, "glb", &Options::default()).unwrap();
    let files: Vec<_> = stats.written.iter().map(|(_, p)| p.clone()).collect();
    assert_eq!(files, [dir.join("Chair A.glb"), dir.join("chair a~2.glb")]);
    assert_eq!(stats.stats.triangles, 6); // A: own face + PART, B: own face
    assert_eq!(read_back(&files[0]).faces, 4);
    assert_eq!(read_back(&files[1]).faces, 2);
    assert!(stats.to_string().contains("blocks: 2 written"), "{stats}");
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn series_mode_on_a_plain_drawing() {
    // nothing but model space: an error that names the right mode
    let mut doc = Doc::new(4);
    let model = doc.model;
    doc.put(model, "3DFACE", square_face());
    let doc = Document::new(doc.drawing, None);
    let dir = tempdir().join("plain");
    let err = dwg2gltf::write_series(&doc, &dir, "glb", &Options::default()).unwrap_err();
    assert!(matches!(err, Error::NoSeriesBlocks), "{err}");
    assert!(err.to_string().contains("without --seriesdwg"), "{err}");
    // blocks plus model space content: converted, with a warning
    let mut doc = series_doc();
    let model = doc.model;
    doc.put(model, "3DFACE", square_face());
    let doc = Document::new(doc.drawing, None);
    let stats = dwg2gltf::write_series(&doc, &dir, "glb", &Options::default()).unwrap();
    assert!(stats.to_string().contains("model space"), "{stats}");
    let _ = std::fs::remove_dir_all(dir);
}

#[test]
fn series_block_base_point_is_the_origin() {
    let mut doc = Doc::new(4); // mm
    let blk = doc.block("SHIFTED", [500.0, 0.0, 0.0]);
    doc.put(blk, "3DFACE", square_face());
    let doc = Document::new(doc.drawing, None);
    let dir = tempdir().join("base");
    let stats = dwg2gltf::write_series(&doc, &dir, "glb", &Options::default()).unwrap();
    let (lo, hi) = bounds(&read_back(&stats.written[0].1).points);
    assert!(
        (lo[0] + 0.5).abs() < 1e-6 && (hi[0] - 0.5).abs() < 1e-6,
        "{lo:?} {hi:?}"
    );
    std::fs::remove_dir_all(dir).unwrap();
}

/// Signed volume of triangles about their mean corner: positive when an
/// (open or closed) shell faces away from its centre.
fn volume_about_centre(tris: &[Corners]) -> f64 {
    use dwg2gltf::geom::{cross, dot, sub};
    let n = (tris.len() * 3) as f64;
    let c = tris
        .iter()
        .flat_map(|(t, _)| t.iter())
        .fold([0.0; 3], |a, p| {
            [a[0] + p[0] / n, a[1] + p[1] / n, a[2] + p[2] / n]
        });
    tris.iter()
        .map(|(t, _)| {
            let [a, b, d] = t.map(|p| sub(p, c));
            dot(a, cross(b, d)) / 6.0
        })
        .sum()
}

#[test]
fn open_box_of_faces_faces_outwards() {
    // a cube without its bottom, every 3DFACE wound towards the inside
    let outward: [[[f64; 3]; 4]; 5] = [
        [[0., 0., 1.], [1., 0., 1.], [1., 1., 1.], [0., 1., 1.]],
        [[0., 0., 0.], [0., 0., 1.], [0., 1., 1.], [0., 1., 0.]],
        [[1., 0., 0.], [1., 1., 0.], [1., 1., 1.], [1., 0., 1.]],
        [[0., 0., 0.], [1., 0., 0.], [1., 0., 1.], [0., 0., 1.]],
        [[0., 1., 0.], [0., 1., 1.], [1., 1., 1.], [1., 1., 0.]],
    ];
    let mut doc = Doc::new(4);
    let model = doc.model;
    for f in outward {
        let mut c = f.map(|p| p.map(|v| v * 1000.0));
        c.reverse();
        doc.put(model, "3DFACE", Shape::Face3d(c));
    }
    let (path, _) = glb_of(doc, Options::default());
    let tris = world_triangles(&path);
    assert_eq!(tris.len(), 10);
    assert!(volume_about_centre(&tris) > 0.0, "inside out");
    assert_eq!(orientation_faults(&path).0, 0);
}

#[test]
fn flat_sheet_keeps_its_winding() {
    // one face wound downwards: nothing encloses, so the author's side stays
    let mut doc = Doc::new(4);
    let model = doc.model;
    let mut c = match square_face() {
        Shape::Face3d(c) => c,
        _ => unreachable!(),
    };
    c.reverse();
    doc.put(model, "3DFACE", Shape::Face3d(c));
    let (path, _) = glb_of(doc, Options::default());
    for (_, n) in world_triangles(&path) {
        assert!(n.iter().all(|n| n[1] < -0.99), "{n:?}"); // DWG -Z is glTF -Y
    }
}

/// A 1 x 1 m top face in a block, inserted squeezed to 0.6 m in X, with a
/// box-mapped OFML fabric; `geoscale`: the EDWG_OFML_MAPPING_INFO text
/// on the INSERT.
fn squeezed_fabric_doc(mapping_info: Option<&str>) -> Doc {
    let mut doc = Doc::new(6);
    let mut zip = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let opts = zip::write::SimpleFileOptions::default();
    zip.start_file("material.mat", opts).unwrap();
    zip.write_all(b"dif 0.5 0.5 0.5\ntex image png fabric\nauto\n")
        .unwrap();
    zip.start_file("fabric.png", opts).unwrap();
    zip.write_all(&noise_png()).unwrap();
    let archive = zip.finish().unwrap().into_inner();
    let rec = doc.add(
        "XRECORD",
        Data::XRecord(vec![(310, XValue::Binary(archive))]),
    );
    let matdict = doc.add("DICTIONARY", Data::Dictionary(vec![("fabric".into(), rec)]));
    doc.add(
        "DICTIONARY",
        Data::Dictionary(vec![("EDWG_MATERIAL_DICT".into(), matdict)]),
    );
    let mat = doc.add(
        "MATERIAL",
        Data::Material(Material {
            name: "fabric".into(),
            diffuse: None,
            opacity: 1.0,
            transmatrix: None,
            filename: String::new(),
        }),
    );
    let block = doc.block("CUSHION", [0.0; 3]);
    let face = doc.put(
        block,
        "3DFACE",
        Shape::Face3d([
            [0.0, 0.0, 0.0],
            [1.0, 0.0, 0.0],
            [1.0, 1.0, 0.0],
            [0.0, 1.0, 0.0],
        ]),
    );
    doc.entity_mut(face).material_refs = [mat, 0];
    doc.entity_mut(face).material_flags = 3;
    let model = doc.model;
    let ins = doc.put(model, "INSERT", insert(block, 0.0));
    if let Shape::Insert { scale, .. } = &mut doc.entity_mut(ins).shape {
        *scale = [0.6, 1.0, 1.0];
    }
    if let Some(text) = mapping_info {
        let info = doc.add(
            "XRECORD",
            Data::XRecord(vec![(300, XValue::Text(text.into()))]),
        );
        let xdic = doc.add(
            "DICTIONARY",
            Data::Dictionary(vec![("EDWG_OFML_MAPPING_INFO".into(), info)]),
        );
        doc.get(ins).xdic = xdic;
    }
    doc
}

/// OFML maps fabric on the scaled geometry (seat cushions inserted at
/// 0.6 x 1 x 1 keep the same weave as the 1.2 m wide body), so a squeezed
/// instance must not squeeze its texture.
#[test]
fn ofml_geoscale_keeps_texture_size() {
    let span_at = |info, sx: f64| {
        let mut doc = squeezed_fabric_doc(info);
        for o in &mut doc.drawing.objects {
            if let Data::Entity(e) = &mut o.data {
                if let Shape::Insert { scale, .. } = &mut e.shape {
                    scale[0] = sx;
                }
            }
        }
        let (path, _) = glb_of(doc, Options::default());
        let r = read_back(&path);
        let (lo, hi) = bounds(
            &r.uvs
                .iter()
                .map(|uv| [uv[0], uv[1], 0.0])
                .collect::<Vec<_>>(),
        );
        [hi[0] - lo[0], hi[1] - lo[1]]
    };
    let span = |info| span_at(info, 0.6);
    // without the record, a shrunk block keeps its block coordinates, an
    // enlarged one is mapped on its scaled geometry (blocks in meters
    // inserted x100 into a centimeter drawing)
    let plain = span(None);
    assert!(
        (plain[0] - 1.0).abs() < 1e-6 && (plain[1] - 1.0).abs() < 1e-6,
        "{plain:?}"
    );
    let enlarged = span_at(None, 2.0);
    assert!(
        (enlarged[0] - 2.0).abs() < 1e-6 && (enlarged[1] - 1.0).abs() < 1e-6,
        "{enlarged:?}"
    );
    // with it: 0.6 m of fabric across, 1 m deep
    let scaled = span(Some("geoscale 0.6 1.0 1.0"));
    assert!(
        (scaled[0] - 0.6).abs() < 1e-6 && (scaled[1] - 1.0).abs() < 1e-6,
        "{scaled:?}"
    );
}

/// The INSERT scale only goes into the texture projection: in a drawing
/// without textures, instances at other scales keep sharing one mesh.
#[test]
fn insert_scale_splits_meshes_only_for_textures() {
    let meshes = |textured: bool| {
        let mut doc = squeezed_fabric_doc(None);
        let block = doc
            .drawing
            .objects
            .iter()
            .find(|o| matches!(&o.data, Data::BlockHeader(b) if b.name == "CUSHION"))
            .unwrap()
            .handle;
        if !textured {
            doc.drawing
                .objects
                .retain(|o| !matches!(o.data, Data::Material(_)));
            for o in &mut doc.drawing.objects {
                if let Data::Entity(e) = &mut o.data {
                    e.material_refs = [0, 0];
                    e.material_flags = 0;
                }
            }
        }
        let model = doc.model;
        let big = doc.put(model, "INSERT", insert(block, 5.0));
        if let Shape::Insert { scale, .. } = &mut doc.entity_mut(big).shape {
            *scale = [2.0; 3];
        }
        let (gl, _) = doc.run(Options::default());
        gl["meshes"].as_array().unwrap().len()
    };
    assert_eq!(meshes(true), 2);
    assert_eq!(meshes(false), 1);
}

#[test]
fn ofml_geoscale_text() {
    use dwg2gltf::ofml::geoscale;
    assert_eq!(geoscale("geoscale 1.2 1.0 1.0"), Some([1.2, 1.0, 1.0]));
    assert_eq!(geoscale("geoscale -1.0 1.0 1.0"), Some([-1.0, 1.0, 1.0]));
    assert_eq!(geoscale("auto xyx xz\nrotate 0 0 0\n"), None);
    assert_eq!(geoscale("geoscale 0 1 1"), None);
    assert_eq!(geoscale("geoscale 1 1"), None);
}

// -- drawings too small for their units --------------------------------------

/// A square face `size` units wide with its corner at `at`.
fn face_at(size: f64, at: [f64; 3]) -> Shape {
    let p = |x: f64, y: f64| [at[0] + x, at[1] + y, at[2]];
    Shape::Face3d([p(0.0, 0.0), p(size, 0.0), p(size, size), p(0.0, size)])
}

/// A drawing in `insunits` with one square face `size` units wide.
fn face_doc(insunits: i32, size: f64) -> Doc {
    let mut doc = Doc::new(insunits);
    let model = doc.model;
    doc.put(model, "3DFACE", face_at(size, [0.0; 3]));
    doc
}

/// The scale on the root node's matrix (the drawing units in meters).
fn root_scale(gl: &Map<String, Value>) -> f64 {
    let root = &gl["nodes"][gl["scenes"][0]["nodes"][0].as_u64().unwrap() as usize];
    root["matrix"][0].as_f64().unwrap()
}

fn tiny_warnings(stats: &Stats) -> Vec<&str> {
    stats
        .warnings
        .iter()
        .map(String::as_str)
        .filter(|w| w.contains("less than 1 cm across"))
        .collect()
}

fn auto() -> Options {
    Options {
        auto_units: true,
        ..Options::default()
    }
}

fn glb(doc: Doc, options: &Options) -> (Vec<u8>, Stats) {
    let document = Document::new(doc.drawing, None);
    let (gltf, stats) = dwg2gltf::convert_document(&document, options).unwrap();
    (gltf.to_glb().unwrap(), stats)
}

/// OFML geometry in meters that says millimeters comes out
/// 1000x too small: a warning, and `auto_units` converts it in meters, as
/// `units: m` does.
#[test]
fn tiny_mm_drawing_warns_and_auto_takes_meters() {
    let (gl, stats) = face_doc(4, 2.0).run(Options::default());
    assert_eq!(root_scale(&gl), 0.001);
    assert_eq!(
        tiny_warnings(&stats),
        [
            "the drawing is less than 1 cm across in millimeters (INSUNITS 4); if its \
             coordinates are meters (as in many OFML drawings), use --units m or --units auto"
        ]
    );

    let (gl, stats) = face_doc(4, 2.0).run(auto());
    assert_eq!(root_scale(&gl), 1.0);
    assert_eq!(
        tiny_warnings(&stats),
        [
            "the drawing is less than 1 cm across in millimeters (INSUNITS 4); its \
             coordinates are taken as meters (--units auto)"
        ]
    );

    let meters = Options {
        units: Some(6),
        ..Options::default()
    };
    let (auto_glb, auto_stats) = glb(face_doc(4, 2.0), &auto());
    let (m_glb, m_stats) = glb(face_doc(4, 2.0), &meters);
    assert!(auto_glb == m_glb);
    assert_eq!(auto_stats.triangles, m_stats.triangles);
    assert!(tiny_warnings(&m_stats).is_empty());
}

/// A drawing 1 cm or more across keeps its units, with or without
/// `auto_units`, and the output does not change.
#[test]
fn plausible_drawings_keep_their_units() {
    let (gl, stats) = face_doc(4, 100.0).run(Options::default());
    assert_eq!(root_scale(&gl), 0.001);
    assert!(tiny_warnings(&stats).is_empty());
    let (plain, plain_stats) = glb(face_doc(4, 100.0), &Options::default());
    let (auto_glb, auto_stats) = glb(face_doc(4, 100.0), &auto());
    assert!(plain == auto_glb);
    assert_eq!(plain_stats.to_string(), auto_stats.to_string());
}

#[test]
fn tiny_threshold_is_one_centimeter() {
    let (_, stats) = face_doc(4, 9.9).run(Options::default());
    assert_eq!(tiny_warnings(&stats).len(), 1);
    let (gl, stats) = face_doc(4, 9.9).run(auto());
    assert_eq!(root_scale(&gl), 1.0);
    assert_eq!(tiny_warnings(&stats).len(), 1);
    let (_, stats) = face_doc(4, 10.1).run(Options::default());
    assert!(tiny_warnings(&stats).is_empty());
    let (gl, stats) = face_doc(4, 10.1).run(auto());
    assert_eq!(root_scale(&gl), 0.001);
    assert!(tiny_warnings(&stats).is_empty());
}

/// Only units smaller than a meter that drawings are made in, and only the
/// file's own units, are second-guessed.
#[test]
fn which_units_are_second_guessed() {
    for (insunits, size, unit) in [
        (1, 0.3, Some("inches (INSUNITS 1)")),
        (2, 0.03, Some("feet (INSUNITS 2)")),
        (5, 0.5, Some("centimeters (INSUNITS 5)")),
        (10, 0.01, Some("yards (INSUNITS 10)")),
        (14, 0.05, Some("decimeters (INSUNITS 14)")),
        (6, 0.002, None),    // meters
        (7, 0.000002, None), // kilometers
        (13, 2.0, None),     // microns: not a unit furniture is drawn in
        (0, 0.002, None),    // no units: taken as meters, with its own warning
    ] {
        let (_, stats) = face_doc(insunits, size).run(Options::default());
        let tiny = tiny_warnings(&stats);
        match unit {
            Some(unit) => {
                assert_eq!(tiny.len(), 1, "INSUNITS {insunits}");
                assert!(tiny[0].contains(unit), "{}", tiny[0]);
            }
            None => assert!(tiny.is_empty(), "INSUNITS {insunits}: {tiny:?}"),
        }
        let (plain, plain_stats) = glb(face_doc(insunits, size), &Options::default());
        let (auto_glb, auto_stats) = glb(face_doc(insunits, size), &auto());
        assert_eq!(plain == auto_glb, unit.is_none(), "INSUNITS {insunits}");
        assert_eq!(plain_stats.triangles, auto_stats.triangles);
    }
    let (_, stats) = face_doc(0, 0.002).run(Options::default());
    assert_eq!(stats.warnings.len(), 1);
    assert!(stats.warnings[0].contains("INSUNITS 0"));

    // given units are never second-guessed
    for units in [Some(4), Some(6)] {
        let options = Options {
            units,
            auto_units: true,
            ..Options::default()
        };
        let (gl, stats) = face_doc(4, 2.0).run(options);
        assert_eq!(root_scale(&gl), if units == Some(4) { 0.001 } else { 1.0 });
        assert!(stats.warnings.is_empty(), "{:?}", stats.warnings);
    }
}

/// The size is measured on everything drawn, through the node matrices.
#[test]
fn tiny_size_covers_lines_offsets_and_inserts() {
    // lines only
    let mut doc = Doc::new(4);
    let model = doc.model;
    let line = Shape::Line {
        start: [0.0; 3],
        end: [2.0, 1.0, 0.0],
    };
    doc.put(model, "LINE", line);
    let (gl, stats) = doc.run(auto());
    assert_eq!(root_scale(&gl), 1.0);
    assert_eq!(tiny_warnings(&stats).len(), 1);

    // far from the origin, with a block instance below the offset mesh
    let mut doc = Doc::new(4);
    let model = doc.model;
    let part = doc.block("PART", [0.0; 3]);
    doc.put(part, "3DFACE", face_at(2.0, [0.0; 3]));
    doc.put(model, "3DFACE", face_at(2.0, [1e6, 0.0, 0.0]));
    doc.put(model, "INSERT", insert(part, 1e6 + 3.0));
    let (_, stats) = doc.run(Options::default());
    assert_eq!(tiny_warnings(&stats).len(), 1);
    // ... but not when the parts are 1 cm apart
    let mut doc = Doc::new(4);
    let model = doc.model;
    let part = doc.block("PART", [0.0; 3]);
    doc.put(part, "3DFACE", face_at(2.0, [0.0; 3]));
    doc.put(model, "3DFACE", face_at(2.0, [1e6, 0.0, 0.0]));
    doc.put(model, "INSERT", insert(part, 1e6 + 12.0));
    let (_, stats) = doc.run(Options::default());
    assert!(tiny_warnings(&stats).is_empty());

    // a block inserted at scale 1000 makes the drawing 2 m across
    let mut doc = Doc::new(4);
    let model = doc.model;
    let part = doc.block("PART", [0.0; 3]);
    doc.put(part, "3DFACE", face_at(2.0, [0.0; 3]));
    let h = doc.put(model, "INSERT", insert(part, 0.0));
    if let Shape::Insert { scale, .. } = &mut doc.entity_mut(h).shape {
        *scale = [1000.0; 3];
    }
    let (gl, stats) = doc.run(auto());
    assert_eq!(root_scale(&gl), 0.001);
    assert!(tiny_warnings(&stats).is_empty());
}

/// The size is the same with compression and in DWG axes.
#[test]
fn tiny_warning_with_compress_and_z_up() {
    for options in [
        Options {
            compress: true,
            ..Options::default()
        },
        Options {
            y_up: false,
            ..Options::default()
        },
    ] {
        let (_, stats) = glb(face_doc(4, 2.0), &options);
        assert_eq!(tiny_warnings(&stats).len(), 1);
        let (_, stats) = glb(face_doc(4, 20.0), &options);
        assert!(tiny_warnings(&stats).is_empty());
    }
    let compress_auto = Options {
        compress: true,
        auto_units: true,
        ..Options::default()
    };
    let compress_m = Options {
        compress: true,
        units: Some(6),
        ..Options::default()
    };
    assert!(glb(face_doc(4, 2.0), &compress_auto).0 == glb(face_doc(4, 2.0), &compress_m).0);
}

#[test]
fn empty_drawing_is_not_tiny() {
    let (_, stats) = Doc::new(4).run(auto());
    assert!(stats.warnings.is_empty(), "{:?}", stats.warnings);
}

/// The blocks of a series DWG share the file's units: they are judged
/// together, by the largest.
#[test]
fn series_are_judged_by_the_largest_block() {
    let library = |sizes: &[f64]| {
        let mut doc = Doc::new(4);
        for (i, &size) in sizes.iter().enumerate() {
            let b = doc.block(&format!("ART{i}"), [0.0; 3]);
            doc.put(b, "3DFACE", face_at(size, [0.0; 3]));
        }
        Document::new(doc.drawing, None)
    };
    let run = |doc: &Document, options: &Options| {
        let mut scales = vec![];
        let series = dwg2gltf::series(doc, &doc.series_blocks(), options, |_, gltf| {
            scales.push(root_scale(gltf.document()));
            Ok(())
        })
        .unwrap();
        (scales, series.stats)
    };

    let doc = library(&[2.0, 3.0]);
    let (scales, stats) = run(&doc, &Options::default());
    assert_eq!(scales, [0.001, 0.001]);
    assert_eq!(tiny_warnings(&stats).len(), 1);
    assert!(tiny_warnings(&stats)[0].contains("use --units m"));
    let (scales, stats) = run(&doc, &auto());
    assert_eq!(scales, [1.0, 1.0]);
    assert_eq!(tiny_warnings(&stats).len(), 1);
    assert!(tiny_warnings(&stats)[0].contains("taken as meters"));

    // a 2 mm screw in a library of mm parts stays in mm
    let doc = library(&[2.0, 500.0]);
    for options in [Options::default(), auto()] {
        let (scales, stats) = run(&doc, &options);
        assert_eq!(scales, [0.001, 0.001]);
        assert!(tiny_warnings(&stats).is_empty());
    }

    // ... also when only the screw is asked for (the loader's `only`)
    let screw = doc.series_blocks()[..1].to_vec();
    for options in [Options::default(), auto()] {
        let mut scales = vec![];
        let series = dwg2gltf::series(&doc, &screw, &options, |_, gltf| {
            scales.push(root_scale(gltf.document()));
            Ok(())
        })
        .unwrap();
        assert_eq!(scales, [0.001]);
        assert!(tiny_warnings(&series.stats).is_empty());
    }
    // ... and a tiny library is tiny when one of its articles is asked for
    let doc = library(&[2.0, 3.0]);
    let first = doc.series_blocks()[..1].to_vec();
    let series = dwg2gltf::series(&doc, &first, &auto(), |_, gltf| {
        assert_eq!(root_scale(gltf.document()), 1.0);
        Ok(())
    })
    .unwrap();
    assert_eq!(tiny_warnings(&series.stats).len(), 1);

    // an article with nothing to show has no size: it neither decides nor
    // ends the judgement
    let mut lib = Doc::new(4);
    let off = lib.add(
        "LAYER",
        Data::Layer(Layer {
            off: true,
            ..layer("OFF", 0xC3000001)
        }),
    );
    let hidden = lib.block("AAA_HIDDEN", [0.0; 3]);
    lib.put_entity(hidden, "3DFACE", entity(off, face_at(500.0, [0.0; 3])));
    for (i, size) in [2.0, 3.0].into_iter().enumerate() {
        let b = lib.block(&format!("ART{i}"), [0.0; 3]);
        lib.put(b, "3DFACE", face_at(size, [0.0; 3]));
    }
    let doc = Document::new(lib.drawing, None);
    assert_eq!(block_names(&doc, &doc.series_blocks())[0], "AAA_HIDDEN");
    for (options, scale) in [(Options::default(), 0.001), (auto(), 1.0)] {
        let mut scales = vec![];
        let series = dwg2gltf::series(&doc, &doc.series_blocks(), &options, |_, gltf| {
            scales.push(root_scale(gltf.document()));
            Ok(())
        })
        .unwrap();
        assert_eq!(series.empty, ["AAA_HIDDEN"]);
        assert_eq!(scales, [scale, scale]);
        assert_eq!(tiny_warnings(&series.stats).len(), 1);
        // asked for alone, the hidden article is judged by the others
        let alone = Options {
            allow_empty: true,
            ..options
        };
        let series =
            dwg2gltf::series(&doc, &doc.series_blocks()[..1], &alone, |_, _| Ok(())).unwrap();
        assert_eq!(tiny_warnings(&series.stats).len(), 1);
    }
}

// -- in-memory API (the three.js loader's WebAssembly build) ---------------

#[test]
fn convert_bytes_names_the_input_in_errors() {
    let data = b"AC1032 is not a drawing".to_vec();
    let err = dwg2gltf::convert_bytes(data, "junk.dwg", &Options::default()).unwrap_err();
    assert!(matches!(err, Error::Read(_)), "{err}");
    assert!(err.to_string().contains("junk.dwg"), "{err}");
}

/// `series` hands over what `write_series` writes, block by block.
#[test]
fn series_streams_what_write_series_writes() {
    let doc = Document::new(series_doc().drawing, None);
    let dir = tempdir().join("series-mem");
    let files = dwg2gltf::write_series(&doc, &dir, "glb", &Options::default()).unwrap();
    let mut got = vec![];
    let series = dwg2gltf::series(
        &doc,
        &doc.series_blocks(),
        &Options::default(),
        |name, gltf| {
            got.push((name.to_string(), gltf.to_glb()?));
            Ok(())
        },
    )
    .unwrap();
    let names: Vec<&str> = got.iter().map(|(n, _)| n.as_str()).collect();
    assert_eq!(names, ["Chair A", "chair a"]);
    assert_eq!(series.written, ["Chair A", "chair a"]);
    for ((_, glb), (_, path)) in got.iter().zip(&files.written) {
        assert!(*glb == std::fs::read(path).unwrap(), "{}", path.display());
    }
    assert_eq!(series.to_string(), files.to_string());
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn is_series() {
    assert!(Document::new(series_doc().drawing, None).is_series());
    let mut doc = series_doc();
    let model = doc.model;
    doc.put(model, "3DFACE", square_face());
    assert!(!Document::new(doc.drawing, None).is_series());
    assert!(!Document::new(Doc::new(4).drawing, None).is_series());
}

/// Two faces with a DWG MATERIAL that names an image file.
fn file_textured_doc(filename: &str) -> Doc {
    let mut doc = Doc::new(6);
    let mat = doc.add(
        "MATERIAL",
        Data::Material(Material {
            name: "wood".into(),
            diffuse: None,
            opacity: 1.0,
            transmatrix: None,
            filename: filename.into(),
        }),
    );
    let model = doc.model;
    for _ in 0..2 {
        let face = doc.put(model, "3DFACE", square_face());
        doc.entity_mut(face).material_refs = [mat, 0];
        doc.entity_mut(face).material_flags = 3;
    }
    doc
}

fn images(doc: &Document) -> Vec<String> {
    let (gltf, _) = Converter::new(doc, Options::default()).run().unwrap();
    gltf.document()
        .get("images")
        .and_then(Value::as_array)
        .map(|a| {
            a.iter()
                .map(|i| i["mimeType"].as_str().unwrap().to_string())
                .collect()
        })
        .unwrap_or_default()
}

/// The image file of a material comes from the document's lookup, asked
/// once per material with the stored file name, and only for PNG/JPEG.
#[test]
fn material_image_files_come_from_the_lookup() {
    let calls = Rc::new(Cell::new(0));
    let lookup = |calls: &Rc<Cell<i32>>| -> dwg2gltf::model::TextureFiles {
        let calls = calls.clone();
        Box::new(move |name: &str| {
            calls.set(calls.get() + 1);
            assert_eq!(name, r"C:\textures\Wood.PNG");
            Some(noise_png())
        })
    };
    let doc = Document::with_texture_files(
        file_textured_doc(r"C:\textures\Wood.PNG").drawing,
        lookup(&calls),
    );
    assert_eq!(images(&doc), ["image/png"]);
    assert_eq!(calls.get(), 1);

    let calls = Rc::new(Cell::new(0));
    let doc = Document::with_texture_files(
        file_textured_doc(r"C:\textures\Wood.tif").drawing,
        lookup(&calls),
    );
    assert!(images(&doc).is_empty());
    assert_eq!(calls.get(), 0);
}

/// `Document::new` finds a material's image next to the drawing.
#[test]
fn material_image_next_to_the_drawing() {
    let dir = tempdir().join("textures");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("Wood.png"), noise_png()).unwrap();
    let drawing = file_textured_doc(r"C:\elsewhere\Wood.png").drawing;
    let doc = Document::new(drawing, Some(&dir.join("chair.dwg")));
    assert_eq!(images(&doc), ["image/png"]);
    std::fs::remove_dir_all(dir).unwrap();
}

/// An AutoCAD material mapper (XDATA ACAD_MATERIAL_MAPPER): origin at 0,
/// axes of length `len` along X, Y, Z.
fn mapper(projection: i16, len: f64) -> dwg2gltf::mapper::MaterialMapper {
    dwg2gltf::mapper::MaterialMapper {
        projection,
        tiling: 1,
        auto_transform: 1,
        origin: [0.0; 3],
        axes_ends: [[len, 0.0, 0.0], [0.0, len, 0.0], [0.0, 0.0, len]],
    }
}

/// The span of the texture coordinates (u, v) of a drawing.
fn uv_span(doc: Doc) -> ([f64; 2], Stats) {
    let (path, stats) = glb_of(doc, Options::default());
    let r = read_back(&path);
    let pts: Vec<[f64; 3]> = r.uvs.iter().map(|uv| [uv[0], uv[1], 0.0]).collect();
    let (lo, hi) = bounds(&pts);
    ([hi[0] - lo[0], hi[1] - lo[1]], stats)
}

fn handle_of(doc: &Doc, kind: &str) -> Handle {
    doc.drawing
        .objects
        .iter()
        .find(|o| o.kind == kind)
        .unwrap()
        .handle
}

/// `squeezed_fabric_doc` with the material on the INSERT and the face
/// ByBlock: the face's material, and its mapper, come from the INSERT.
fn byblock_fabric_doc() -> Doc {
    let mut doc = squeezed_fabric_doc(None);
    let (ins, face) = (handle_of(&doc, "INSERT"), handle_of(&doc, "3DFACE"));
    let refs = doc.entity_mut(face).material_refs;
    doc.entity_mut(face).material_refs = [0, 0];
    doc.entity_mut(face).material_flags = 1;
    doc.entity_mut(ins).material_refs = refs;
    doc.entity_mut(ins).material_flags = 3;
    doc
}

fn near(a: [f64; 2], b: [f64; 2]) -> bool {
    (a[0] - b[0]).abs() < 1e-6 && (a[1] - b[1]).abs() < 1e-6
}

/// Projected textures follow AutoCAD's material mapper, which goes with
/// the material: the face's own, or with a ByBlock material the mapper of
/// the INSERT the material comes from; in the frame's units (no meters, no
/// INSERT scale): a rug mapped this way spans its texture once.
#[test]
fn material_mapper_sets_the_projection() {
    // the 1 x 1 face in a block inserted at 0.6 x 1 x 1: without a mapper
    // it keeps its block coordinates in meters
    let (span, _) = uv_span(squeezed_fabric_doc(None));
    assert!(near(span, [1.0, 1.0]), "{span:?}");
    // the face's own mapper (axes 0.25) with its own material
    let mut doc = squeezed_fabric_doc(None);
    let (ins, face) = (handle_of(&doc, "INSERT"), handle_of(&doc, "3DFACE"));
    doc.entity_mut(face).material_mapper = Some(mapper(1, 0.25));
    doc.entity_mut(ins).material_mapper = Some(mapper(2, 0.5));
    let (span, _) = uv_span(doc);
    assert!(near(span, [4.0, 4.0]), "{span:?}");
    // ByBlock: the mapper of the INSERT (axes 0.5) that gives the
    // material, over the face's own
    let mut doc = byblock_fabric_doc();
    let (ins, face) = (handle_of(&doc, "INSERT"), handle_of(&doc, "3DFACE"));
    doc.entity_mut(ins).material_mapper = Some(mapper(2, 0.5));
    doc.entity_mut(face).material_mapper = Some(mapper(1, 0.25));
    let (span, _) = uv_span(doc);
    assert!(near(span, [2.0, 2.0]), "{span:?}");
    // also an identity mapper on the INSERT
    let mut doc = byblock_fabric_doc();
    let (ins, face) = (handle_of(&doc, "INSERT"), handle_of(&doc, "3DFACE"));
    doc.entity_mut(ins).material_mapper = Some(mapper(2, 1.0));
    doc.entity_mut(face).material_mapper = Some(mapper(1, 0.25));
    let (span, _) = uv_span(doc);
    assert!(near(span, [1.0, 1.0]), "{span:?}");
    // an INSERT without a mapper leaves the face's own
    let mut doc = byblock_fabric_doc();
    let face = handle_of(&doc, "3DFACE");
    doc.entity_mut(face).material_mapper = Some(mapper(1, 0.25));
    let (span, _) = uv_span(doc);
    assert!(near(span, [4.0, 4.0]), "{span:?}");
    // an identity mapper alone is the face's own coordinates, one repeat
    // per unit (not per meter: the drawing is in millimeters)
    let mut doc = squeezed_fabric_doc(None);
    let face = handle_of(&doc, "3DFACE");
    if let Shape::Face3d(c) = &mut doc.entity_mut(face).shape {
        *c = c.map(|p| p.map(|x| x * 3.0));
    }
    doc.entity_mut(face).material_mapper = Some(mapper(1, 1.0));
    let (span, _) = uv_span(doc);
    assert!(near(span, [3.0, 3.0]), "{span:?}");
}

/// The face's span with the material, and so the mapper, on the outer
/// INSERT (`material_on_inner` false) or the inner one: outer INSERT
/// (mapper axes 0.5) -> inner INSERT x0.001 (mapper axes 0.125) -> ByBlock
/// face with an identity mapper.
fn nested_mapper_span(material_on_inner: bool) -> [f64; 2] {
    let mut doc = byblock_fabric_doc();
    let cushion = doc
        .drawing
        .objects
        .iter()
        .find(|o| matches!(&o.data, Data::BlockHeader(b) if b.name == "CUSHION"))
        .unwrap()
        .handle;
    let ins = handle_of(&doc, "INSERT");
    let outer = doc.block("OUTER", [0.0; 3]);
    let inner = doc.put(outer, "INSERT", insert(cushion, 0.0));
    if let Shape::Insert { scale, .. } = &mut doc.entity_mut(inner).shape {
        *scale = [0.001; 3];
    }
    if let Shape::Insert { block, scale, .. } = &mut doc.entity_mut(ins).shape {
        *block = outer;
        *scale = [1.0; 3];
    }
    doc.entity_mut(ins).material_mapper = Some(mapper(2, 0.5));
    doc.entity_mut(inner).material_mapper = Some(mapper(2, 0.125));
    doc.entity_mut(inner).material_flags = 1;
    if material_on_inner {
        let refs = doc.entity_mut(ins).material_refs;
        doc.entity_mut(inner).material_refs = refs;
        doc.entity_mut(inner).material_flags = 3;
        doc.entity_mut(ins).material_refs = [0, 0];
        doc.entity_mut(ins).material_flags = 0;
    }
    let face = handle_of(&doc, "3DFACE");
    doc.entity_mut(face).material_mapper = Some(mapper(1, 1.0));
    uv_span(doc).0
}

/// An INSERT's mapper applies to the coordinates of the block the faces
/// are in, also below further INSERTs, without their transforms (1000 mm
/// axes on an INSERT in a meter block whose faces are in millimeter blocks
/// inserted x0.001 show one repeat per meter). The INSERT that gives the
/// material decides (an outer INSERT without a material with a 1 inch
/// mapper, the inner ones with the material with 1 m mappers: the latter
/// count).
#[test]
fn material_mapper_is_not_carried_through_nested_inserts() {
    let span = nested_mapper_span(false);
    assert!(near(span, [2.0, 2.0]), "{span:?}");
    let span = nested_mapper_span(true);
    assert!(near(span, [8.0, 8.0]), "{span:?}");
}

/// Cylindrical and spherical mappers are not followed: the faces are
/// projected as without a mapper, and that is reported.
#[test]
fn cylindrical_mapper_is_reported() {
    let mut doc = byblock_fabric_doc();
    let ins = handle_of(&doc, "INSERT");
    doc.entity_mut(ins).material_mapper = Some(mapper(3, 0.5));
    let (span, stats) = uv_span(doc);
    assert!(near(span, [1.0, 1.0]), "{span:?}");
    assert_eq!(
        stats
            .skipped
            .get("3DFACE material mapper (cylindrical, not followed)"),
        Some(&1)
    );
    assert!(stats.warnings.iter().any(|w| w.contains("cylindrical")));
}

/// The BricsCAD test drawings (tests/bricscad): their open procedural
/// surfaces in every DWG version. Up to 2010 their ACIS data is inside the
/// entity, which LibreDWG left undecoded (EXTRUDEDSURFACE and the others are
/// debugging classes there), so they converted to nothing; 2013+ keep it in
/// the AcDs section. Checks the area and bounds against BricsCAD's
/// (expected.json).
#[test]
fn bricscad_surfaces_convert_in_every_version() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/bricscad/dwg/v27");
    let expected: Value =
        serde_json::from_slice(&std::fs::read(root.join("expected.json")).unwrap()).unwrap();
    let dir = tempdir();
    let out = dir.join("surface.glb");
    let options = Options {
        units: Some(4),
        y_up: false,
        ..Options::default()
    };
    let mut failed = vec![];
    for (shape, kind) in [
        ("surface_extruded", "EXTRUDEDSURFACE"),
        ("surface_revolved", "REVOLVEDSURFACE"),
        ("surface_lofted", "LOFTEDSURFACE"),
        ("surface_swept", "SWEPTSURFACE"),
    ] {
        for version in [
            "AC1014", "AC1015", "AC1018", "AC1021", "AC1024", "AC1027", "AC1032",
        ] {
            let rel = format!("surface/{shape}/{shape}_{version}.dwg");
            let stats = match dwg2gltf::convert(&root.join(&rel), &out, &options) {
                Ok(stats) => stats,
                Err(e) => {
                    failed.push(format!("{rel}: {e:?}"));
                    continue;
                }
            };
            if stats.converted.get(kind) != Some(&1) || !stats.skipped.is_empty() {
                failed.push(format!("{rel}: {:?} {:?}", stats.converted, stats.skipped));
                continue;
            }
            // glTF meters, Z up
            let tris: Vec<[[f64; 3]; 3]> = world_triangles(&out)
                .iter()
                .map(|(t, _)| t.map(|p| p.map(|x| x * 1000.0)))
                .collect();
            let area: f64 = tris
                .iter()
                .map(|t| {
                    use dwg2gltf::geom::{cross, length, sub};
                    length(cross(sub(t[1], t[0]), sub(t[2], t[0]))) / 2.0
                })
                .sum();
            let want_area = expected[&rel]["area"].as_f64().unwrap();
            // measure.py's AREA_SHARE
            if (area - want_area).abs() > 0.01 * want_area {
                failed.push(format!("{rel}: area {area:.1}, expected {want_area:.1}"));
            }
            let points: Vec<[f64; 3]> = tris.iter().flatten().copied().collect();
            let (lo, hi) = bounds(&points);
            let want = &expected[&rel]["bounds"];
            let size = (0..3)
                .map(|k| want[1][k].as_f64().unwrap() - want[0][k].as_f64().unwrap())
                .fold(0.0, f64::max);
            let off = (0..3)
                .flat_map(|k| {
                    [
                        (lo[k] - want[0][k].as_f64().unwrap()).abs(),
                        (hi[k] - want[1][k].as_f64().unwrap()).abs(),
                    ]
                })
                .fold(0.0, f64::max);
            if off > 0.01 * size {
                failed.push(format!("{rel}: bounds {lo:?} {hi:?}, expected {want}"));
            }
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
    assert!(failed.is_empty(), "{failed:#?}");
}
