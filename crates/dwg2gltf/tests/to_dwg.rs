// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! glTF/OBJ/STL -> DWG, checked by reading the DWG back with the converter.

use std::path::{Path, PathBuf};

use dwg2gltf::dwg::{Data, Entity};
use dwg2gltf::gltf::srgb_to_linear;
use dwg2gltf::{DwgVersion, Options, WriteOptions};
use serde_json::{Value, json};

fn tempdir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dwg2gltf-to-dwg-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn b64(data: &[u8]) -> String {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for c in data.chunks(3) {
        let n = (c[0] as u32) << 16
            | (*c.get(1).unwrap_or(&0) as u32) << 8
            | *c.get(2).unwrap_or(&0) as u32;
        for i in 0..4 {
            out.push(if i <= c.len() {
                A[(n >> (18 - 6 * i) & 63) as usize] as char
            } else {
                '='
            });
        }
    }
    out
}

/// A GLB file: its JSON and binary chunk.
struct Glb {
    json: Value,
    bin: Vec<u8>,
}

impl Glb {
    fn read(path: &Path) -> Glb {
        let b = std::fs::read(path).unwrap();
        let word = |at: usize| u32::from_le_bytes(b[at..at + 4].try_into().unwrap()) as usize;
        let jlen = word(12);
        let json = serde_json::from_slice(&b[20..20 + jlen]).unwrap();
        let bin = b.get(28 + jlen..).unwrap_or_default().to_vec();
        Glb { json, bin }
    }

    fn floats(&self, accessor: &Value) -> Vec<f32> {
        let view = &self.json["bufferViews"][accessor["bufferView"].as_u64().unwrap() as usize];
        let start = view["byteOffset"].as_u64().unwrap_or(0) as usize;
        let len = view["byteLength"].as_u64().unwrap() as usize;
        self.bin[start..start + len]
            .chunks_exact(4)
            .map(|c| f32::from_le_bytes(c.try_into().unwrap()))
            .collect()
    }

    /// Every vertex position in world coordinates, and every UV pair.
    fn world_points_and_uvs(&self) -> (Vec<[f64; 3]>, Vec<[f32; 2]>) {
        let (mut pts, mut uvs) = (vec![], vec![]);
        let nodes = self.json["nodes"].as_array().unwrap();
        let mut stack: Vec<(usize, [f64; 16])> = self.json["scenes"][0]["nodes"]
            .as_array()
            .unwrap()
            .iter()
            .map(|n| (n.as_u64().unwrap() as usize, identity()))
            .collect();
        while let Some((i, parent)) = stack.pop() {
            let node = &nodes[i];
            let local = node["matrix"].as_array().map_or_else(identity, |m| {
                let v: Vec<f64> = m.iter().map(|x| x.as_f64().unwrap()).collect();
                std::array::from_fn(|k| v[(k % 4) * 4 + k / 4]) // column-major
            });
            let world = mul(&parent, &local);
            if let Some(mesh) = node["mesh"].as_u64() {
                for p in self.json["meshes"][mesh as usize]["primitives"]
                    .as_array()
                    .unwrap()
                {
                    let acc = &self.json["accessors"];
                    let pos =
                        self.floats(&acc[p["attributes"]["POSITION"].as_u64().unwrap() as usize]);
                    for c in pos.chunks_exact(3) {
                        let v = [c[0] as f64, c[1] as f64, c[2] as f64];
                        pts.push(std::array::from_fn(|r| {
                            (0..3).map(|k| world[r * 4 + k] * v[k]).sum::<f64>() + world[r * 4 + 3]
                        }));
                    }
                    if let Some(t) = p["attributes"]["TEXCOORD_0"].as_u64() {
                        uvs.extend(
                            self.floats(&acc[t as usize])
                                .chunks_exact(2)
                                .map(|c| [c[0], c[1]]),
                        );
                    }
                }
            }
            for c in node["children"].as_array().into_iter().flatten() {
                stack.push((c.as_u64().unwrap() as usize, world));
            }
        }
        (pts, uvs)
    }
}

fn identity() -> [f64; 16] {
    std::array::from_fn(|k| if k % 5 == 0 { 1.0 } else { 0.0 })
}

