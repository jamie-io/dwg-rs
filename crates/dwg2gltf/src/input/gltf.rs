// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! glTF 2.0 reader (.gltf with data URIs or external files, and .glb):
//! meshes, the node hierarchy, and each material's base color, metallic and
//! roughness factors and base color texture. Reads meshopt-compressed and
//! quantized files (as gltfpack, glTF-Transform and `dwg2gltf --compress`
//! write them), not Draco.

use std::path::Path;

use meshopt_rs::index::buffer::decode_index_buffer;
use meshopt_rs::index::sequence::decode_index_sequence;
use meshopt_rs::vertex::buffer::decode_vertex_buffer;
use meshopt_rs::vertex::filter::{
    decode_filter_exp, decode_filter_oct_8, decode_filter_oct_16, decode_filter_quat,
};
use serde_json::{Value, json};

use super::{
    InputError, Material, Mesh, Node, Primitive, Scene, Texture, clean_faces, flat_normals,
};
use crate::geom::{IDENTITY, Mat, Vec2, Vec3, add, mat_mul, mul, scale, translate};
use crate::gltf::{base64_decode, linear_to_srgb};

const MAX_DEPTH: usize = 64;

/// Extensions a file may require: handled, or (unlit) without effect on
/// the geometry.
const SUPPORTED: [&str; 6] = [
    "KHR_mesh_quantization",
    "EXT_meshopt_compression",
    "EXT_texture_webp",
    "KHR_texture_transform",
    "EXT_mesh_gpu_instancing",
    "KHR_materials_unlit",
];

pub fn read(path: &Path) -> Result<Scene, InputError> {
    parse(&std::fs::read(path)?, path.parent())
}

/// `dir` resolves relative buffer and image URIs.
pub fn parse(bytes: &[u8], dir: Option<&Path>) -> Result<Scene, InputError> {
    let (json, bin) = split_glb(bytes)?;
    let mut doc: Value =
        serde_json::from_slice(json).map_err(|e| err(format!("invalid glTF JSON: {e}")))?;
    if !doc["asset"]["version"]
        .as_str()
        .is_some_and(|v| v.starts_with("2."))
    {
        return Err(err("not a glTF 2.0 file"));
    }
    for ext in list(&doc["extensionsRequired"])
        .iter()
        .filter_map(Value::as_str)
    {
        if !SUPPORTED.contains(&ext) {
            return Err(err(format!(
                "the file requires {ext}, which is not supported"
            )));
        }
    }
    let mut buffers = list(&doc["buffers"])
        .iter()
        .enumerate()
        .map(|(i, b)| match b["uri"].as_str() {
            None if i == 0 => bin
                .map(<[u8]>::to_vec)
                .ok_or_else(|| err("buffer 0 has no data")),
            // decoded below
            None if b["extensions"]["EXT_meshopt_compression"]["fallback"] == true => Ok(vec![]),
            None => Err(err(format!("buffer {i} has no uri"))),
            Some(uri) => load_uri(uri, dir),
        })
        .collect::<Result<Vec<_>, _>>()?;
    decode_meshopt(&mut doc, &mut buffers)?;
    let reader = Reader {
        doc: &doc,
        buffers,
        dir,
    };
    reader.scene()
}

fn err(msg: impl Into<String>) -> InputError {
    InputError(msg.into())
}

fn list(v: &Value) -> &[Value] {
    v.as_array().map(Vec::as_slice).unwrap_or(&[])
}

fn index(v: &Value) -> Option<usize> {
    v.as_u64().map(|i| i as usize)
}

/// (JSON chunk, BIN chunk) of a GLB file, or the whole file as JSON.
fn split_glb(bytes: &[u8]) -> Result<(&[u8], Option<&[u8]>), InputError> {
    if !bytes.starts_with(b"glTF") {
        return Ok((bytes, None));
    }
    let word = |at: usize| -> Result<usize, InputError> {
        bytes
            .get(at..at + 4)
            .map(|b| u32::from_le_bytes(b.try_into().unwrap()) as usize)
            .ok_or_else(|| err("truncated GLB file"))
    };
    if word(4)? != 2 {
        return Err(err("unsupported GLB version"));
    }
    let end = word(8)?.min(bytes.len());
    let (mut json, mut bin) = (None, None);
    let mut at = 12;
    while at + 8 <= end {
        let (len, kind) = (word(at)?, word(at + 4)?);
        let data = bytes
            .get(at + 8..at + 8 + len)
            .ok_or_else(|| err("truncated GLB chunk"))?;
        match kind {
            0x4E4F534A if json.is_none() => json = Some(data),
            0x004E4942 if bin.is_none() => bin = Some(data),
            _ => {}
        }
        at += 8 + len.next_multiple_of(4);
    }
    Ok((json.ok_or_else(|| err("GLB file without JSON chunk"))?, bin))
}

