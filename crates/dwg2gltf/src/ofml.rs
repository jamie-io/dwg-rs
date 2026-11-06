// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! The OFML extension data some DWG exporters write (the `EDWG_*` records and
//! embedded material archives), shared by the reader and the writer; see
//! docs/IMPLEMENTATION_NOTES.md.

use std::io::{Cursor, Write};

use zip::ZipWriter;
use zip::write::SimpleFileOptions;

/// Named object dictionary entry holding [`MATERIAL_DICT`].
pub const PXCAD_DICT: &str = "PXCAD_DICT";
/// Dictionary of material archives (XRECORDs of 310 chunks), keyed by MATERIAL name.
pub const MATERIAL_DICT: &str = "EDWG_MATERIAL_DICT";
/// MATERIAL extension record pointing to the base material of a derived one.
pub const BASE_MATERIAL: &str = "EDWG_BASE_MATERIAL";
/// MESH extension record: per-vertex u/v doubles in 310 chunks.
pub const TEX_COORDS: &str = "EDWG_TEX_COORDS_2D";
/// MESH extension record: 290 bool, 0 = single-sided (absent = double-sided).
pub const DOUBLE_SIDED: &str = "EDWG_DOUBLE_SIDED_FACES";
/// AutoCAD's MESH vertex normals: 40/41/42 per vertex (used at level 0 only).
pub const VERTEX_NORMALS: &str = "ADSK_XREC_SUBDVERTEXNORMALS";
/// MESH extension record: 50, the smoothing angle in degrees for a MESH
/// without vertex normals (53 when the record is absent).
pub const NORMAL_SMOOTHNESS: &str = "EDWG_NORMAL_SMOOTHNESS";
/// INSERT extension record: 300 text such as "geoscale 1.2 1.0 1.0", the
/// OFML geometry scale the INSERT applies. OFML maps textures on the scaled
/// geometry, so projected UVs use block coordinates times this scale.
pub const MAPPING_INFO: &str = "EDWG_OFML_MAPPING_INFO";

/// The scale in an [`MAPPING_INFO`] text, if any (non-zero, finite).
pub fn geoscale(text: &str) -> Option<[f64; 3]> {
    text.lines().find_map(|line| {
        let mut words = line.split_whitespace();
        if words.next()? != "geoscale" {
            return None;
        }
        let v: Vec<f64> = words
            .take(3)
            .map(|w| w.parse().ok())
            .collect::<Option<_>>()?;
        let s: [f64; 3] = v.try_into().ok()?;
        s.iter().all(|c| c.is_finite() && *c != 0.0).then_some(s)
    })
}

/// Binary chunk sizes as ODA writes them (the importer accepts any size; UV
/// chunks must hold whole u/v pairs, and LibreDWG allows at most 255 bytes).
pub const ARCHIVE_CHUNK: u32 = 127;
pub const UV_CHUNK: u32 = 112;

/// What goes into a material archive.
pub struct ArchiveMaterial<'a> {
    /// Package id, written to id.txt as `::gltf2dwg::<id>_<content hash>`
    /// (the hash keeps equally named materials of different files apart).
    /// OFML importers only apply a texture mapping when the id starts with "::".
    pub id: &'a str,
    /// sRGB 0..1
    pub diffuse: [f64; 3],
    pub metallic: f64,
    pub roughness: f64,
    /// Texture image and its file extension ("png" or "jpg").
    pub texture: Option<(&'a [u8], &'a str)>,
}

/// File stem of the texture inside a package.
const TEXTURE_STEM: &str = "diffuse";

/// An embedded OFML material archive: a zip with id.txt, material.mat and the texture. The
/// texture uses the UVs stored on the mesh (`import` mapping).
pub fn material_archive(m: &ArchiveMaterial) -> Vec<u8> {
    let mut spec = format!(
        "type common\ndif {:.6} {:.6} {:.6}\nmetallic {:.6}\nroughness {:.6}\n",
        m.diffuse[0], m.diffuse[1], m.diffuse[2], m.metallic, m.roughness
    );
    if let Some((_, ext)) = m.texture {
        spec += &format!("tex image {ext} {TEXTURE_STEM}\nimport\n");
    }
    let mut zip = ZipWriter::new(Cursor::new(Vec::new()));
    let options = SimpleFileOptions::default();
    let mut add = |name: &str, data: &[u8]| {
        zip.start_file(name, options)
            .and_then(|()| zip.write_all(data).map_err(Into::into))
            .expect("writing to memory");
    };
    let mut hash = Fnv::default();
    hash.write(spec.as_bytes());
    if let Some((data, _)) = m.texture {
        hash.write(data);
    }
    add(
        "id.txt",
        format!("::gltf2dwg::{}_{:016x}", m.id, hash.0).as_bytes(),
    );
    add("material.mat", spec.as_bytes());
    if let Some((data, ext)) = m.texture {
        add(&format!("{TEXTURE_STEM}.{ext}"), data);
    }
    zip.finish().expect("writing to memory").into_inner()
}

/// FNV-1a, 64 bit.
struct Fnv(u64);

impl Default for Fnv {
    fn default() -> Self {
        Fnv(0xcbf2_9ce4_8422_2325)
    }
}

impl Fnv {
    fn write(&mut self, data: &[u8]) {
        for &b in data {
            self.0 = (self.0 ^ b as u64).wrapping_mul(0x0100_0000_01b3);
        }
    }
}
