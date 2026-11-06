// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Opt-in compression of a finished glTF document, the way gltfpack and
//! glTF-Transform's `optimize` do it:
//! - positions become 16-bit integers over their mesh's bounds and normals
//!   16-bit (KHR_mesh_quantization); the nodes carry the dequantization;
//!   triangles thinner than a step, which collapse or turn over on the
//!   grid, are left out;
//! - triangles are reordered for the vertex cache and vertices into first-use
//!   order, which also helps the codec;
//! - vertex and index data use meshoptimizer's codecs (EXT_meshopt_compression);
//! - a texture becomes WebP (EXT_texture_webp) when that is smaller (with
//!   the `webp` feature, on by default); lossy, except for normal maps,
//!   which are data and stay lossless.
//!
//! The extensions are required: three.js, Babylon.js and model-viewer read
//! the result; importers without meshopt support (Blender 4.2, for one) do
//! not.

#[cfg(feature = "webp")]
use libwebp_sys::{
    WebPEncodeLosslessRGB, WebPEncodeLosslessRGBA, WebPEncodeRGB, WebPEncodeRGBA, WebPFree,
};
use meshopt_rs::index::IndexEncodingVersion;
use meshopt_rs::index::buffer::{encode_index_buffer, encode_index_buffer_bound};
use meshopt_rs::index::sequence::{encode_index_sequence, encode_index_sequence_bound};
use meshopt_rs::vertex::VertexEncodingVersion;
use meshopt_rs::vertex::buffer::{encode_vertex_buffer, encode_vertex_buffer_bound};
use meshopt_rs::vertex::cache::optimize_vertex_cache;
use serde_json::{Map, Value, json};

use crate::geom::{IDENTITY, Mat, mat_mul, scale, translate};
use crate::gltf::{ARRAY_BUFFER, ELEMENT_ARRAY_BUFFER, FLOAT, TRIANGLES, UINT16, UINT32};

const SHORT: u32 = 5122;
/// libwebp quality (0..100) for re-encoded textures.
#[cfg(feature = "webp")]
const WEBP_QUALITY: f32 = 85.0;

pub(crate) struct Compressed {
    pub bin: Vec<u8>,
    /// Size of the decoded data the meshopt views describe (buffer 1).
    pub fallback: usize,
    /// Extensions the document now uses and requires.
    pub extensions: Vec<&'static str>,
}

/// Rewrite `doc` as [`crate::gltf::GltfWriter`] builds it (one tightly
/// packed view per accessor, float32 vertices, u16/u32 indices, images in
/// buffer views) and return its new binary buffer.
pub(crate) fn compress(doc: &mut Map<String, Value>, bin: &[u8]) -> Compressed {
    let take = |doc: &mut Map<String, Value>, key: &str| match doc[key].take() {
        Value::Array(v) => v,
        _ => vec![],
    };
    let mut p = Packer {
        src: bin,
        old_views: take(doc, "bufferViews"),
        old_accessors: take(doc, "accessors"),
        bin: vec![],
        views: vec![],
        accessors: vec![],
        fallback: 0,
    };
    let mut meshes = take(doc, "meshes");
    let dequant: Vec<Mat> = meshes.iter_mut().map(|m| p.mesh(m)).collect();
    doc["meshes"] = Value::Array(meshes);
    if let Some(nodes) = doc["nodes"].as_array_mut() {
        dequantize_nodes(nodes, &dequant);
    }
    let webp = p.images(doc);
    let mut extensions = vec![];
    if p.fallback > 0 {
        extensions.extend(["EXT_meshopt_compression", "KHR_mesh_quantization"]);
    }
    if webp {
        extensions.push("EXT_texture_webp");
    }
    doc["bufferViews"] = Value::Array(p.views);
    doc["accessors"] = Value::Array(p.accessors);
    Compressed {
        bin: p.bin,
        fallback: p.fallback,
        extensions,
    }
}