fn mul(a: &[f64; 16], b: &[f64; 16]) -> [f64; 16] {
    std::array::from_fn(|k| (0..4).map(|i| a[k / 4 * 4 + i] * b[i * 4 + k % 4]).sum())
}

fn bounds(pts: &[[f64; 3]]) -> ([f64; 3], [f64; 3]) {
    let mut lo = [f64::INFINITY; 3];
    let mut hi = [f64::NEG_INFINITY; 3];
    for p in pts {
        for k in 0..3 {
            lo[k] = lo[k].min(p[k]);
            hi[k] = hi[k].max(p[k]);
        }
    }
    (lo, hi)
}

/// A textured quad used by three nodes: two plain transforms (one block,
/// two INSERTs) and one sheared (written out as a transformed copy).
fn quad_gltf(texture: &[u8]) -> String {
    let f32s = |v: &[f32]| v.iter().flat_map(|f| f.to_le_bytes()).collect::<Vec<u8>>();
    let mut bin = f32s(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0]);
    bin.extend(f32s(&[0.0, 0.0, 1.0].repeat(4)));
    bin.extend(f32s(&[0.0, 1.0, 1.0, 1.0, 1.0, 0.0, 0.0, 0.0]));
    bin.extend([0u16, 1, 2, 0, 2, 3].iter().flat_map(|i| i.to_le_bytes()));
    let h = std::f64::consts::FRAC_1_SQRT_2;
    json!({
        "asset": {"version": "2.0"},
        "scene": 0,
        "scenes": [{"nodes": [0, 1, 2]}],
        "nodes": [
            {"name": "a", "mesh": 0, "translation": [1, 0, 0]},
            {"name": "b", "mesh": 0, "translation": [3, 0, 0], "rotation": [0, h, 0, h], "scale": [2, 2, 2]},
            {"name": "c", "mesh": 0, "matrix": [1, 0, 0, 0, 0.5, 1, 0, 0, 0, 0, 1, 0, 0, 0, 5, 1]}
        ],
        "meshes": [{"name": "quad", "primitives": [{
            "attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2},
            "indices": 3, "material": 0}]}],
        "materials": [{"name": "checker",
            "pbrMetallicRoughness": {"baseColorFactor": [0.5, 0.25, 0.125, 1],
                "metallicFactor": 0.1, "roughnessFactor": 0.8, "baseColorTexture": {"index": 0}}}],
        "textures": [{"source": 0}],
        "images": [{"uri": format!("data:image/png;base64,{}", b64(texture))}],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3",
             "min": [0, 0, 0], "max": [1, 1, 0]},
            {"bufferView": 1, "componentType": 5126, "count": 4, "type": "VEC3"},
            {"bufferView": 2, "componentType": 5126, "count": 4, "type": "VEC2"},
            {"bufferView": 3, "componentType": 5123, "count": 6, "type": "SCALAR"}],
        "bufferViews": [
            {"buffer": 0, "byteOffset": 0, "byteLength": 48},
            {"buffer": 0, "byteOffset": 48, "byteLength": 48},
            {"buffer": 0, "byteOffset": 96, "byteLength": 32},
            {"buffer": 0, "byteOffset": 128, "byteLength": 12}],
        "buffers": [{"byteLength": bin.len(),
            "uri": format!("data:application/octet-stream;base64,{}", b64(&bin))}]
    })
    .to_string()
}

