// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Build a DWG file with the bundled LibreDWG encoder.
//!
//! Next to `dwg` (reading), this is the only module that touches LibreDWG.
//! Everything else talks to [`DwgBuilder`] with plain Rust data and handles.

use std::ffi::{CString, c_char, c_short, c_void};
use std::sync::MutexGuard;
use std::{fmt, ptr, slice};

use libredwg_sys as sys;

use crate::dwg::Handle;
use crate::geom::{Vec2, Vec3};
use crate::ofml;

/// The DWG versions with MESH and MATERIAL that LibreDWG writes (r2007
/// output is not supported upstream).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DwgVersion {
    R2010,
    R2013,
    R2018,
}

impl DwgVersion {
    pub const NAMES: [(&'static str, DwgVersion); 3] = [
        ("r2010", DwgVersion::R2010),
        ("r2013", DwgVersion::R2013),
        ("r2018", DwgVersion::R2018),
    ];

    fn sys(self) -> sys::Dwg_Version_Type {
        match self {
            DwgVersion::R2010 => sys::DWG_VERSION_TYPE_R_2010,
            DwgVersion::R2013 => sys::DWG_VERSION_TYPE_R_2013,
            DwgVersion::R2018 => sys::DWG_VERSION_TYPE_R_2018,
        }
    }
}

#[derive(Debug)]
pub struct WriteError(pub String);

impl fmt::Display for WriteError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str(&self.0)
    }
}

type Result<T> = std::result::Result<T, WriteError>;

fn fail<T>(what: &str) -> Result<T> {
    Err(WriteError(format!("LibreDWG could not add {what}")))
}

fn cstr(s: &str) -> Result<CString> {
    CString::new(s).map_err(|_| WriteError(format!("name contains a NUL byte: {s:?}")))
}

fn point(p: Vec3) -> sys::dwg_point_3d {
    sys::dwg_point_3d {
        x: p[0],
        y: p[1],
        z: p[2],
    }
}

/// Where entities go: model space or a block definition.
#[derive(Clone, Copy)]
pub struct Block(*mut sys::Dwg_Object_BLOCK_HEADER);

/// Layer and MATERIAL of an entity (both by handle).
#[derive(Clone, Copy)]
pub struct Style {
    pub layer: Handle,
    pub material: Option<Handle>,
}

/// A MESH entity with the OFML extension records.
pub struct MeshData<'a> {
    pub vertices: &'a [Vec3],
    /// Polygons: vertex count, then the indices.
    pub faces: &'a [u32],
    pub normals: Option<&'a [Vec3]>,
    /// DWG convention: v grows upwards.
    pub uvs: Option<&'a [Vec2]>,
    /// Written as EDWG_DOUBLE_SIDED_FACES when set.
    pub double_sided: Option<bool>,
}

pub struct DwgBuilder {
    dwg: *mut sys::Dwg_Data,
    /// NOD/PXCAD_DICT/EDWG_MATERIAL_DICT, created on first use.
    archive_dict: *mut sys::Dwg_Object_DICTIONARY,
    /// LibreDWG is not reentrant; held until drop has freed `dwg`.
    _lock: MutexGuard<'static, ()>,
}

impl Drop for DwgBuilder {
    fn drop(&mut self) {
        // SAFETY: dwg came from dwg2gltf_new (calloc) and is freed once
        unsafe {
            sys::dwg_free(self.dwg);
            sys::dwg2gltf_free(self.dwg as *mut c_void);
        }
    }
}

// All pointers below are LibreDWG objects owned by self.dwg. Object structs
// (Dwg_Object_*, Dwg_Entity_*) stay where they are; only the Dwg_Object array
// moves when objects are added, so no Dwg_Object pointer is kept.
impl DwgBuilder {
    /// A new drawing whose coordinates are in `insunits` (AutoCAD units code).
    pub fn new(version: DwgVersion, insunits: i32) -> Result<Self> {
        let lock = sys::lock();
        let dwg = unsafe { sys::dwg2gltf_new(version.sys(), insunits as u16) };
        if dwg.is_null() {
            return fail("a document");
        }
        Ok(DwgBuilder {
            dwg,
            archive_dict: ptr::null_mut(),
            _lock: lock,
        })
    }

    fn handleref(&self, code: u8, handle: Handle) -> *mut sys::Dwg_Object_Ref {
        unsafe { sys::dwg_add_handleref(self.dwg, code, handle, ptr::null()) }
    }

    fn handle(obj: *const c_void) -> Handle {
        unsafe { sys::dwg2gltf_handle(obj) }
    }

    /// Layer "0".
    pub fn layer_zero(&self) -> Handle {
        unsafe {
            let clayer = (*self.dwg).header_vars.CLAYER;
            if clayer.is_null() {
                0
            } else {
                (*clayer).absolute_ref
            }
        }
    }