struct Packer<'a> {
    src: &'a [u8],
    old_views: Vec<Value>,
    old_accessors: Vec<Value>,
    bin: Vec<u8>,
    views: Vec<Value>,
    accessors: Vec<Value>,
    fallback: usize,
}

fn index(v: &Value) -> usize {
    v.as_u64().expect("writer layout") as usize
}

impl<'a> Packer<'a> {
    fn view_data(&self, view: usize) -> &'a [u8] {
        let v = &self.old_views[view];
        let start = v["byteOffset"].as_u64().unwrap_or(0) as usize;
        &self.src[start..start + index(&v["byteLength"])]
    }

    fn floats<const N: usize>(&self, accessor: usize) -> Vec<[f32; N]> {
        let a = &self.old_accessors[accessor];
        assert_eq!(a["componentType"], FLOAT, "writer layout");
        self.view_data(index(&a["bufferView"]))
            .chunks_exact(4 * N)
            .map(|c| {
                std::array::from_fn(|k| f32::from_le_bytes(c[4 * k..4 * k + 4].try_into().unwrap()))
            })
            .collect()
    }

    fn indices(&self, accessor: usize) -> Vec<u32> {
        let a = &self.old_accessors[accessor];
        let data = self.view_data(index(&a["bufferView"]));
        if a["componentType"] == UINT16 {
            data.chunks_exact(2)
                .map(|c| u16::from_le_bytes([c[0], c[1]]) as u32)
                .collect()
        } else {
            data.chunks_exact(4)
                .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
                .collect()
        }
    }

    fn align(&mut self) {
        while self.bin.len() % 4 != 0 {
            self.bin.push(0);
        }
    }

    fn push_view(&mut self, view: Value) -> usize {
        self.views.push(view);
        self.views.len() - 1
    }

    fn push_accessor(&mut self, accessor: Value) -> usize {
        self.accessors.push(accessor);
        self.accessors.len() - 1
    }

    /// A plain view of `data`.
    fn raw(&mut self, data: &[u8]) -> usize {
        self.align();
        let view = json!({"buffer": 0, "byteOffset": self.bin.len(), "byteLength": data.len()});
        self.bin.extend_from_slice(data);
        self.push_view(view)
    }

    /// A view of `count` elements of `stride` bytes, stored meshopt-encoded;
    /// it decodes into the fallback buffer.
    fn meshopt(&mut self, encoded: &[u8], count: usize, stride: usize, mode: &str) -> usize {
        self.align();
        let len = count * stride;
        let mut view = json!({
            "buffer": 1, "byteOffset": self.fallback, "byteLength": len,
            "extensions": {"EXT_meshopt_compression": {
                "buffer": 0, "byteOffset": self.bin.len(), "byteLength": encoded.len(),
                "byteStride": stride, "count": count, "mode": mode}}});
        if mode == "ATTRIBUTES" {
            view["byteStride"] = json!(stride);
            view["target"] = json!(ARRAY_BUFFER);
        } else {
            view["target"] = json!(ELEMENT_ARRAY_BUFFER);
        }
        self.bin.extend_from_slice(encoded);
        self.fallback += len.next_multiple_of(4);
        self.push_view(view)
    }

    fn attribute<const N: usize>(&mut self, vertices: &[[u8; N]], mut accessor: Value) -> usize {
        let mut buf = vec![0; encode_vertex_buffer_bound(vertices.len(), N)];
        let n = encode_vertex_buffer(&mut buf, vertices, VertexEncodingVersion::V0)
            .expect("buffer of the bound size");
        accessor["bufferView"] = json!(self.meshopt(&buf[..n], vertices.len(), N, "ATTRIBUTES"));
        accessor["count"] = json!(vertices.len());
        self.push_accessor(accessor)
    }

    fn index_accessor(&mut self, indices: &[u32], vertex_count: usize, triangles: bool) -> usize {
        let (mut buf, n, mode);
        if triangles {
            buf = vec![0; encode_index_buffer_bound(indices.len(), vertex_count)];
            n = encode_index_buffer(&mut buf, indices, IndexEncodingVersion::V1)
                .expect("buffer of the bound size");
            mode = "TRIANGLES";
        } else {
            buf = vec![0; encode_index_sequence_bound(indices.len(), vertex_count)];
            n = encode_index_sequence(&mut buf, indices, IndexEncodingVersion::V1);
            mode = "INDICES";
        }
        let (stride, ctype) = if vertex_count <= 0xFFFF {
            (2, UINT16)
        } else {
            (4, UINT32)
        };
        let view = self.meshopt(&buf[..n], indices.len(), stride, mode);
        self.push_accessor(json!({"bufferView": view, "componentType": ctype,
            "count": indices.len(), "type": "SCALAR"}))
    }

    /// Compress a mesh's primitives; returns the dequantization matrix for
    /// its nodes.
    fn mesh(&mut self, mesh: &mut Value) -> Mat {
        let prims = mesh["primitives"].as_array_mut().expect("writer layout");
        // the mesh's bounds, from the POSITION accessors' min/max
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        for p in prims.iter() {
            let a = &self.old_accessors[index(&p["attributes"]["POSITION"])];
            for k in 0..3 {
                lo[k] = lo[k].min(a["min"][k].as_f64().unwrap_or(f64::INFINITY));
                hi[k] = hi[k].max(a["max"][k].as_f64().unwrap_or(f64::NEG_INFINITY));
            }
        }
        if !lo.iter().chain(&hi).all(|c| c.is_finite()) {
            (lo, hi) = ([0.0; 3], [0.0; 3]);
        }
        let extent = (0..3).map(|k| hi[k] - lo[k]).fold(0.0, f64::max);
        let step = if extent > 0.0 { extent / 65535.0 } else { 1.0 };
        for p in prims.iter_mut() {
            self.primitive(p, lo, step);
        }
        mat_mul(&translate(lo), &scale([step; 3]))
    }

    fn primitive(&mut self, p: &mut Value, lo: [f64; 3], step: f64) {
        let attr = |name: &str| p["attributes"][name].as_u64().map(|a| a as usize);
        let positions = self.floats::<3>(attr("POSITION").expect("writer layout"));
        let normals = attr("NORMAL").map(|a| self.floats::<3>(a));
        let uvs = attr("TEXCOORD_0").map(|a| self.floats::<2>(a));
        let triangles = p["mode"].as_u64().unwrap_or(TRIANGLES as u64) == TRIANGLES as u64;
        let mut indices = self.indices(index(&p["indices"]));
        let grid = |i: u32| -> [i64; 3] {
            std::array::from_fn(|k| {
                ((positions[i as usize][k] as f64 - lo[k]) / step)
                    .round()
                    .clamp(0.0, 65535.0) as i64
            })
        };
        if triangles {
            // a triangle must keep its area and its facing on the grid
            let kept: Vec<u32> = indices
                .chunks_exact(3)
                .filter(|t| {
                    let [a, b, c] = [t[0], t[1], t[2]].map(grid);
                    let q = cross_i(sub_i(b, a), sub_i(c, a));
                    let [fa, fb, fc] =
                        [t[0], t[1], t[2]].map(|i| positions[i as usize].map(f64::from));
                    let f = crate::geom::cross(crate::geom::sub(fb, fa), crate::geom::sub(fc, fa));
                    q != [0; 3] && (0..3).map(|k| q[k] as f64 * f[k]).sum::<f64>() > 0.0
                })
                .flatten()
                .copied()
                .collect();
            if !kept.is_empty() {
                indices = kept;
            }
        }
        if triangles {
            let mut ordered = vec![0; indices.len()];
            optimize_vertex_cache(&mut ordered, &indices, positions.len());
            indices = ordered;
        }
        let order = first_use(&mut indices, positions.len());

        let quantized: Vec<[u16; 3]> = order
            .iter()
            .map(|&i| {
                std::array::from_fn(|k| {
                    ((positions[i][k] as f64 - lo[k]) / step)
                        .round()
                        .clamp(0.0, 65535.0) as u16
                })
            })
            .collect();
        let (mut qmin, mut qmax) = ([u16::MAX; 3], [0; 3]);
        for q in &quantized {
            for k in 0..3 {
                qmin[k] = qmin[k].min(q[k]);
                qmax[k] = qmax[k].max(q[k]);
            }
        }
        // 6 bytes padded to 8: vertex attributes are 4-byte aligned
        let packed: Vec<[u8; 8]> = quantized
            .iter()
            .map(|q| {
                let mut out = [0; 8];
                for k in 0..3 {
                    out[2 * k..2 * k + 2].copy_from_slice(&q[k].to_le_bytes());
                }
                out
            })
            .collect();
        let mut attrs = json!({"POSITION": self.attribute(&packed, json!({
            "componentType": UINT16, "type": "VEC3", "min": qmin, "max": qmax}))});
        if let Some(normals) = normals {
            // 16-bit: 8-bit normals mottle the shading of large smooth
            // surfaces (upholstery)
            let packed: Vec<[u8; 8]> = order
                .iter()
                .map(|&i| {
                    let mut out = [0; 8];
                    for k in 0..3 {
                        let c = (normals[i][k] * 32767.0).round().clamp(-32767.0, 32767.0) as i16;
                        out[2 * k..2 * k + 2].copy_from_slice(&c.to_le_bytes());
                    }
                    out
                })
                .collect();
            attrs["NORMAL"] = json!(self.attribute(
                &packed,
                json!({
                "componentType": SHORT, "normalized": true, "type": "VEC3"})
            ));
        }
        if let Some(uvs) = uvs {
            // float32: OFML texture coordinates leave 0..1
            let packed: Vec<[u8; 8]> = order
                .iter()
                .map(|&i| {
                    let mut out = [0; 8];
                    out[..4].copy_from_slice(&uvs[i][0].to_le_bytes());
                    out[4..].copy_from_slice(&uvs[i][1].to_le_bytes());
                    out
                })
                .collect();
            attrs["TEXCOORD_0"] = json!(self.attribute(
                &packed,
                json!({
                "componentType": FLOAT, "type": "VEC2"})
            ));
        }
        p["attributes"] = attrs;
        p["indices"] = json!(self.index_accessor(&indices, order.len(), triangles));
    }

    /// Copy the images, as WebP where that is smaller; returns whether any
    /// became WebP.
    fn images(&mut self, doc: &mut Map<String, Value>) -> bool {
        let normal_maps = normal_map_images(doc);
        let Some(images) = doc.get_mut("images").and_then(Value::as_array_mut) else {
            return false;
        };
        let mut webp = vec![false; images.len()];
        for (i, img) in images.iter_mut().enumerate() {
            let data = self.view_data(index(&img["bufferView"]));
            // normal maps are data: lossy coding bends the normals
            let lossless = normal_maps.contains(&i);
            let encoded = to_webp(data, lossless).filter(|w| w.len() < data.len());
            img["bufferView"] = json!(self.raw(encoded.as_deref().unwrap_or(data)));
            if encoded.is_some() {
                img["mimeType"] = json!("image/webp");
                webp[i] = true;
            }
        }
        for t in doc
            .get_mut("textures")
            .and_then(Value::as_array_mut)
            .into_iter()
            .flatten()
        {
            let source = t["source"].as_u64().map(|s| s as usize);
            if let (Some(s), Some(obj)) = (source, t.as_object_mut()) {
                if webp[s] {
                    obj.remove("source");
                    obj.insert(
                        "extensions".into(),
                        json!({"EXT_texture_webp": {"source": s}}),
                    );
                }
            }
        }
        webp.contains(&true)
    }
}