/// Decode the EXT_meshopt_compression views into new buffers, so their
/// accessors read like any other.
fn decode_meshopt(doc: &mut Value, buffers: &mut Vec<Vec<u8>>) -> Result<(), InputError> {
    let Some(views) = doc["bufferViews"].as_array_mut() else {
        return Ok(());
    };
    for (i, view) in views.iter_mut().enumerate() {
        let ext = &view["extensions"]["EXT_meshopt_compression"];
        if ext.is_null() {
            continue;
        }
        let bad = || err(format!("bufferView {i}: invalid meshopt data"));
        let (count, stride) = (index(&ext["count"]), index(&ext["byteStride"]));
        let (count, stride) = count.zip(stride).ok_or_else(bad)?;
        let start = index(&ext["byteOffset"]).unwrap_or(0);
        let src = index(&ext["byteLength"])
            .and_then(|len| {
                buffers
                    .get(index(&ext["buffer"])?)?
                    .get(start..start.checked_add(len)?)
            })
            .ok_or_else(bad)?;
        let data = count
            .checked_mul(stride)
            .filter(|&size| size <= 1 << 30)
            .and_then(|_| {
                let filter = ext["filter"].as_str().unwrap_or("NONE");
                meshopt_decode(src, count, stride, ext["mode"].as_str()?, filter)
            })
            .ok_or_else(bad)?;
        view["byteLength"] = json!(data.len());
        view["byteOffset"] = json!(0);
        view["buffer"] = json!(buffers.len());
        buffers.push(data);
    }
    Ok(())
}

/// `count` elements of `stride` bytes from meshoptimizer's codecs.
fn meshopt_decode(
    src: &[u8],
    count: usize,
    stride: usize,
    mode: &str,
    filter: &str,
) -> Option<Vec<u8>> {
    let mut data = match mode {
        "ATTRIBUTES" => decode_attributes(src, count, stride)?,
        "TRIANGLES" | "INDICES" if stride == 2 || stride == 4 => {
            let mut idx = vec![0u32; count];
            if mode == "INDICES" {
                decode_index_sequence(&mut idx, src).ok()?;
            } else if count % 3 == 0 {
                decode_index_buffer(&mut idx, src).ok()?;
            } else {
                return None;
            }
            if stride == 2 {
                idx.iter().flat_map(|&i| (i as u16).to_le_bytes()).collect()
            } else {
                idx.iter().flat_map(|i| i.to_le_bytes()).collect()
            }
        }
        _ => return None,
    };
    let u16s = |data: &[u8]| -> Vec<[u16; 4]> {
        data.chunks_exact(8)
            .map(|c| std::array::from_fn(|k| u16::from_le_bytes([c[2 * k], c[2 * k + 1]])))
            .collect()
    };
    let bytes16 = |v: Vec<[u16; 4]>| -> Vec<u8> {
        v.iter().flatten().flat_map(|x| x.to_le_bytes()).collect()
    };
    match (filter, stride) {
        ("NONE", _) => {}
        ("OCTAHEDRAL", 4) => {
            let mut v: Vec<[u8; 4]> = data
                .chunks_exact(4)
                .map(|c| c.try_into().unwrap())
                .collect();
            decode_filter_oct_8(&mut v);
            data = v.as_flattened().to_vec();
        }
        ("OCTAHEDRAL", 8) => {
            let mut v = u16s(&data);
            decode_filter_oct_16(&mut v);
            data = bytes16(v);
        }
        ("QUATERNION", 8) => {
            let mut v = u16s(&data);
            decode_filter_quat(&mut v);
            data = bytes16(v);
        }
        ("EXPONENTIAL", _) => {
            let mut v: Vec<u32> = data
                .chunks_exact(4)
                .map(|c| u32::from_le_bytes(c.try_into().unwrap()))
                .collect();
            decode_filter_exp(&mut v);
            data = v.iter().flat_map(|x| x.to_le_bytes()).collect();
        }
        _ => return None,
    }
    Some(data)
}

/// The vertex codec takes the element size as a type; gltfpack and
/// glTF-Transform write streams of at most 16 bytes.
fn decode_attributes(src: &[u8], count: usize, stride: usize) -> Option<Vec<u8>> {
    fn decode<const N: usize>(src: &[u8], count: usize) -> Option<Vec<u8>> {
        let mut v = vec![[0u8; N]; count];
        decode_vertex_buffer(&mut v, src).ok()?;
        Some(v.as_flattened().to_vec())
    }
    macro_rules! strides {
        ($($n:literal)*) => {
            match stride {
                $($n => decode::<$n>(src, count),)*
                _ => None,
            }
        };
    }
    strides!(4 8 12 16 20 24 28 32 36 40 44 48 52 56 60 64)
}

/// A data: URI or a file relative to the glTF file.
fn load_uri(uri: &str, dir: Option<&Path>) -> Result<Vec<u8>, InputError> {
    if let Some(rest) = uri.strip_prefix("data:") {
        let (head, data) = rest
            .split_once(',')
            .ok_or_else(|| err("invalid data URI"))?;
        if !head.ends_with(";base64") {
            return Err(err("data URI is not base64"));
        }
        return base64_decode(data).ok_or_else(|| err("invalid base64 in data URI"));
    }
    let name = percent_decode(uri);
    let path = dir.map_or_else(|| Path::new(&name).to_path_buf(), |d| d.join(&name));
    std::fs::read(&path).map_err(|e| err(format!("cannot read {}: {e}", path.display())))
}

fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = bytes
            .get(i + 1..i + 3)
            .and_then(|h| std::str::from_utf8(h).ok())
            .and_then(|h| u8::from_str_radix(h, 16).ok());
        match (bytes[i], hex) {
            (b'%', Some(b)) => {
                out.push(b);
                i += 3;
            }
            (c, _) => {
                out.push(c);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

struct Reader<'a> {
    doc: &'a Value,
    buffers: Vec<Vec<u8>>,
    dir: Option<&'a Path>,
}

impl Reader<'_> {
    fn scene(&self) -> Result<Scene, InputError> {
        let mut scene = Scene {
            y_up: true,
            meters_per_unit: Some(1.0),
            // the default material is single-sided
            default_double_sided: Some(false),
            ..Scene::default()
        };
        scene.materials = list(&self.doc["materials"])
            .iter()
            .enumerate()
            .map(|(i, m)| self.material(i, m, &mut scene))
            .collect::<Result<_, _>>()?;
        for (i, m) in list(&self.doc["meshes"]).iter().enumerate() {
            let mesh = self.mesh(i, m, &mut scene)?;
            scene.meshes.push(mesh);
        }
        self.nodes(&mut scene)?;
        Ok(scene)
    }

    // -- accessors -----------------------------------------------------------
    /// An accessor's elements as f64 (normalized integers mapped to 0..1 or
    /// -1..1), `count * components` values.
    fn accessor(&self, i: usize) -> Result<(Vec<f64>, usize), InputError> {
        let a = &self.doc["accessors"][i];
        let count = index(&a["count"]).ok_or_else(|| err(format!("accessor {i}: no count")))?;
        let ncomp = match a["type"].as_str() {
            Some("SCALAR") => 1,
            Some("VEC2") => 2,
            Some("VEC3") => 3,
            Some("VEC4") | Some("MAT2") => 4,
            Some("MAT3") => 9,
            Some("MAT4") => 16,
            _ => return Err(err(format!("accessor {i}: invalid type"))),
        };
        let ctype = a["componentType"].as_u64().unwrap_or(0);
        let normalized = a["normalized"].as_bool().unwrap_or(false);
        let mut out = vec![0.0; count * ncomp];
        if let Some(view) = index(&a["bufferView"]) {
            let offset = index(&a["byteOffset"]).unwrap_or(0);
            self.read_view(view, offset, count, ncomp, ctype, normalized, &mut out)
                .ok_or_else(|| err(format!("accessor {i}: out of bounds")))?;
        }
        let sparse = &a["sparse"];
        if let Some(n) = index(&sparse["count"]) {
            let (si, sv) = (&sparse["indices"], &sparse["values"]);
            let mut ids = vec![0.0; n];
            let mut vals = vec![0.0; n * ncomp];
            let ok = index(&si["bufferView"])
                .and_then(|v| {
                    let off = index(&si["byteOffset"]).unwrap_or(0);
                    let ct = si["componentType"].as_u64().unwrap_or(0);
                    self.read_view(v, off, n, 1, ct, false, &mut ids)
                })
                .and_then(|()| {
                    let off = index(&sv["byteOffset"]).unwrap_or(0);
                    self.read_view(
                        index(&sv["bufferView"])?,
                        off,
                        n,
                        ncomp,
                        ctype,
                        normalized,
                        &mut vals,
                    )
                })
                .is_some();
            if !ok {
                return Err(err(format!("accessor {i}: invalid sparse data")));
            }
            for (k, &id) in ids.iter().enumerate() {
                let id = id as usize;
                if id >= count {
                    return Err(err(format!("accessor {i}: sparse index out of range")));
                }
                out[id * ncomp..(id + 1) * ncomp]
                    .copy_from_slice(&vals[k * ncomp..(k + 1) * ncomp]);
            }
        }
        Ok((out, ncomp))
    }

    #[allow(clippy::too_many_arguments)]
    fn read_view(
        &self,
        view: usize,
        offset: usize,
        count: usize,
        ncomp: usize,
        ctype: u64,
        normalized: bool,
        out: &mut [f64],
    ) -> Option<()> {
        let v = &self.doc["bufferViews"][view];
        let buf = self.buffers.get(index(&v["buffer"])?)?;
        let start = index(&v["byteOffset"]).unwrap_or(0);
        let len = index(&v["byteLength"])?;
        let data = buf.get(start..start.checked_add(len)?)?;
        let csize = match ctype {
            5120 | 5121 => 1,
            5122 | 5123 => 2,
            5125 | 5126 => 4,
            _ => return None,
        };
        let stride = index(&v["byteStride"])
            .unwrap_or(csize * ncomp)
            .max(csize * ncomp);
        if count > 0 && offset + (count - 1) * stride + csize * ncomp > data.len() {
            return None;
        }
        for k in 0..count {
            for c in 0..ncomp {
                let at = offset + k * stride + c * csize;
                let b = &data[at..at + csize];
                out[k * ncomp + c] = match (ctype, normalized) {
                    (5126, _) => f32::from_le_bytes(b.try_into().ok()?) as f64,
                    (5120, false) => b[0] as i8 as f64,
                    (5120, true) => (b[0] as i8 as f64 / 127.0).max(-1.0),
                    (5121, false) => b[0] as f64,
                    (5121, true) => b[0] as f64 / 255.0,
                    (5122, false) => i16::from_le_bytes([b[0], b[1]]) as f64,
                    (5122, true) => (i16::from_le_bytes([b[0], b[1]]) as f64 / 32767.0).max(-1.0),
                    (5123, false) => u16::from_le_bytes([b[0], b[1]]) as f64,
                    (5123, true) => u16::from_le_bytes([b[0], b[1]]) as f64 / 65535.0,
                    _ => u32::from_le_bytes(b.try_into().ok()?) as f64,
                };
            }
        }
        Some(())
    }

    fn vec3s(&self, i: usize) -> Result<Vec<Vec3>, InputError> {
        let (v, n) = self.accessor(i)?;
        if n != 3 {
            return Err(err(format!("accessor {i}: expected VEC3")));
        }
        Ok(v.chunks_exact(3).map(|c| [c[0], c[1], c[2]]).collect())
    }

    // -- meshes --------------------------------------------------------------
    fn mesh(&self, i: usize, m: &Value, scene: &mut Scene) -> Result<Mesh, InputError> {
        let mut mesh = Mesh {
            name: m["name"]
                .as_str()
                .map_or_else(|| format!("mesh_{i}"), str::to_string),
            primitives: vec![],
        };
        let weights: Vec<f64> = list(&m["weights"])
            .iter()
            .filter_map(Value::as_f64)
            .collect();
        for p in list(&m["primitives"]) {
            if let Some(prim) = self.primitive(p, &weights, scene)? {
                mesh.primitives.push(prim);
            }
        }
        Ok(mesh)
    }

    /// `weights`: the mesh's default morph target weights.
    fn primitive(
        &self,
        p: &Value,
        weights: &[f64],
        scene: &mut Scene,
    ) -> Result<Option<Primitive>, InputError> {
        let attrs = &p["attributes"];
        let Some(pos) = index(&attrs["POSITION"]) else {
            scene.skip("primitive without POSITION", 1);
            return Ok(None);
        };
        if !p["extensions"]["KHR_draco_mesh_compression"].is_null()
            && self.doc["accessors"][pos]["bufferView"].is_null()
        {
            return Err(err(
                "compressed glTF (KHR_draco_mesh_compression) is not supported",
            ));
        }
        let mut positions = self.vec3s(pos)?;
        let n = positions.len();
        let mut normals = match index(&attrs["NORMAL"]) {
            Some(a) => Some(self.vec3s(a)?).filter(|v| v.len() == n),
            None => None,
        };
        // morph targets at the mesh's default weights
        for (t, target) in list(&p["targets"]).iter().enumerate() {
            let w = weights.get(t).copied().unwrap_or(0.0);
            if w == 0.0 {
                continue;
            }
            if let Some(a) = index(&target["POSITION"]) {
                let d = self.vec3s(a)?;
                for (p, d) in positions.iter_mut().zip(&d) {
                    *p = add(*p, mul(*d, w));
                }
            }
            if let (Some(a), Some(ns)) = (index(&target["NORMAL"]), normals.as_mut()) {
                let d = self.vec3s(a)?;
                for (n, d) in ns.iter_mut().zip(&d) {
                    *n = add(*n, mul(*d, w));
                }
            }
        }
        let material = index(&p["material"]);
        // the texture coordinate set the base color texture uses, and its
        // transform
        let tex =
            material.map(|m| &self.doc["materials"][m]["pbrMetallicRoughness"]["baseColorTexture"]);
        let transform = tex.map(|t| &t["extensions"]["KHR_texture_transform"]);
        let set = transform
            .and_then(|x| x["texCoord"].as_u64())
            .or_else(|| tex.and_then(|t| t["texCoord"].as_u64()))
            .unwrap_or(0);
        let uvs = match index(&attrs[format!("TEXCOORD_{set}")]) {
            Some(a) => {
                let (v, c) = self.accessor(a)?;
                let m = transform.map_or([1.0, 0.0, 0.0, 0.0, 1.0, 0.0], uv_transform);
                (c == 2)
                    .then(|| {
                        v.chunks_exact(2)
                            .map(|c| {
                                [
                                    m[0] * c[0] + m[1] * c[1] + m[2],
                                    m[3] * c[0] + m[4] * c[1] + m[5],
                                ]
                            })
                            .collect::<Vec<Vec2>>()
                    })
                    .filter(|v| v.len() == n)
            }
            None => None,
        };
        if !attrs["COLOR_0"].is_null() {
            scene.skip("vertex colors (ignored)", 1);
        }
        let indices: Vec<u32> = match index(&p["indices"]) {
            Some(a) => {
                let (v, _) = self.accessor(a)?;
                v.into_iter().map(|x| x as u32).collect()
            }
            None => (0..n as u32).collect(),
        };
        let mut prim = Primitive {
            positions,
            normals,
            uvs,
            material,
            ..Primitive::default()
        };
        let mut faces = vec![];
        let tri = |faces: &mut Vec<u32>, a: u32, b: u32, c: u32| faces.extend([3, a, b, c]);
        match p["mode"].as_u64().unwrap_or(4) {
            4 => indices
                .chunks_exact(3)
                .for_each(|t| tri(&mut faces, t[0], t[1], t[2])),
            5 => {
                for k in 2..indices.len() {
                    let (a, b, c) = (indices[k - 2], indices[k - 1], indices[k]);
                    if k % 2 == 0 {
                        tri(&mut faces, a, b, c);
                    } else {
                        tri(&mut faces, b, a, c);
                    }
                }
            }
            6 => {
                for k in 2..indices.len() {
                    tri(&mut faces, indices[0], indices[k - 1], indices[k]);
                }
            }
            1 => prim.lines = indices.chunks_exact(2).map(|l| [l[0], l[1]]).collect(),
            mode @ (2 | 3) => {
                prim.lines = indices.windows(2).map(|l| [l[0], l[1]]).collect();
                if mode == 2 && indices.len() > 2 {
                    prim.lines.push([indices[indices.len() - 1], indices[0]]);
                }
            }
            _ => {
                scene.skip("points", 1);
                return Ok(None);
            }
        }
        let (faces, dropped) = clean_faces(&faces, n);
        scene.skip("degenerate faces", dropped);
        let before = prim.lines.len();
        prim.lines
            .retain(|&[a, b]| a != b && (a as usize) < n && (b as usize) < n);
        scene.skip("degenerate lines", before - prim.lines.len());
        prim.faces = faces;
        // glTF: without normals, faces are flat
        flat_normals(&mut prim);
        Ok(Some(prim))
    }

    // -- materials -----------------------------------------------------------
    fn material(&self, i: usize, m: &Value, scene: &mut Scene) -> Result<Material, InputError> {
        let pbr = &m["pbrMetallicRoughness"];
        let factor: Vec<f64> = list(&pbr["baseColorFactor"])
            .iter()
            .filter_map(Value::as_f64)
            .collect();
        let factor = if factor.len() == 4 {
            factor
        } else {
            vec![1.0; 4]
        };
        let tex = &pbr["baseColorTexture"];
        let texture = match index(&tex["index"]) {
            Some(t) => {
                let tex = self.texture(t)?;
                if tex.is_none() {
                    scene.skip("texture (not PNG, JPEG or WebP)", 1);
                }
                tex
            }
            None => None,
        };
        Ok(Material {
            name: m["name"]
                .as_str()
                .map_or_else(|| format!("material_{i}"), str::to_string),
            color: [0, 1, 2].map(|k| linear_to_srgb(factor[k].clamp(0.0, 1.0))),
            alpha: factor[3].clamp(0.0, 1.0),
            metallic: pbr["metallicFactor"]
                .as_f64()
                .unwrap_or(1.0)
                .clamp(0.0, 1.0),
            roughness: pbr["roughnessFactor"]
                .as_f64()
                .unwrap_or(1.0)
                .clamp(0.0, 1.0),
            double_sided: m["doubleSided"].as_bool().unwrap_or(false),
            texture,
        })
    }

    fn texture(&self, t: usize) -> Result<Option<Texture>, InputError> {
        let tex = &self.doc["textures"][t];
        let source = index(&tex["source"])
            .or_else(|| index(&tex["extensions"]["EXT_texture_webp"]["source"]));
        let Some(source) = source else {
            return Ok(None); // only another extension's source (KTX2)
        };
        let img = &self.doc["images"][source];
        let data = match (img["uri"].as_str(), index(&img["bufferView"])) {
            (Some(uri), _) => load_uri(uri, self.dir)?,
            (None, Some(v)) => {
                let view = &self.doc["bufferViews"][v];
                let slice = index(&view["buffer"])
                    .and_then(|b| self.buffers.get(b))
                    .and_then(|buf| {
                        let start = index(&view["byteOffset"]).unwrap_or(0);
                        buf.get(start..start + index(&view["byteLength"])?)
                    });
                slice
                    .ok_or_else(|| err(format!("image {source}: out of bounds")))?
                    .to_vec()
            }
            _ => return Ok(None),
        };
        Ok(Texture::from_bytes(data))
    }

    // -- nodes ---------------------------------------------------------------
    fn nodes(&self, scene: &mut Scene) -> Result<(), InputError> {
        let nodes = list(&self.doc["nodes"]);
        if nodes.is_empty() {
            // no hierarchy: every mesh once, untransformed
            for (i, m) in scene.meshes.iter().enumerate() {
                scene.nodes.push(Node {
                    name: m.name.clone(),
                    matrix: IDENTITY,
                    mesh: i,
                });
            }
            return Ok(());
        }
        let scene_index = index(&self.doc["scene"]).unwrap_or(0);
        let roots: Vec<usize> = match self.doc["scenes"][scene_index]["nodes"].as_array() {
            Some(r) => r.iter().filter_map(index).collect(),
            None => {
                // no scene: every node that is nobody's child
                let children: Vec<usize> = nodes
                    .iter()
                    .flat_map(|n| list(&n["children"]).iter().filter_map(index))
                    .collect();
                (0..nodes.len()).filter(|i| !children.contains(i)).collect()
            }
        };
        let mut stack: Vec<(usize, Mat, usize)> =
            roots.into_iter().rev().map(|r| (r, IDENTITY, 0)).collect();
        while let Some((i, parent, depth)) = stack.pop() {
            let node = nodes
                .get(i)
                .ok_or_else(|| err(format!("node {i} does not exist")))?;
            if depth > MAX_DEPTH {
                return Err(err("node hierarchy too deep (cycle?)"));
            }
            let world = mat_mul(&parent, &local_matrix(node));
            if let Some(mesh) = index(&node["mesh"]).filter(|&m| m < scene.meshes.len()) {
                let name = node["name"]
                    .as_str()
                    .map_or_else(|| scene.meshes[mesh].name.clone(), str::to_string);
                let instancing = &node["extensions"]["EXT_mesh_gpu_instancing"];
                let instances = if instancing.is_null() {
                    vec![IDENTITY]
                } else {
                    self.instances(&instancing["attributes"])?
                };
                for m in instances {
                    scene.nodes.push(Node {
                        name: name.clone(),
                        matrix: mat_mul(&world, &m),
                        mesh,
                    });
                }
                if !list(&node["weights"]).is_empty() {
                    scene.skip("node morph weights (the mesh's are used)", 1);
                }
            }
            if !node["skin"].is_null() {
                scene.skip("skin (ignored)", 1);
            }
            for c in list(&node["children"]).iter().rev().filter_map(index) {
                stack.push((c, world, depth + 1));
            }
        }
        Ok(())
    }
}