#[test]
fn textured_gltf_round_trip() {
    let dir = tempdir("gltf");
    // only the signature matters to the writer and the reader
    let texture = b"\x89PNG\r\n\x1a\nnot really an image".to_vec();
    let src = dir.join("quad.gltf");
    std::fs::write(&src, quad_gltf(&texture)).unwrap();
    let source = dwg2gltf::input::load(&src).unwrap();
    let (src_pts, _) = {
        // world positions of the source, by the same traversal as the output
        let pts: Vec<[f64; 3]> = source
            .nodes
            .iter()
            .flat_map(|n| {
                source.meshes[n.mesh].primitives[0]
                    .positions
                    .iter()
                    .map(|&p| dwg2gltf::geom::apply(&n.matrix, p))
            })
            .collect();
        (pts, ())
    };
    for (name, version) in DwgVersion::NAMES {
        let dwg = dir.join(format!("quad_{name}.dwg"));
        let stats = dwg2gltf::convert_to_dwg(
            &src,
            &dwg,
            &WriteOptions {
                version,
                ..WriteOptions::default()
            },
        )
        .unwrap();
        assert_eq!(
            (stats.meshes, stats.blocks, stats.inserts, stats.textures),
            (2, 1, 2, 1),
            "{name}"
        );

        // every MESH has its MATERIAL in the file's material slot (flags 3,
        // first handle), not in the plot style slot where ODA ignores it
        let drawing = dwg2gltf::dwg::read(&dwg).unwrap();
        let materials: Vec<u64> = drawing
            .objects
            .iter()
            .filter(|o| matches!(&o.data, Data::Material(m) if m.name == "checker"))
            .map(|o| o.handle)
            .collect();
        assert_eq!(materials.len(), 1, "{name}");
        let meshes: Vec<&Entity> = drawing
            .objects
            .iter()
            .filter(|o| o.kind == "MESH")
            .filter_map(|o| match &o.data {
                Data::Entity(e) => Some(&**e),
                _ => None,
            })
            .collect();
        assert_eq!(meshes.len(), 2, "{name}");
        for e in meshes {
            assert_eq!(
                (e.material_flags, e.material_refs),
                (3, [materials[0], 0]),
                "{name}"
            );
        }

        let glb = dir.join(format!("quad_{name}.glb"));
        let back = dwg2gltf::convert(&dwg, &glb, &Options::default()).unwrap();
        // the block's triangles count once
        assert_eq!((back.triangles, back.instances), (4, 2), "{name}");
        let out = Glb::read(&glb);
        // the MATERIAL resolves, and its material archive brings the texture and
        // the PBR values back
        let mat = &out.json["materials"][0];
        assert_eq!(mat["name"], "checker", "{name}");
        assert_eq!(
            mat["pbrMetallicRoughness"]["roughnessFactor"]
                .as_f64()
                .unwrap(),
            0.8
        );
        let image = &out.json["images"][0];
        let view = &out.json["bufferViews"][image["bufferView"].as_u64().unwrap() as usize];
        let start = view["byteOffset"].as_u64().unwrap() as usize;
        assert_eq!(&out.bin[start..start + texture.len()], &texture[..]);

        let (pts, uvs) = out.world_points_and_uvs();
        let (lo, hi) = bounds(&pts);
        let (slo, shi) = bounds(&src_pts);
        for k in 0..3 {
            assert!(
                (lo[k] - slo[k]).abs() < 1e-5 && (hi[k] - shi[k]).abs() < 1e-5,
                "{name}: {lo:?} {hi:?} vs {slo:?} {shi:?}"
            );
        }
        let mut uvs: Vec<[f32; 2]> = uvs;
        uvs.sort_by(|a, b| a.partial_cmp(b).unwrap());
        uvs.dedup();
        assert_eq!(
            uvs,
            vec![[0.0, 0.0], [0.0, 1.0], [1.0, 0.0], [1.0, 1.0]],
            "{name}"
        );
    }
    std::fs::remove_dir_all(dir).unwrap();
}