/// Renumber the vertices in order of first use; returns the old index of
/// each new vertex (unused vertices are dropped).
fn sub_i(a: [i64; 3], b: [i64; 3]) -> [i64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

fn cross_i(a: [i64; 3], b: [i64; 3]) -> [i64; 3] {
    [
        a[1] * b[2] - a[2] * b[1],
        a[2] * b[0] - a[0] * b[2],
        a[0] * b[1] - a[1] * b[0],
    ]
}

fn first_use(indices: &mut [u32], vertex_count: usize) -> Vec<usize> {
    let mut remap = vec![u32::MAX; vertex_count];
    let mut order = vec![];
    for i in indices.iter_mut() {
        let r = &mut remap[*i as usize];
        if *r == u32::MAX {
            *r = order.len() as u32;
            order.push(*i as usize);
        }
        *i = *r;
    }
    order
}

/// Apply each mesh's dequantization in the nodes that show it. A node with
/// children hands its mesh to a new child, so the children keep their place.
fn dequantize_nodes(nodes: &mut Vec<Value>, dequant: &[Mat]) {
    for i in 0..nodes.len() {
        let Some(mesh) = nodes[i]["mesh"].as_u64() else {
            continue;
        };
        let d = &dequant[mesh as usize];
        if nodes[i]["children"].is_null() {
            let m = mat_mul(&row_major(&nodes[i]["matrix"]), d);
            nodes[i]["matrix"] = column_major(&m);
        } else {
            let node = nodes[i].as_object_mut().expect("writer layout");
            let mut child = Map::new();
            if let Some(name) = node.get("name") {
                child.insert("name".into(), name.clone());
            }
            child.insert("matrix".into(), column_major(d));
            child.insert("mesh".into(), node.remove("mesh").expect("checked above"));
            let at = nodes.len();
            nodes.push(Value::Object(child));
            nodes[i]["children"]
                .as_array_mut()
                .expect("writer layout")
                .push(json!(at));
        }
    }
}

/// A glTF (column-major) matrix as row-major; identity when absent.
fn row_major(m: &Value) -> Mat {
    match m.as_array() {
        Some(v) if v.len() == 16 => {
            std::array::from_fn(|i| v[(i % 4) * 4 + i / 4].as_f64().unwrap_or(0.0))
        }
        _ => IDENTITY,
    }
}

fn column_major(m: &Mat) -> Value {
    json!(
        (0..16)
            .map(|i| m[(i % 4) * 4 + i / 4])
            .collect::<Vec<f64>>()
    )
}

/// The images that materials use as normal maps (indices into `images`).
fn normal_map_images(doc: &Map<String, Value>) -> Vec<usize> {
    let textures = doc.get("textures").and_then(Value::as_array);
    doc.get("materials")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|m| m["normalTexture"]["index"].as_u64())
        .filter_map(|t| textures?.get(t as usize)?["source"].as_u64())
        .map(|s| s as usize)
        .collect()
}