impl Reader<'_> {
    /// EXT_mesh_gpu_instancing: one matrix per instance (T * R * S).
    fn instances(&self, attrs: &Value) -> Result<Vec<Mat>, InputError> {
        let read = |key: &str, n: usize| -> Result<Option<Vec<Vec<f64>>>, InputError> {
            match index(&attrs[key]) {
                Some(a) => {
                    let (v, c) = self.accessor(a)?;
                    if c != n {
                        return Err(err(format!("instancing {key}: wrong type")));
                    }
                    Ok(Some(v.chunks_exact(n).map(<[f64]>::to_vec).collect()))
                }
                None => Ok(None),
            }
        };
        let (t, r, s) = (
            read("TRANSLATION", 3)?,
            read("ROTATION", 4)?,
            read("SCALE", 3)?,
        );
        let count = [&t, &r, &s]
            .iter()
            .filter_map(|x| x.as_ref().map(Vec::len))
            .max()
            .unwrap_or(0);
        Ok((0..count)
            .map(|i| {
                let tr = t
                    .as_ref()
                    .and_then(|v| v.get(i))
                    .map_or([0.0; 3], |v| [v[0], v[1], v[2]]);
                let rot = r
                    .as_ref()
                    .and_then(|v| v.get(i))
                    .map_or(IDENTITY, |q| quaternion(q[0], q[1], q[2], q[3]));
                let sc = s
                    .as_ref()
                    .and_then(|v| v.get(i))
                    .map_or([1.0; 3], |v| [v[0], v[1], v[2]]);
                mat_mul(&mat_mul(&translate(tr), &rot), &scale(sc))
            })
            .collect())
    }
}

