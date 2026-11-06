// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Minimal glTF 2.0 writer (GLB, or .gltf with an embedded buffer).

use std::collections::{BTreeSet, HashMap};
use std::path::Path;

use serde_json::{Map, Value, json};

use crate::Error;
use crate::geom::{IDENTITY, Mat, Vec3, apply, mat_mul};

pub(crate) const FLOAT: u32 = 5126;
pub(crate) const UINT16: u32 = 5123;
pub(crate) const UINT32: u32 = 5125;
pub(crate) const ARRAY_BUFFER: u32 = 34962;
pub(crate) const ELEMENT_ARRAY_BUFFER: u32 = 34963;
pub const TRIANGLES: u32 = 4;
pub const LINES: u32 = 1;

pub fn srgb_to_linear(c: f64) -> f64 {
    if c <= 0.04045 {
        c / 12.92
    } else {
        ((c + 0.055) / 1.055).powf(2.4)
    }
}

pub fn linear_to_srgb(c: f64) -> f64 {
    if c <= 0.0031308 {
        c * 12.92
    } else {
        1.055 * c.powf(1.0 / 2.4) - 0.055
    }
}

/// A material's parameters, as [`GltfWriter::add_material`] takes them.
pub struct MaterialDesc<'a> {
    pub name: &'a str,
    pub rgb: [f64; 3],
    pub alpha: f64,
    pub metallic: f64,
    pub roughness: f64,
    pub texture: Option<usize>,
    /// Tangent-space normal map, on the same texture coordinates.
    pub normal: Option<NormalTexture>,
    pub unlit: bool,
    pub extras: Option<Value>,
}

impl<'a> MaterialDesc<'a> {
    pub fn new(name: &'a str, rgb: [f64; 3]) -> Self {
        MaterialDesc {
            name,
            rgb,
            alpha: 1.0,
            metallic: 0.0,
            roughness: 0.7,
            texture: None,
            normal: None,
            unlit: false,
            extras: None,
        }
    }
}

/// A material's `normalTexture`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct NormalTexture {
    /// Texture index, from [`GltfWriter::add_image_texture`].
    pub index: usize,
    /// Its own transform of the texture coordinates, if any.
    pub transform: Option<TextureTransform>,
}

/// `KHR_texture_transform`: uv' = T(offset) · R(rotation) · S(scale) · uv,
/// R(r) = [cos r, sin r; −sin r, cos r] (glTF UVs, v down).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TextureTransform {
    pub offset: [f64; 2],
    pub rotation: f64,
    pub scale: [f64; 2],
}

impl TextureTransform {
    /// The transform equal to the 2x3 affine map `m` (row-major
    /// [a b c; d e f]), or None when `m` shears.
    pub fn from_affine(m: [f64; 6]) -> Option<Self> {
        if m.iter().any(|v| !v.is_finite()) {
            return None;
        }
        // columns of R·S: sx (cos, −sin) and sy (sin, cos)
        let sx = m[0].hypot(m[3]);
        if sx < 1e-12 {
            return None;
        }
        let rotation = (-m[3]).atan2(m[0]);
        let (sin, cos) = rotation.sin_cos();
        let sy = m[1] * sin + m[4] * cos;
        let residual = (m[1] - sy * sin).hypot(m[4] - sy * cos);
        if residual > 1e-9 * sx.max(sy.abs()) || sy.abs() < 1e-12 {
            return None;
        }
        Some(TextureTransform {
            offset: [m[2], m[5]],
            rotation,
            scale: [sx, sy],
        })
    }
}

pub struct GltfWriter {
    doc: Map<String, Value>,
    bin: Vec<u8>,
    /// Texture index by image bytes: equal images are stored once.
    textures: HashMap<Vec<u8>, usize>,
    extensions_used: BTreeSet<&'static str>,
    extensions_required: BTreeSet<&'static str>,
    /// Size of the EXT_meshopt_compression fallback buffer (0: none).
    fallback: usize,
    /// A non-finite number reached the document: a bug upstream.
    non_finite: bool,
}

const LISTS: [&str; 6] = [
    "nodes",
    "meshes",
    "materials",
    "accessors",
    "bufferViews",
    "buffers",
];