#[test]
fn obj_and_stl_inputs() {
    let dir = tempdir("objstl");
    std::fs::write(dir.join("m.mtl"), "newmtl blue\nKd 0.2 0.4 0.6\n").unwrap();
    std::fs::write(
        dir.join("q.obj"),
        "mtllib m.mtl\nv 0 0 0\nv 1 0 0\nv 1 1 0\nv 0 1 0\nusemtl blue\nf 1 2 3 4\n",
    )
    .unwrap();
    let options = WriteOptions::default();
    let stats = dwg2gltf::convert_to_dwg(&dir.join("q.obj"), &dir.join("q.dwg"), &options).unwrap();
    assert_eq!((stats.meshes, stats.faces, stats.materials), (1, 1, 1));
    let back =
        dwg2gltf::convert(&dir.join("q.dwg"), &dir.join("q.glb"), &Options::default()).unwrap();
    assert_eq!(back.triangles, 2); // the quad stays one MESH face
    let out = Glb::read(&dir.join("q.glb"));
    let factor = &out.json["materials"][0]["pbrMetallicRoughness"]["baseColorFactor"];
    for (k, c) in [0.2, 0.4, 0.6].into_iter().enumerate() {
        // 8-bit sRGB in the DWG
        let expect = srgb_to_linear((c * 255.0f64).round() / 255.0);
        assert!(
            (factor[k].as_f64().unwrap() - expect).abs() < 1e-4,
            "{factor}"
        );
    }

    let mut stl = vec![0u8; 80];
    stl.extend(2u32.to_le_bytes());
    for tri in [
        [0.0f32, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0],
        [1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0],
    ] {
        stl.extend([0u8; 12]);
        stl.extend(tri.iter().flat_map(|f| f.to_le_bytes()));
        stl.extend([0, 0]);
    }
    std::fs::write(dir.join("t.stl"), stl).unwrap();
    let stats = dwg2gltf::convert_to_dwg(&dir.join("t.stl"), &dir.join("t.dwg"), &options).unwrap();
    assert_eq!((stats.meshes, stats.faces, stats.materials), (1, 2, 0));
    let back =
        dwg2gltf::convert(&dir.join("t.dwg"), &dir.join("t.glb"), &Options::default()).unwrap();
    assert_eq!(back.triangles, 2);
    std::fs::remove_dir_all(dir).unwrap();
}

/// DWG's CRC-16 (reflected 0xA001) as the header and classes sections use it.
fn crc16(data: &[u8], seed: u16) -> u16 {
    data.iter().fold(seed, |mut crc, &b| {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                crc >> 1 ^ 0xA001
            } else {
                crc >> 1
            };
        }
        crc
    })
}

/// The header variables and classes sections as ODA-based readers and
/// AutoCAD read them: sentinel, RL size, RL high size word (0), size bytes of
/// data, CRC-16 over size, high word and data, end sentinel. ODA refused
/// every file with "CRC does not match" when the size counted the high word.
#[test]
fn header_and_classes_sections_as_oda_reads_them() {
    const SECTIONS: [(&str, &str, &str); 2] = [
        (
            "header",
            "cf7b1f23fdde38a95f7c68b84e6d335f",
            "3084e0dc0221c756a0839747b192cca0",
        ),
        (
            "classes",
            "8da1c4b8c4a9f8c5c0dcf45fe7cfb68a",
            "725e3b473b56073a3f230ba018304975",
        ),
    ];
    let hex = |s: &str| -> Vec<u8> {
        (0..s.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap())
            .collect()
    };
    let rl = |d: &[u8], at: usize| u32::from_le_bytes(d[at..at + 4].try_into().unwrap()) as usize;
    let dir = tempdir("sections");
    let src = dir.join("quad.gltf");
    std::fs::write(&src, quad_gltf(b"\x89PNG\r\n\x1a\nnot really an image")).unwrap();
    for (name, version) in DwgVersion::NAMES {
        let dwg = dir.join(format!("quad_{name}.dwg"));
        let options = WriteOptions {
            version,
            ..WriteOptions::default()
        };
        dwg2gltf::convert_to_dwg(&src, &dwg, &options).unwrap();
        let data = std::fs::read(&dwg).unwrap();
        for (section, begin, end) in SECTIONS {
            // the writer stores data pages uncompressed and unencrypted
            let begin = hex(begin);
            let at = data
                .windows(16)
                .position(|w| w == begin)
                .unwrap_or_else(|| panic!("{name}: no {section} section"))
                + 16;
            let size = rl(&data, at);
            assert_eq!(rl(&data, at + 4), 0, "{name}: {section} high size word");
            let crc_at = at + 8 + size;
            let stored = u16::from_le_bytes([data[crc_at], data[crc_at + 1]]);
            assert_eq!(
                stored,
                crc16(&data[at..crc_at], 0xC0C1),
                "{name}: {section} CRC"
            );
            assert_eq!(
                &data[crc_at + 2..crc_at + 18],
                &hex(end)[..],
                "{name}: {section} end"
            );
        }
    }
    std::fs::remove_dir_all(dir).unwrap();
}