/// KHR_texture_transform as a 2x3 matrix on (u, v): translation * rotation
/// * scale, the rotation counter-clockwise in the image.
fn uv_transform(x: &Value) -> [f64; 6] {
    let pair = |v: &Value, d: [f64; 2]| {
        let a: Vec<f64> = list(v).iter().filter_map(Value::as_f64).collect();
        if a.len() == 2 { [a[0], a[1]] } else { d }
    };
    let [tx, ty] = pair(&x["offset"], [0.0, 0.0]);
    let [sx, sy] = pair(&x["scale"], [1.0, 1.0]);
    let (sin, cos) = x["rotation"].as_f64().unwrap_or(0.0).sin_cos();
    [cos * sx, sin * sy, tx, -sin * sx, cos * sy, ty]
}

/// A node's local transform, row-major.
fn local_matrix(node: &Value) -> Mat {
    let nums = |v: &Value, n: usize| -> Option<Vec<f64>> {
        let v: Vec<f64> = list(v).iter().filter_map(Value::as_f64).collect();
        (v.len() == n).then_some(v)
    };
    if let Some(m) = nums(&node["matrix"], 16) {
        // column-major in glTF
        let mut out = [0.0; 16];
        for r in 0..4 {
            for c in 0..4 {
                out[r * 4 + c] = m[c * 4 + r];
            }
        }
        return out;
    }
    let t = nums(&node["translation"], 3).map_or([0.0; 3], |v| [v[0], v[1], v[2]]);
    let s = nums(&node["scale"], 3).map_or([1.0; 3], |v| [v[0], v[1], v[2]]);
    let r = match nums(&node["rotation"], 4) {
        Some(q) => quaternion(q[0], q[1], q[2], q[3]),
        None => IDENTITY,
    };
    mat_mul(&mat_mul(&translate(t), &r), &scale(s))
}