/// Without libwebp (the WebAssembly build) textures keep their format.
#[cfg(not(feature = "webp"))]
fn to_webp(_data: &[u8], _lossless: bool) -> Option<Vec<u8>> {
    None
}

/// `data` (any format the image crate reads) as WebP, lossy or lossless;
/// None when it cannot be decoded or encoded (WebP allows at most
/// 16383 x 16383).
#[cfg(feature = "webp")]
fn to_webp(data: &[u8], lossless: bool) -> Option<Vec<u8>> {
    let img = image::load_from_memory(data).ok()?;
    let (w, h) = (
        i32::try_from(img.width()).ok()?,
        i32::try_from(img.height()).ok()?,
    );
    let rgba = img.to_rgba8();
    let opaque = rgba.pixels().all(|p| p[3] == 255);
    let mut out: *mut u8 = std::ptr::null_mut();
    // SAFETY: the pixel buffers hold w * h pixels of 3 or 4 bytes, rows
    // tightly packed; libwebp allocates `out` (freed below) and returns its
    // size, or 0 on failure.
    let size = unsafe {
        match (opaque, lossless) {
            (true, false) => {
                let rgb = img.to_rgb8();
                WebPEncodeRGB(rgb.as_ptr(), w, h, w * 3, WEBP_QUALITY, &mut out)
            }
            (false, false) => WebPEncodeRGBA(rgba.as_ptr(), w, h, w * 4, WEBP_QUALITY, &mut out),
            (true, true) => {
                let rgb = img.to_rgb8();
                WebPEncodeLosslessRGB(rgb.as_ptr(), w, h, w * 3, &mut out)
            }
            (false, true) => WebPEncodeLosslessRGBA(rgba.as_ptr(), w, h, w * 4, &mut out),
        }
    };
    if out.is_null() {
        return None;
    }
    // SAFETY: `out` holds `size` bytes until WebPFree.
    let webp = (size > 0).then(|| unsafe { std::slice::from_raw_parts(out, size) }.to_vec());
    unsafe { WebPFree(out.cast()) };
    webp
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A noisy RGB PNG (lossy WebP cannot keep it exactly).
    fn noise_png() -> (Vec<u8>, image::RgbImage) {
        let mut seed = 12345u32;
        let img = image::RgbImage::from_fn(64, 64, |_, _| {
            seed = seed.wrapping_mul(1_103_515_245).wrapping_add(12345);
            image::Rgb([
                (seed >> 16) as u8,
                (seed >> 8) as u8,
                128 + (seed >> 25) as u8,
            ])
        });
        let mut png = vec![];
        img.write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
            .unwrap();
        (png, img)
    }

    #[test]
    fn normal_maps_stay_lossless() {
        let (png, img) = noise_png();
        let webp = to_webp(&png, true).expect("encodes");
        let back = image::load_from_memory(&webp).unwrap().to_rgb8();
        assert_eq!(back, img);
        let lossy = to_webp(&png, false).expect("encodes");
        assert_ne!(image::load_from_memory(&lossy).unwrap().to_rgb8(), img);
    }

    #[test]
    fn normal_map_images_by_material() {
        let doc = json!({
            "materials": [
                {"pbrMetallicRoughness": {"baseColorTexture": {"index": 0}}, "normalTexture": {"index": 1}},
                {"pbrMetallicRoughness": {}}
            ],
            "textures": [{"source": 0, "sampler": 0}, {"source": 2, "sampler": 0}]
        });
        assert_eq!(normal_map_images(doc.as_object().unwrap()), vec![2]);
    }

    #[test]
    fn first_use_order_drops_unused() {
        let mut indices = vec![3, 1, 3, 4];
        assert_eq!(first_use(&mut indices, 5), vec![3, 1, 4]);
        assert_eq!(indices, vec![0, 1, 0, 2]);
    }

    #[test]
    fn dequantization_spares_children() {
        let d = mat_mul(&translate([1.0, 2.0, 3.0]), &scale([0.5; 3]));
        let t = translate([10.0, 0.0, 0.0]);
        let mut nodes = vec![
            json!({"name": "parent", "mesh": 0, "children": [1]}),
            json!({"name": "leaf", "mesh": 0, "matrix": column_major(&t)}),
        ];
        dequantize_nodes(&mut nodes, &[d]);
        assert!(nodes[0].get("mesh").is_none());
        assert_eq!(nodes[0]["children"], json!([1, 2]));
        assert_eq!(nodes[2]["mesh"], 0);
        assert_eq!(row_major(&nodes[2]["matrix"]), d);
        assert_eq!(row_major(&nodes[1]["matrix"]), mat_mul(&t, &d));
    }
}