impl GltfWriter {
    pub fn new(generator: &str) -> Self {
        let mut doc = Map::new();
        doc.insert(
            "asset".into(),
            json!({"version": "2.0", "generator": generator}),
        );
        doc.insert("scene".into(), json!(0));
        doc.insert("scenes".into(), json!([{"name": "Scene", "nodes": []}]));
        for key in LISTS {
            doc.insert(key.into(), json!([]));
        }
        GltfWriter {
            doc,
            bin: vec![],
            textures: HashMap::new(),
            extensions_used: BTreeSet::new(),
            extensions_required: BTreeSet::new(),
            fallback: 0,
            non_finite: false,
        }
    }

    fn num(&mut self, x: f64) -> Value {
        match serde_json::Number::from_f64(x) {
            Some(n) => Value::Number(n),
            None => {
                self.non_finite = true;
                Value::Null
            }
        }
    }

    fn push(&mut self, list: &str, item: Value) -> usize {
        let arr = self
            .doc
            .entry(list)
            .or_insert_with(|| json!([]))
            .as_array_mut()
            .unwrap();
        arr.push(item);
        arr.len() - 1
    }

    // -- binary data -------------------------------------------------------
    fn view(&mut self, data: &[u8], target: Option<u32>) -> usize {
        self.pad_bin();
        let mut view = json!({"buffer": 0, "byteOffset": self.bin.len(), "byteLength": data.len()});
        if let Some(t) = target {
            view["target"] = json!(t);
        }
        self.bin.extend_from_slice(data);
        self.push("bufferViews", view)
    }

    fn pad_bin(&mut self) {
        while self.bin.len() % 4 != 0 {
            self.bin.push(0);
        }
    }

    /// Float32 VEC2/VEC3 accessor; `with_bounds` adds min/max (required for POSITION).
    pub fn vec_accessor<const N: usize>(
        &mut self,
        values: &[[f64; N]],
        with_bounds: bool,
    ) -> usize {
        let mut data = Vec::with_capacity(values.len() * N * 4);
        for v in values {
            for c in v {
                data.extend_from_slice(&(*c as f32).to_le_bytes());
            }
        }
        let view = self.view(&data, Some(ARRAY_BUFFER));
        let kind = if N == 2 { "VEC2" } else { "VEC3" };
        let mut acc = json!({"bufferView": view, "componentType": FLOAT, "count": values.len(), "type": kind});
        if with_bounds && !values.is_empty() {
            // min/max must match the float32 data exactly
            let (mut lo, mut hi) = ([f64::INFINITY; N], [f64::NEG_INFINITY; N]);
            for v in values {
                for i in 0..N {
                    lo[i] = lo[i].min(v[i]);
                    hi[i] = hi[i].max(v[i]);
                }
            }
            let lo: Vec<Value> = lo.iter().map(|&c| self.num(c as f32 as f64)).collect();
            let hi: Vec<Value> = hi.iter().map(|&c| self.num(c as f32 as f64)).collect();
            acc["min"] = Value::Array(lo);
            acc["max"] = Value::Array(hi);
        }
        self.push("accessors", acc)
    }

    pub fn index_accessor(&mut self, indices: &[u32], vertex_count: usize) -> usize {
        let (data, ctype): (Vec<u8>, u32) = if vertex_count <= 0xFFFF {
            (
                indices
                    .iter()
                    .flat_map(|&i| (i as u16).to_le_bytes())
                    .collect(),
                UINT16,
            )
        } else {
            (
                indices.iter().flat_map(|&i| i.to_le_bytes()).collect(),
                UINT32,
            )
        };
        let view = self.view(&data, Some(ELEMENT_ARRAY_BUFFER));
        self.push("accessors", json!({"bufferView": view, "componentType": ctype, "count": indices.len(), "type": "SCALAR"}))
    }

    // -- scene objects -----------------------------------------------------
    pub fn add_image_texture(&mut self, data: &[u8], mime: &str, name: &str) -> usize {
        if let Some(&t) = self.textures.get(data) {
            return t;
        }
        let view = self.view(data, None);
        let image = self.push(
            "images",
            json!({"bufferView": view, "mimeType": mime, "name": name}),
        );
        self.doc
            .entry("samplers")
            .or_insert_with(|| json!([{"wrapS": 10497, "wrapT": 10497}]));
        let t = self.push("textures", json!({"source": image, "sampler": 0}));
        self.textures.insert(data.to_vec(), t);
        t
    }