    pub fn model_space(&self) -> Block {
        unsafe {
            let ms = sys::dwg_model_space_object(self.dwg);
            Block((*(*ms).tio.object).tio.BLOCK_HEADER)
        }
    }

    /// The drawing extents (header EXTMIN/EXTMAX), for the initial zoom.
    pub fn set_extents(&mut self, min: Vec3, max: Vec3) {
        unsafe {
            let h = &mut (*self.dwg).header_vars;
            h.EXTMIN = sys::Dwg_Bitcode_3BD {
                x: min[0],
                y: min[1],
                z: min[2],
            };
            h.EXTMAX = sys::Dwg_Bitcode_3BD {
                x: max[0],
                y: max[1],
                z: max[2],
            };
        }
    }

    /// A layer with a true color and the Continuous linetype.
    pub fn layer(&mut self, name: &str, rgb: [u8; 3]) -> Result<Handle> {
        let name = cstr(name)?;
        unsafe {
            let layer = sys::dwg_add_LAYER(self.dwg, name.as_ptr());
            if layer.is_null() {
                return fail("a layer");
            }
            // an unset color would be written as ByBlock
            (*layer).color.rgb = 0xC200_0000 | u32::from_be_bytes([0, rgb[0], rgb[1], rgb[2]]);
            let cont = (*self.dwg).header_vars.LTYPE_CONTINUOUS;
            if !cont.is_null() {
                (*layer).ltype = self.handleref(5, (*cont).absolute_ref);
            }
            Ok(Self::handle(layer as *const c_void))
        }
    }

    /// A MATERIAL with a diffuse color override. Names must be unique.
    pub fn material(&mut self, name: &str, rgb: [u8; 3], opacity: f64) -> Result<Handle> {
        let name = cstr(name)?;
        unsafe {
            let m = sys::dwg_add_MATERIAL(self.dwg, name.as_ptr());
            if m.is_null() {
                return fail("a material");
            }
            (*m).diffuse_color.flag = 1;
            (*m).diffuse_color.rgb = 0xC200_0000 | u32::from_be_bytes([0, rgb[0], rgb[1], rgb[2]]);
            (*m).opacity_percent = opacity;
            sys::dwg2gltf_free((*m).description as *mut c_void);
            (*m).description = sys::dwg_add_u8_input(self.dwg, c"gltf2dwg".as_ptr());
            Ok(Self::handle(m as *const c_void))
        }
    }

    /// An embedded OFML material archive for the MATERIAL called `name`.
    pub fn material_archive(&mut self, name: &str, package: &[u8]) -> Result<()> {
        let name = cstr(name)?;
        let pxcad = cstr(ofml::PXCAD_DICT)?;
        let key = cstr(ofml::MATERIAL_DICT)?;
        unsafe {
            if self.archive_dict.is_null() {
                let nod_entry = sys::dwg_add_DICTIONARY(self.dwg, pxcad.as_ptr(), ptr::null(), 0);
                if nod_entry.is_null() {
                    return fail(ofml::PXCAD_DICT);
                }
                self.archive_dict = sys::dwg2gltf_add_dictionary(nod_entry, key.as_ptr());
                if self.archive_dict.is_null() {
                    return fail(ofml::MATERIAL_DICT);
                }
            }
            let x = sys::dwg_add_XRECORD(self.archive_dict, name.as_ptr());
            if x.is_null()
                || sys::dwg_add_XRECORD_chunks(
                    x,
                    310,
                    package.len(),
                    package.as_ptr(),
                    ofml::ARCHIVE_CHUNK,
                )
                .is_null()
            {
                return fail("a material archive");
            }
        }
        Ok(())
    }

    /// A block definition; finish it with [`DwgBuilder::end_block`].
    pub fn block(&mut self, name: &str) -> Result<Block> {
        let name = cstr(name)?;
        unsafe {
            let hdr = sys::dwg_add_BLOCK_HEADER(self.dwg, name.as_ptr());
            if hdr.is_null() {
                return fail("a block");
            }
            (*hdr).explodable = 1;
            if sys::dwg_add_BLOCK(hdr, name.as_ptr()).is_null() {
                return fail("a block");
            }
            Ok(Block(hdr))
        }
    }

    pub fn end_block(&mut self, block: Block) -> Result<()> {
        if unsafe { sys::dwg_add_ENDBLK(block.0) }.is_null() {
            return fail("a block end");
        }
        Ok(())
    }

    unsafe fn style(&self, ent: *mut sys::Dwg_Object_Entity, style: Style) {
        unsafe {
            (*ent).layer = self.handleref(5, style.layer);
            if let Some(m) = style.material {
                // kept by the encoder for add API drawings
                // (libredwg-entity-material.patch)
                (*ent).material_flags = 3;
                (*ent).material = self.handleref(5, m);
            }
        }
    }