/// Rotation matrix of the unit quaternion (x, y, z, w).
fn quaternion(x: f64, y: f64, z: f64, w: f64) -> Mat {
    let n = (x * x + y * y + z * z + w * w).sqrt();
    let (x, y, z, w) = if n > 0.0 {
        (x / n, y / n, z / n, w / n)
    } else {
        (0.0, 0.0, 0.0, 1.0)
    };
    [
        1.0 - 2.0 * (y * y + z * z),
        2.0 * (x * y - z * w),
        2.0 * (x * z + y * w),
        0.0,
        2.0 * (x * y + z * w),
        1.0 - 2.0 * (x * x + z * z),
        2.0 * (y * z - x * w),
        0.0,
        2.0 * (x * z - y * w),
        2.0 * (y * z + x * w),
        1.0 - 2.0 * (x * x + y * y),
        0.0,
        0.0,
        0.0,
        0.0,
        1.0,
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gltf::base64;

    fn f32s(v: &[f32]) -> Vec<u8> {
        v.iter().flat_map(|f| f.to_le_bytes()).collect()
    }

    /// A quad as a triangle strip with interleaved positions and normalized
    /// u16 UVs, one node rotated 90 degrees about Y and translated.
    fn strip_gltf() -> String {
        let mut bin = vec![];
        for (p, uv) in [
            ([0.0f32, 0.0, 0.0], [0u16, 65535]),
            ([1.0, 0.0, 0.0], [65535, 65535]),
            ([0.0, 1.0, 0.0], [0, 0]),
            ([1.0, 1.0, 0.0], [65535, 0]),
        ] {
            bin.extend(f32s(&p));
            bin.extend(uv[0].to_le_bytes());
            bin.extend(uv[1].to_le_bytes());
        }
        let h = std::f64::consts::FRAC_1_SQRT_2;
        serde_json::json!({
            "asset": {"version": "2.0"},
            "scene": 0,
            "scenes": [{"nodes": [0]}],
            "nodes": [{"name": "quad", "mesh": 0, "translation": [5, 0, 0], "rotation": [0, h, 0, h]}],
            "meshes": [{"name": "m", "primitives": [{
                "attributes": {"POSITION": 0, "TEXCOORD_0": 1}, "mode": 5, "material": 0}]}],
            "materials": [{"name": "red", "doubleSided": true,
                "pbrMetallicRoughness": {"baseColorFactor": [1, 0, 0, 0.5], "metallicFactor": 0}}],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3"},
                {"bufferView": 0, "byteOffset": 12, "componentType": 5123, "normalized": true,
                 "count": 4, "type": "VEC2"}],
            "bufferViews": [{"buffer": 0, "byteLength": bin.len(), "byteStride": 16}],
            "buffers": [{"byteLength": bin.len(),
                "uri": format!("data:application/octet-stream;base64,{}", base64(&bin))}]
        })
        .to_string()
    }

    /// The faces' corners as indices of the first vertex at each position.
    fn corners(prim: &Primitive) -> Vec<Vec<usize>> {
        let first = |v: u32| {
            let p = prim.positions[v as usize];
            prim.positions.iter().position(|&q| q == p).unwrap()
        };
        let mut out = vec![];
        let mut i = 0;
        while i < prim.faces.len() {
            let n = prim.faces[i] as usize;
            out.push(
                prim.faces[i + 1..i + 1 + n]
                    .iter()
                    .map(|&v| first(v))
                    .collect(),
            );
            i += n + 1;
        }
        out
    }

    #[test]
    fn instancing_texture_transform_and_morph_weights() {
        let pos = f32s(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 1.0, 0.0]);
        let uv = f32s(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
        let delta = f32s(&[0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0]);
        let inst = f32s(&[0.0, 0.0, 0.0, 10.0, 0.0, 0.0, 20.0, 0.0, 0.0]);
        let mut bin = vec![];
        for b in [&pos, &uv, &delta, &inst] {
            bin.extend(b);
        }
        let views: Vec<serde_json::Value> = [(0, 36), (36, 24), (60, 36), (96, 36)]
            .iter()
            .map(|&(o, l)| serde_json::json!({"buffer": 0, "byteOffset": o, "byteLength": l}))
            .collect();
        let doc = serde_json::json!({
            "asset": {"version": "2.0"},
            "extensionsRequired": ["EXT_mesh_gpu_instancing", "KHR_texture_transform"],
            "scenes": [{"nodes": [0]}],
            "nodes": [{"mesh": 0, "extensions": {"EXT_mesh_gpu_instancing": {
                "attributes": {"TRANSLATION": 3}}}}],
            "meshes": [{"weights": [0.5], "primitives": [{
                "attributes": {"POSITION": 0, "TEXCOORD_0": 1}, "material": 0,
                "targets": [{"POSITION": 2}]}]}],
            "materials": [{"pbrMetallicRoughness": {"baseColorTexture": {"index": 0,
                "extensions": {"KHR_texture_transform": {"offset": [0.5, 0], "scale": [2, 2]}}}}}],
            "textures": [{}],
            "accessors": [
                {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3"},
                {"bufferView": 1, "componentType": 5126, "count": 3, "type": "VEC2"},
                {"bufferView": 2, "componentType": 5126, "count": 3, "type": "VEC3"},
                {"bufferView": 3, "componentType": 5126, "count": 3, "type": "VEC3"}],
            "bufferViews": views,
            "buffers": [{"byteLength": bin.len(),
                "uri": format!("data:application/octet-stream;base64,{}", base64(&bin))}]
        });
        let scene = parse(doc.to_string().as_bytes(), None).unwrap();
        assert_eq!(scene.nodes.len(), 3);
        assert_eq!(
            crate::geom::apply(&scene.nodes[2].matrix, [0.0; 3]),
            [20.0, 0.0, 0.0]
        );
        let prim = &scene.meshes[0].primitives[0];
        // moved half way to the morph target
        assert_eq!(prim.positions[1], [1.0, 0.0, 0.5]);
        // scaled by 2, then moved by 0.5 in u
        assert_eq!(prim.uvs.as_ref().unwrap()[1], [2.5, 0.0]);

        let unknown = serde_json::json!({"asset": {"version": "2.0"},
            "extensionsRequired": ["KHR_texture_basisu"]});
        assert!(parse(unknown.to_string().as_bytes(), None).is_err());
    }

    #[test]
    fn strip_with_strided_normalized_uvs() {
        let scene = parse(strip_gltf().as_bytes(), None).unwrap();
        let prim = &scene.meshes[0].primitives[0];
        // strip vertices 0 1 2 and 2 1 3 (odd triangles swap), each face
        // with its own vertices for flat normals
        assert_eq!(corners(prim), vec![vec![0, 1, 2], vec![2, 1, 3]]);
        assert_eq!(prim.positions[3], [1.0, 1.0, 0.0]);
        assert_eq!(prim.uvs.as_ref().unwrap()[0], [0.0, 1.0]);
        assert_eq!(
            prim.normals.as_ref().unwrap()[prim.faces[1] as usize],
            [0.0, 0.0, 1.0]
        );
        let mat = &scene.materials[0];
        assert!((mat.color[0] - 1.0).abs() < 1e-12 && mat.color[1] == 0.0);
        assert_eq!((mat.alpha, mat.metallic, mat.roughness), (0.5, 0.0, 1.0));
        assert!(mat.double_sided);
        let m = scene.nodes[0].matrix;
        // +X turns into -Z, then moves by 5 along X
        let p = crate::geom::apply(&m, [1.0, 0.0, 0.0]);
        assert!(
            (p[0] - 5.0).abs() < 1e-12 && (p[2] + 1.0).abs() < 1e-12,
            "{p:?}"
        );
    }

    #[test]
    fn glb_container() {
        let json = strip_gltf().into_bytes();
        let mut glb = b"glTF".to_vec();
        glb.extend(2u32.to_le_bytes());
        let pad = json.len().next_multiple_of(4) - json.len();
        glb.extend(((20 + json.len() + pad) as u32).to_le_bytes());
        glb.extend(((json.len() + pad) as u32).to_le_bytes());
        glb.extend(0x4E4F534Au32.to_le_bytes());
        glb.extend(&json);
        glb.extend(std::iter::repeat_n(b' ', pad));
        let scene = parse(&glb, None).unwrap();
        assert_eq!(scene.meshes[0].primitives[0].faces.len(), 8);
    }

    #[test]
    fn fan_lines_and_draco() {
        let fan = serde_json::json!({
            "asset": {"version": "2.0"},
            "meshes": [{"primitives": [
                {"attributes": {"POSITION": 0}, "mode": 6},
                {"attributes": {"POSITION": 0}, "mode": 2}]}],
            "accessors": [{"bufferView": 0, "componentType": 5126, "count": 4, "type": "VEC3"}],
            "bufferViews": [{"buffer": 0, "byteLength": 48}],
            "buffers": [{"byteLength": 48, "uri": format!("data:application/octet-stream;base64,{}",
                base64(&f32s(&[0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 1.0, 1.0, 0.0, 0.0, 1.0, 0.0])))}]
        });
        let scene = parse(fan.to_string().as_bytes(), None).unwrap();
        assert_eq!(
            corners(&scene.meshes[0].primitives[0]),
            vec![vec![0, 1, 2], vec![0, 2, 3]]
        );
        assert_eq!(
            scene.meshes[0].primitives[1].lines,
            vec![[0, 1], [1, 2], [2, 3], [3, 0]]
        );
        assert_eq!(scene.nodes.len(), 1); // no nodes: the mesh once

        let draco = serde_json::json!({"asset": {"version": "2.0"},
            "extensionsRequired": ["KHR_draco_mesh_compression"]});
        assert!(parse(draco.to_string().as_bytes(), None).is_err());
    }
}