    pub fn add_material(&mut self, m: MaterialDesc) -> usize {
        let mut factor: Vec<Value> = m.rgb.iter().map(|&c| self.num(srgb_to_linear(c))).collect();
        factor.push(self.num(m.alpha));
        let mut pbr = json!({"baseColorFactor": factor, "metallicFactor": self.num(m.metallic), "roughnessFactor": self.num(m.roughness)});
        if let Some(t) = m.texture {
            pbr["baseColorTexture"] = json!({"index": t});
        }
        let mut mat = json!({"name": m.name, "pbrMetallicRoughness": pbr, "doubleSided": true});
        if let Some(n) = m.normal {
            let mut info = json!({"index": n.index});
            if let Some(t) = n.transform {
                let offset: Vec<Value> = t.offset.iter().map(|&c| self.num(c)).collect();
                let scale: Vec<Value> = t.scale.iter().map(|&c| self.num(c)).collect();
                info["extensions"] = json!({"KHR_texture_transform": {
                    "offset": offset, "rotation": self.num(t.rotation), "scale": scale}});
                self.extensions_used.insert("KHR_texture_transform");
            }
            mat["normalTexture"] = info;
        }
        if m.alpha < 1.0 {
            mat["alphaMode"] = json!("BLEND");
        }
        if m.unlit {
            mat["extensions"] = json!({"KHR_materials_unlit": {}});
            self.extensions_used.insert("KHR_materials_unlit");
        }
        if let Some(extras) = m.extras {
            mat["extras"] = extras;
        }
        self.push("materials", mat)
    }

    pub fn add_mesh(&mut self, name: &str, primitives: Vec<Value>) -> usize {
        self.push("meshes", json!({"name": name, "primitives": primitives}))
    }

    /// `matrix`: row-major; stored column-major as glTF wants.
    pub fn add_node(
        &mut self,
        name: &str,
        matrix: Option<&[f64; 16]>,
        mesh: Option<usize>,
        children: &[usize],
        extras: Option<Value>,
    ) -> usize {
        let mut node = json!({"name": name});
        if let Some(m) = matrix {
            let cols: Vec<Value> = (0..16).map(|i| self.num(m[(i % 4) * 4 + i / 4])).collect();
            node["matrix"] = Value::Array(cols);
        }
        if let Some(mesh) = mesh {
            node["mesh"] = json!(mesh);
        }
        if !children.is_empty() {
            node["children"] = json!(children);
        }
        if let Some(extras) = extras {
            node["extras"] = extras;
        }
        self.push("nodes", node)
    }

    pub fn add_root(&mut self, node: usize) {
        self.doc["scenes"][0]["nodes"]
            .as_array_mut()
            .unwrap()
            .push(json!(node));
    }

    pub fn document(&self) -> &Map<String, Value> {
        &self.doc
    }

    /// The scene's bounding box in world coordinates: every POSITION
    /// accessor's min/max corners through its node's matrices (so a rotated
    /// box comes out larger). None for an empty scene. Call it before
    /// [`Self::compress`], which turns the positions into integers.
    pub fn bounds(&self) -> Option<(Vec3, Vec3)> {
        let nodes = self.doc["nodes"].as_array()?;
        let mut stack: Vec<(usize, Mat)> = self.doc["scenes"][0]["nodes"]
            .as_array()?
            .iter()
            .filter_map(|n| Some((n.as_u64()? as usize, IDENTITY)))
            .collect();
        let corner = |acc: &Value, key: &str| -> Option<Vec3> {
            let v = acc[key].as_array()?;
            Some([
                v.first()?.as_f64()?,
                v.get(1)?.as_f64()?,
                v.get(2)?.as_f64()?,
            ])
        };
        let (mut lo, mut hi) = ([f64::INFINITY; 3], [f64::NEG_INFINITY; 3]);
        while let Some((i, parent)) = stack.pop() {
            let node = &nodes[i];
            let world = match node["matrix"].as_array() {
                Some(cols) => {
                    let cols: Option<Vec<f64>> = cols.iter().map(Value::as_f64).collect();
                    let Some(cols) = cols.filter(|c| c.len() == 16) else {
                        continue; // non-finite: the document is refused anyway
                    };
                    // stored column-major
                    mat_mul(&parent, &std::array::from_fn(|k| cols[(k % 4) * 4 + k / 4]))
                }
                None => parent,
            };
            if let Some(mesh) = node["mesh"].as_u64() {
                for p in self.doc["meshes"][mesh as usize]["primitives"]
                    .as_array()
                    .into_iter()
                    .flatten()
                {
                    let Some(acc) = p["attributes"]["POSITION"]
                        .as_u64()
                        .map(|a| &self.doc["accessors"][a as usize])
                    else {
                        continue;
                    };
                    let (Some(a), Some(b)) = (corner(acc, "min"), corner(acc, "max")) else {
                        continue;
                    };
                    for c in 0..8 {
                        let p = apply(
                            &world,
                            std::array::from_fn(|k| if c >> k & 1 == 0 { a[k] } else { b[k] }),
                        );
                        for k in 0..3 {
                            lo[k] = lo[k].min(p[k]);
                            hi[k] = hi[k].max(p[k]);
                        }
                    }
                }
            }
            for c in node["children"].as_array().into_iter().flatten() {
                if let Some(c) = c.as_u64() {
                    stack.push((c as usize, world));
                }
            }
        }
        (lo[0] <= hi[0]).then_some((lo, hi))
    }