    pub fn mesh(&mut self, block: Block, m: &MeshData, style: Style) -> Result<()> {
        // LibreDWG's encoder writes an empty MESH (without an error) when a
        // list exceeds 16M values; the caller splits bigger meshes
        const ENC_MAX_COUNT: usize = 0x100_0000;
        if m.faces.len() > ENC_MAX_COUNT / 2 || m.vertices.len() > ENC_MAX_COUNT / 4 {
            return Err(WriteError(format!(
                "a MESH with {} vertices and {} face values is too large to encode",
                m.vertices.len(),
                m.faces.len()
            )));
        }
        let verts: Vec<sys::dwg_point_3d> = m.vertices.iter().map(|&p| point(p)).collect();
        unsafe {
            let mesh = sys::dwg_add_MESH(
                block.0,
                verts.len() as u32,
                verts.as_ptr(),
                m.faces.len() as u32,
                m.faces.as_ptr(),
            );
            if mesh.is_null() {
                return fail("a MESH");
            }
            self.style((*mesh).parent, style);
            if m.double_sided.is_none() && m.uvs.is_none() && m.normals.is_none() {
                return Ok(());
            }
            let xdic = sys::dwg_add_xdictionary(mesh as *const c_void);
            if xdic.is_null() {
                return fail("an extension dictionary");
            }
            let record = |key: &str| -> Result<*mut sys::Dwg_Object_XRECORD> {
                let x = sys::dwg_add_XRECORD(xdic, cstr(key)?.as_ptr());
                if x.is_null() { fail(key) } else { Ok(x) }
            };
            if let Some(ds) = m.double_sided {
                sys::dwg_add_XRECORD_bool(record(ofml::DOUBLE_SIDED)?, 290, ds as u8);
            }
            if let Some(uvs) = m.uvs {
                let bytes: Vec<u8> = uvs.iter().flatten().flat_map(|c| c.to_le_bytes()).collect();
                let x = record(ofml::TEX_COORDS)?;
                if sys::dwg_add_XRECORD_chunks(x, 310, bytes.len(), bytes.as_ptr(), ofml::UV_CHUNK)
                    .is_null()
                {
                    return fail(ofml::TEX_COORDS);
                }
            }
            if let Some(normals) = m.normals {
                let values: Vec<f64> = normals.iter().flatten().copied().collect();
                const CODES: [c_short; 3] = [40, 41, 42];
                let x = record(ofml::VERTEX_NORMALS)?;
                if sys::dwg_add_XRECORD_reals(x, 3, CODES.as_ptr(), values.len(), values.as_ptr())
                    .is_null()
                {
                    return fail(ofml::VERTEX_NORMALS);
                }
            }
        }
        Ok(())
    }

    pub fn line(&mut self, block: Block, a: Vec3, b: Vec3, style: Style) -> Result<()> {
        unsafe {
            let line = sys::dwg_add_LINE(block.0, &point(a), &point(b));
            if line.is_null() {
                return fail("a LINE");
            }
            self.style((*line).parent, style);
        }
        Ok(())
    }

    /// An INSERT of the block `name`; `ins_pt` is in the OCS of `extrusion`.
    #[allow(clippy::too_many_arguments)]
    pub fn insert(
        &mut self,
        owner: Block,
        name: &str,
        ins_pt: Vec3,
        scale: Vec3,
        rotation: f64,
        extrusion: Vec3,
        style: Style,
    ) -> Result<()> {
        let name = cstr(name)?;
        unsafe {
            let ins = sys::dwg_add_INSERT(
                owner.0,
                &point(ins_pt),
                name.as_ptr() as *const c_char,
                scale[0],
                scale[1],
                scale[2],
                rotation,
            );
            if ins.is_null() {
                return fail("an INSERT");
            }
            (*ins).extrusion = sys::Dwg_Bitcode_3BD {
                x: extrusion[0],
                y: extrusion[1],
                z: extrusion[2],
            };
            self.style((*ins).parent, style);
        }
        Ok(())
    }

    /// Encode the drawing.
    pub fn encode(self) -> Result<Vec<u8>> {
        let mut data = ptr::null_mut();
        let mut size = 0;
        let error = unsafe { sys::dwg2gltf_encode(self.dwg, &mut data, &mut size) };
        // (DWG_ERR_CRITICAL is DWG_ERR_CLASSESNOTFOUND)
        if error >= sys::DWG_ERROR_DWG_ERR_CLASSESNOTFOUND as i32 || data.is_null() {
            return Err(WriteError(format!(
                "LibreDWG cannot encode the drawing (error 0x{error:x})"
            )));
        }
        let out = unsafe { slice::from_raw_parts(data, size) }.to_vec();
        unsafe { sys::dwg2gltf_free(data as *mut c_void) };
        Ok(out)
    }
}