    /// Quantize and meshopt-compress the geometry and re-encode textures as
    /// WebP; see [`crate::compress`]. Call once, after the last `add_*`.
    pub fn compress(&mut self) {
        let out = crate::compress::compress(&mut self.doc, &self.bin);
        self.bin = out.bin;
        self.fallback = out.fallback;
        self.extensions_used.extend(&out.extensions);
        self.extensions_required.extend(&out.extensions);
    }

    // -- output ------------------------------------------------------------
    fn finish(&mut self) -> Result<Map<String, Value>, Error> {
        if self.non_finite {
            return Err(Error::NonFinite);
        }
        self.pad_bin();
        let mut doc: Map<String, Value> = self
            .doc
            .iter()
            .filter(|(_, v)| **v != json!([]))
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        if doc["scenes"][0]["nodes"] == json!([]) {
            doc["scenes"] = json!([{"name": "Scene"}]); // an empty node list is invalid
        }
        if !self.extensions_used.is_empty() {
            doc.insert("extensionsUsed".into(), json!(self.extensions_used));
        }
        if !self.extensions_required.is_empty() {
            doc.insert("extensionsRequired".into(), json!(self.extensions_required));
        }
        if !self.bin.is_empty() {
            let mut buffers = vec![json!({"byteLength": self.bin.len()})];
            if self.fallback > 0 {
                // meshopt views decode into this buffer; it has no data
                buffers.push(json!({"byteLength": self.fallback,
                    "extensions": {"EXT_meshopt_compression": {"fallback": true}}}));
            }
            doc.insert("buffers".into(), json!(buffers));
        }
        Ok(doc)
    }

    /// Write a .glb file, or a .gltf file with an embedded buffer.
    pub fn write(self, path: &Path) -> Result<(), Error> {
        let gltf = path
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("gltf"));
        let data = if gltf {
            self.to_gltf()?
        } else {
            self.to_glb()?
        };
        std::fs::write(path, data)?;
        Ok(())
    }

    /// The .gltf file: JSON with the buffer embedded as a data URI.
    pub fn to_gltf(mut self) -> Result<Vec<u8>, Error> {
        let mut doc = self.finish()?;
        if !self.bin.is_empty() {
            doc["buffers"][0]["uri"] = json!(format!(
                "data:application/octet-stream;base64,{}",
                base64(&self.bin)
            ));
        }
        Ok(serde_json::to_vec(&doc).expect("JSON values serialize"))
    }

    /// The .glb file.
    pub fn to_glb(mut self) -> Result<Vec<u8>, Error> {
        let doc = self.finish()?;
        let mut js = serde_json::to_vec(&doc).expect("JSON values serialize");
        while js.len() % 4 != 0 {
            js.push(b' ');
        }
        let bin_chunk = if self.bin.is_empty() {
            0
        } else {
            8 + self.bin.len()
        };
        let total = 12 + 8 + js.len() + bin_chunk;
        let mut out = Vec::with_capacity(total);
        for word in [0x46546C67, 2, total as u32, js.len() as u32, 0x4E4F534A] {
            out.extend_from_slice(&u32::to_le_bytes(word));
        }
        out.extend_from_slice(&js);
        if !self.bin.is_empty() {
            out.extend_from_slice(&(self.bin.len() as u32).to_le_bytes());
            out.extend_from_slice(&0x004E4942u32.to_le_bytes());
            out.extend_from_slice(&self.bin);
        }
        Ok(out)
    }
}

pub(crate) fn base64(data: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let b = [
            chunk[0],
            *chunk.get(1).unwrap_or(&0),
            *chunk.get(2).unwrap_or(&0),
        ];
        let n = (b[0] as u32) << 16 | (b[1] as u32) << 8 | b[2] as u32;
        for i in 0..4 {
            if i <= chunk.len() {
                out.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize] as char);
            } else {
                out.push('=');
            }
        }
    }
    out
}

/// Decode standard base64 (padding optional, whitespace ignored).
pub fn base64_decode(text: &str) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(text.len() / 4 * 3);
    let (mut acc, mut bits) = (0u32, 0u32);
    for c in text.bytes() {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            b'=' => break,
            b' ' | b'\t' | b'\r' | b'\n' => continue,
            _ => return None,
        };
        acc = acc << 6 | v as u32;
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            out.push((acc >> bits) as u8);
            acc &= (1 << bits) - 1;
        }
    }
    Some(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn texture_transform_from_affine() {
        let (sx, sy, r) = (3.0, -0.5, 0.3f64);
        let (s, c) = r.sin_cos();
        // T · R · S with R = [cos sin; -sin cos]
        let m = [c * sx, s * sy, 0.25, -s * sx, c * sy, -2.0];
        let t = TextureTransform::from_affine(m).unwrap();
        assert!((t.rotation - r).abs() < 1e-12);
        assert!((t.scale[0] - sx).abs() < 1e-12 && (t.scale[1] - sy).abs() < 1e-12);
        assert_eq!(t.offset, [0.25, -2.0]);
        // a shear has no KHR_texture_transform
        assert_eq!(
            TextureTransform::from_affine([1.0, 0.5, 0.0, 0.0, 1.0, 0.0]),
            None
        );
    }

    #[test]
    fn normal_texture_is_written() {
        let mut w = GltfWriter::new("test");
        let t = w.add_image_texture(b"png", "image/png", "n");
        let mut desc = MaterialDesc::new("m", [1.0; 3]);
        desc.normal = Some(NormalTexture {
            index: t,
            transform: None,
        });
        w.add_material(desc);
        let mut desc = MaterialDesc::new("m2", [1.0; 3]);
        desc.normal = Some(NormalTexture {
            index: t,
            transform: Some(TextureTransform {
                offset: [0.0, 1.0],
                rotation: 0.0,
                scale: [2.0, 2.0],
            }),
        });
        w.add_material(desc);
        let doc = w.finish().unwrap();
        assert_eq!(doc["materials"][0]["normalTexture"], json!({"index": t}));
        assert!(doc["materials"][0]["normalTexture"].get("scale").is_none());
        assert_eq!(
            doc["materials"][1]["normalTexture"]["extensions"]["KHR_texture_transform"]["scale"],
            json!([2.0, 2.0])
        );
        assert_eq!(doc["extensionsUsed"], json!(["KHR_texture_transform"]));
        assert!(doc.get("extensionsRequired").is_none());
    }

    #[test]
    fn base64_padding() {
        assert_eq!(base64(b""), "");
        assert_eq!(base64(b"f"), "Zg==");
        assert_eq!(base64(b"fo"), "Zm8=");
        assert_eq!(base64(b"foo"), "Zm9v");
        assert_eq!(base64(b"foobar"), "Zm9vYmFy");
    }

    #[test]
    fn base64_round_trip() {
        for data in [
            &b""[..],
            b"f",
            b"fo",
            b"foo",
            b"foob",
            b"\xff\x00\x10binary",
        ] {
            assert_eq!(base64_decode(&base64(data)).as_deref(), Some(data));
        }
        assert_eq!(base64_decode("Zm9v\nYmE").as_deref(), Some(&b"fooba"[..]));
        assert_eq!(base64_decode("Zm9v*"), None);
    }
}
