// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Read DWG files with the bundled LibreDWG decoder into plain Rust data.
//!
//! This is the only module that touches LibreDWG. It copies what the
//! converter needs out of `Dwg_Data` and frees it again, compensating for
//! LibreDWG quirks on the way (see the comments at the fields).

use std::ffi::{CStr, c_char};
use std::path::Path;
use std::slice;

use libredwg_sys as sys;

use crate::acis::sab::AcisData;
use crate::geom::{Vec2, Vec3};
use crate::mapper::{self, MaterialMapper};

/// An absolute object handle; 0 means none.
pub type Handle = u64;

#[derive(Debug, Default)]
pub struct Drawing {
    /// INSUNITS header variable.
    pub insunits: i32,
    /// All objects in file order.
    pub objects: Vec<Object>,
    /// The decompressed AcDsPrototype_1b section (R2013+), or empty.
    pub acds: Vec<u8>,
    /// LibreDWG's error bits below the critical ones: parts of the file it
    /// could not decode and skipped (0: none).
    pub decode_error: i32,
}

#[derive(Debug)]
pub struct Object {
    pub handle: Handle,
    /// LibreDWG's type name: "LINE", "MESH", "LAYER", ...
    pub kind: String,
    pub owner: Handle,
    /// Extension dictionary.
    pub xdic: Handle,
    pub data: Data,
}

#[derive(Debug)]
pub enum Data {
    Entity(Box<Entity>),
    BlockHeader(BlockHeader),
    Layer(Layer),
    Material(Material),
    Dictionary(Vec<(String, Handle)>),
    /// (DXF group code, value) pairs.
    XRecord(Vec<(i16, XValue)>),
    Other,
}

#[derive(Debug)]
pub struct BlockHeader {
    pub name: String,
    pub base_pt: Vec3,
    /// The block's entities (R2004+ files list them; older ones only link
    /// entities to their owner).
    pub entities: Option<Vec<Handle>>,
    /// An external reference (another drawing, not in this file): its path.
    pub xref: Option<String>,
}

#[derive(Debug)]
pub struct Layer {
    pub name: String,
    pub color: Color,
    pub material: Handle,
    pub off: bool,
    pub frozen: bool,
}

#[derive(Debug)]
pub struct Material {
    pub name: String,
    /// diffuse_color.rgb if diffuse_color.flag says it overrides.
    pub diffuse: Option<u32>,
    pub opacity: f64,
    /// diffusemap.transmatrix (4x4)
    pub transmatrix: Option<[f64; 16]>,
    /// diffusemap.filename
    pub filename: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum XValue {
    Real(f64),
    Point(Vec3),
    Int(i64),
    Text(String),
    Binary(Vec<u8>),
    Handle(Handle),
    Other,
}

/// A DWG color as stored: an ACI index and/or an RGB value whose top byte
/// is the color method (0xC0 ByLayer, 0xC1 ByBlock, 0xC2 RGB, 0xC3 ACI).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Color {
    pub index: Option<i32>,
    /// Only when it carries a color method byte.
    pub rgb: Option<u32>,
}

#[derive(Debug)]
pub struct Entity {
    /// 0 owner handle, 1 paper space, 2 model space, 3 owner handle
    pub entmode: u8,
    pub invisible: bool,
    pub layer: Handle,
    pub color: Color,
    /// 0 ByLayer, 1 ByBlock, 2 continuous, 3 explicit handle.
    pub material_flags: u8,
    /// LibreDWG's `material` and `plotstyle` handles; the material is
    /// whichever is a MATERIAL object (see [`read`]).
    pub material_refs: [Handle; 2],
    /// R2013+: the ACIS data lives in the AcDs section.
    pub has_ds_data: bool,
    pub thickness: f64,
    pub extrusion: Option<Vec3>,
    /// AutoCAD's material mapper from the entity's XDATA (APPID
    /// `ACAD_MATERIAL_MAPPER`).
    pub material_mapper: Option<MaterialMapper>,
    pub shape: Shape,
}

#[derive(Debug)]
pub enum Shape {
    Face3d([Vec3; 4]),
    /// SOLID and TRACE: 2D corners in OCS.
    Solid {
        corners: [Vec3; 4],
        elevation: f64,
    },
    /// POLYLINE_2D/3D/MESH/PFACE.
    Polyline {
        flag: u16,
        elevation: f64,
        /// Default start and end width of the segments (POLYLINE_2D).
        widths: [f64; 2],
        m: u16,
        n: u16,
        vertices: Vec<Handle>,
        first: Handle,
        last: Handle,
    },
    /// VERTEX_2D/3D/MESH/PFACE.
    Vertex {
        point: Vec3,
        bulge: f64,
        flag: u8,
        /// Start and end width of the segment from this vertex (VERTEX_2D).
        widths: [f64; 2],
    },
    /// VERTEX_PFACE_FACE: 1-based vertex numbers, negative = invisible edge.
    PfaceFace([i16; 4]),
    Mesh {
        vertices: Vec<Vec3>,
        faces: Vec<u32>,
    },
    Line {
        start: Vec3,
        end: Vec3,
    },
    Circle {
        center: Vec3,
        radius: f64,
    },
    Arc {
        center: Vec3,
        radius: f64,
        start_angle: f64,
        end_angle: f64,
    },
    Ellipse {
        center: Vec3,
        major: Vec3,
        ratio: f64,
        start_angle: f64,
        end_angle: f64,
    },
    LwPolyline {
        flag: u16,
        elevation: f64,
        points: Vec<Vec2>,
        bulges: Vec<f64>,
        const_width: f64,
        /// Start and end width per segment (empty: `const_width`).
        widths: Vec<[f64; 2]>,
    },
    Spline {
        flag: u16,
        degree: usize,
        ctrl: Vec<Vec3>,
        weights: Vec<f64>,
        knots: Vec<f64>,
        fit: Vec<Vec3>,
    },
    Insert {
        block: Handle,
        ins_pt: Vec3,
        scale: Vec3,
        rotation: f64,
        cols: u16,
        rows: u16,
        col_spacing: f64,
        row_spacing: f64,
    },
    /// 3DSOLID, BODY, REGION and the surface entities.
    Acis {
        data: Option<AcisData>,
        wires: Vec<Vec<Vec3>>,
    },
    Other,
}

#[derive(Debug)]
pub struct DwgReadError(pub String);

impl std::fmt::Display for DwgReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for DwgReadError {}

/// Owns a decoded `Dwg_Data` and frees it on drop.
struct Decoded(Box<sys::Dwg_Data>);

impl Drop for Decoded {
    fn drop(&mut self) {
        unsafe { sys::dwg_free(&mut *self.0) }
    }
}

pub fn read(path: &Path) -> Result<Drawing, DwgReadError> {
    let data = std::fs::read(path)
        .map_err(|e| DwgReadError(format!("cannot read {}: {e}", path.display())))?;
    read_bytes(data, &path.display().to_string())
}

/// Decode a DWG file image; `name` names it in error messages.
pub fn read_bytes(mut data: Vec<u8>, name: &str) -> Result<Drawing, DwgReadError> {
    let size = data.len();
    data.push(0); // LibreDWG expects a terminating 0 byte
    // declared first, so it is released after `dwg` is freed
    let _lock = sys::lock();
    // SAFETY: Dwg_Data is plain C data; all-zero is its initial state.
    let mut dwg = Decoded(Box::new(unsafe { std::mem::zeroed() }));
    let error = unsafe { sys::dwg2gltf_decode(data.as_mut_ptr(), size, &mut *dwg.0) };
    // like dwgread: recoverable errors are only reported, and the critical
    // ones mean the file could not be decoded
    // (DWG_ERR_CRITICAL is DWG_ERR_CLASSESNOTFOUND)
    if error >= sys::DWG_ERROR_DWG_ERR_CLASSESNOTFOUND as i32 || dwg.0.num_objects == 0 {
        return Err(DwgReadError(format!(
            "LibreDWG cannot decode {name} (error 0x{error:x})"
        )));
    }
    let mut reader = Reader {
        dwg: &dwg.0,
        mapper_app: 0,
    };
    reader.mapper_app = unsafe { reader.appid(mapper::APPID) };
    let mut drawing = unsafe { reader.drawing() };
    drawing.decode_error = error;
    Ok(drawing)
}

struct Reader<'a> {
    dwg: &'a sys::Dwg_Data,
    /// The APPID handle of the material mapper's XDATA (0: none).
    mapper_app: Handle,
}

unsafe fn handle(r: sys::BITCODE_H) -> Handle {
    if r.is_null() {
        0
    } else {
        unsafe { (*r).absolute_ref }
    }
}

fn p3(p: sys::BITCODE_3BD) -> Vec3 {
    [p.x, p.y, p.z]
}

/// An XRECORD string: UTF-16 in R2007+ files, else bytes in the drawing's
/// codepage (read as UTF-8, which covers ASCII).
unsafe fn resbuf_text(s: &sys::_dwg_binary_chunk) -> String {
    let n = s.size as usize;
    unsafe {
        if s.is_tu() != 0 {
            String::from_utf16_lossy(array(s.u.wdata.cast_const(), n))
        } else {
            String::from_utf8_lossy(array(s.u.data.cast::<u8>().cast_const(), n)).into_owned()
        }
    }
}

/// The elements of a C array; empty for NULL.
unsafe fn array<'a, T>(ptr: *const T, len: impl TryInto<usize>) -> &'a [T] {
    let len = len.try_into().unwrap_or(0);
    if ptr.is_null() || len == 0 {
        &[]
    } else {
        unsafe { slice::from_raw_parts(ptr, len) }
    }
}

impl Reader<'_> {
    /// The file's own version (LibreDWG's `from_version`).
    fn file_version(&self) -> u32 {
        self.dwg.header.from_version
    }

    /// The handle of the APPID named `name` (ignoring case), or 0.
    unsafe fn appid(&self, name: &str) -> Handle {
        for o in unsafe { array(self.dwg.object, self.dwg.num_objects) } {
            if o.fixedtype != sys::DWG_OBJECT_TYPE_DWG_TYPE_APPID
                || o.supertype == sys::DWG_OBJECT_SUPERTYPE_DWG_SUPERTYPE_ENTITY
            {
                continue;
            }
            let ob = unsafe { o.tio.object };
            if ob.is_null() || unsafe { (*ob).tio.APPID.is_null() } {
                continue;
            }
            let app = unsafe { &*(*ob).tio.APPID };
            if unsafe { self.text(app.name) }.eq_ignore_ascii_case(name) {
                return o.handle.value;
            }
        }
        0
    }

    /// The XDATA items of the APPID `app` on an entity, as (raw EED code,
    /// value): 70 int16, 71 int32, 10..15 points, 40..42 reals; others are
    /// `XValue::Other`.
    unsafe fn eed(&self, e: &sys::Dwg_Object_Entity, app: Handle) -> Vec<(u8, XValue)> {
        let mut out = vec![];
        if app == 0 {
            return out;
        }
        for item in unsafe { array(e.eed, e.num_eed) } {
            if item.handle.value != app || item.data.is_null() {
                continue;
            }
            // the union is packed after the 1-byte code (25 bytes in all,
            // which LibreDWG always allocates): read it unaligned
            let data = item.data;
            let code = unsafe { (*data).code };
            let u = unsafe { std::ptr::addr_of!((*data).u) }.cast::<u8>();
            let value = unsafe {
                match code {
                    70 => XValue::Int(std::ptr::read_unaligned(u.cast::<u16>()) as i16 as i64),
                    71 => XValue::Int(std::ptr::read_unaligned(u.cast::<u32>()) as i32 as i64),
                    10..=15 => XValue::Point(std::ptr::read_unaligned(u.cast::<[f64; 3]>())),
                    40..=42 => XValue::Real(std::ptr::read_unaligned(u.cast::<f64>())),
                    _ => XValue::Other,
                }
            };
            out.push((code, value));
        }
        out
    }

    unsafe fn drawing(&self) -> Drawing {
        let dwg = self.dwg;
        let acds = unsafe { array(dwg.acds.raw, dwg.acds.raw_size) }.to_vec();
        let objects = unsafe { array(dwg.object, dwg.num_objects) }
            .iter()
            .map(|o| unsafe { self.object(o) })
            .collect();
        Drawing {
            insunits: dwg.header_vars.INSUNITS as i32,
            objects,
            acds,
            decode_error: 0,
        }
    }

    /// DXF name of the class behind a custom object type (500 and up).
    unsafe fn class_name(&self, ty: u16) -> Option<String> {
        let i = (ty as usize).checked_sub(500)?;
        if i >= self.dwg.num_classes as usize || self.dwg.dwg_class.is_null() {
            return None;
        }
        let name = unsafe { (*self.dwg.dwg_class.add(i)).dxfname };
        (!name.is_null()).then(|| {
            unsafe { CStr::from_ptr(name) }
                .to_string_lossy()
                .into_owned()
        })
    }

    unsafe fn text(&self, s: *const c_char) -> String {
        let utf8 = unsafe { sys::dwg2gltf_text(self.dwg, s) };
        if utf8.is_null() {
            return String::new();
        }
        let out = unsafe { CStr::from_ptr(utf8) }
            .to_string_lossy()
            .into_owned();
        unsafe { sys::dwg2gltf_free(utf8.cast()) };
        out
    }

    /// LibreDWG keeps the entity color of R2004+ files as ENC (index plus
    /// optional RGB) and the table colors as CMC (method byte in the RGB).
    fn color(&self, c: &sys::Dwg_Color) -> Color {
        if self.file_version() < sys::DWG_VERSION_TYPE_R_2004 {
            return Color {
                index: Some(c.index as i32),
                rgb: None,
            };
        }
        let index = if c.index != 0 {
            Some(c.index as u16 as i32)
        } else if c.method == 0xC2 && (1..256).contains(&(c.rgb & 0xFF)) {
            Some((c.rgb & 0xFF) as i32)
        } else {
            None
        };
        Color {
            index,
            rgb: (c.rgb >= 0x1000_0000).then_some(c.rgb),
        }
    }

    unsafe fn object(&self, o: &sys::Dwg_Object) -> Object {
        let mut kind = if o.name.is_null() {
            String::new()
        } else {
            unsafe { CStr::from_ptr(o.name) }
                .to_string_lossy()
                .into_owned()
        };
        if kind == "UNKNOWN_ENT" {
            // LibreDWG decodes only the common data of classes it doesn't
            // know (e.g. SURFACE, AcDbSurface, whose ACIS data lives in the
            // AcDs section); name them by their class instead
            if let Some(name) = unsafe { self.class_name(o.type_) } {
                kind = name;
            }
        }
        let mut obj = Object {
            handle: o.handle.value,
            kind,
            owner: 0,
            xdic: 0,
            data: Data::Other,
        };
        if o.supertype == sys::DWG_OBJECT_SUPERTYPE_DWG_SUPERTYPE_ENTITY {
            let ent = unsafe { o.tio.entity };
            if !ent.is_null() {
                let ent = unsafe { &*ent };
                (obj.owner, obj.xdic) =
                    unsafe { (handle(ent.ownerhandle), handle(ent.xdicobjhandle)) };
                obj.data = Data::Entity(Box::new(unsafe { self.entity(o, ent) }));
            }
        } else {
            let ob = unsafe { o.tio.object };
            if !ob.is_null() {
                let ob = unsafe { &*ob };
                (obj.owner, obj.xdic) =
                    unsafe { (handle(ob.ownerhandle), handle(ob.xdicobjhandle)) };
                obj.data = unsafe { self.object_data(o.fixedtype, ob) };
            }
        }
        obj
    }

    unsafe fn object_data(&self, ty: u32, ob: &sys::Dwg_Object_Object) -> Data {
        unsafe {
            // every member of the union is the pointer to the type's data,
            // which a damaged object may lack (as checked for entities)
            if ob.tio.APPID.is_null() {
                return Data::Other;
            }
            match ty {
                sys::DWG_OBJECT_TYPE_DWG_TYPE_BLOCK_HEADER => {
                    let b = &*ob.tio.BLOCK_HEADER;
                    let entities =
                        (self.file_version() >= sys::DWG_VERSION_TYPE_R_2004).then(|| {
                            array(b.entities, b.num_owned)
                                .iter()
                                .map(|&h| handle(h))
                                .collect()
                        });
                    // flag 4: xref, 8: overlaid xref
                    let xref = (b.blkisxref != 0 || b.xrefoverlaid != 0 || b.flag & 12 != 0)
                        .then(|| self.text(b.xref_pname));
                    Data::BlockHeader(BlockHeader {
                        name: self.text(b.name),
                        base_pt: p3(b.base_pt),
                        entities,
                        xref,
                    })
                }
                sys::DWG_OBJECT_TYPE_DWG_TYPE_LAYER => {
                    let l = &*ob.tio.LAYER;
                    Data::Layer(Layer {
                        name: self.text(l.name),
                        color: self.color(&l.color),
                        material: handle(l.material),
                        // pre-R13 files mark off layers by a negative color
                        off: l.off != 0 || l.color.index < 0,
                        frozen: l.frozen != 0,
                    })
                }
                sys::DWG_OBJECT_TYPE_DWG_TYPE_MATERIAL => {
                    let m = &*ob.tio.MATERIAL;
                    let map = &m.diffusemap;
                    let transmatrix = (!map.transmatrix.is_null())
                        .then(|| array(map.transmatrix, 16usize).try_into().unwrap());
                    Data::Material(Material {
                        name: self.text(m.name),
                        diffuse: (m.diffuse_color.flag == 1).then_some(m.diffuse_color.rgb),
                        opacity: m.opacity_percent,
                        transmatrix,
                        filename: self.text(map.filename),
                    })
                }
                sys::DWG_OBJECT_TYPE_DWG_TYPE_DICTIONARY => {
                    let d = &*ob.tio.DICTIONARY;
                    Data::Dictionary(self.dictionary(d.texts, d.itemhandles, d.numitems))
                }
                sys::DWG_OBJECT_TYPE_DWG_TYPE_DICTIONARYWDFLT => {
                    let d = &*ob.tio.DICTIONARYWDFLT;
                    Data::Dictionary(self.dictionary(d.texts, d.itemhandles, d.numitems))
                }
                sys::DWG_OBJECT_TYPE_DWG_TYPE_XRECORD => {
                    Data::XRecord(self.xrecord(&*ob.tio.XRECORD))
                }
                _ => Data::Other,
            }
        }
    }

    unsafe fn dictionary(
        &self,
        texts: *mut sys::BITCODE_TV,
        handles: *mut sys::BITCODE_H,
        n: u32,
    ) -> Vec<(String, Handle)> {
        unsafe {
            let (texts, handles) = (array(texts, n), array(handles, n));
            texts
                .iter()
                .zip(handles)
                .map(|(&t, &h)| (self.text(t), handle(h)))
                .collect()
        }
    }

    unsafe fn xrecord(&self, x: &sys::Dwg_Object_XRECORD) -> Vec<(i16, XValue)> {
        let mut out = vec![];
        let mut rb = x.xdata;
        while !rb.is_null() && out.len() < x.num_xdata as usize {
            let r = unsafe { &*rb };
            let v = &r.value;
            let value = unsafe {
                match sys::dwg_resbuf_value_type(r.type_) {
                    sys::RESBUF_VALUE_TYPE_DWG_VT_REAL => XValue::Real(v.dbl),
                    sys::RESBUF_VALUE_TYPE_DWG_VT_POINT3D => XValue::Point(v.pt),
                    sys::RESBUF_VALUE_TYPE_DWG_VT_BOOL | sys::RESBUF_VALUE_TYPE_DWG_VT_INT8 => {
                        XValue::Int(v.i8_ as i64)
                    }
                    sys::RESBUF_VALUE_TYPE_DWG_VT_INT16 => XValue::Int(v.i16_ as i64),
                    sys::RESBUF_VALUE_TYPE_DWG_VT_INT32 => XValue::Int(v.i32_ as i64),
                    sys::RESBUF_VALUE_TYPE_DWG_VT_INT64 => XValue::Int(v.i64_ as i64),
                    sys::RESBUF_VALUE_TYPE_DWG_VT_STRING => XValue::Text(resbuf_text(&v.str_)),
                    sys::RESBUF_VALUE_TYPE_DWG_VT_BINARY => {
                        XValue::Binary(array(v.str_.u.data.cast::<u8>(), v.str_.size).to_vec())
                    }
                    sys::RESBUF_VALUE_TYPE_DWG_VT_HANDLE
                    | sys::RESBUF_VALUE_TYPE_DWG_VT_OBJECTID => XValue::Handle(v.absref),
                    _ => XValue::Other,
                }
            };
            out.push((r.type_, value));
            rb = r.nextrb;
        }
        out
    }

    unsafe fn entity(&self, o: &sys::Dwg_Object, e: &sys::Dwg_Object_Entity) -> Entity {
        // LibreDWG 0.14 read the R2007+ plot-style and material flag bits in
        // swapped order (GH #1436, fixed upstream). The handles are still
        // told apart by object type later.
        let mut ent = Entity {
            entmode: e.entmode,
            invisible: e.invisible != 0,
            layer: unsafe { handle(e.layer) },
            color: self.color(&e.color),
            material_flags: e.material_flags,
            material_refs: unsafe { [handle(e.material), handle(e.plotstyle)] },
            has_ds_data: e.has_ds_data != 0,
            thickness: 0.0,
            extrusion: None,
            material_mapper: None,
            shape: Shape::Other,
        };
        if self.mapper_app != 0 {
            ent.material_mapper =
                MaterialMapper::from_eed(&unsafe { self.eed(e, self.mapper_app) });
        }
        ent.shape = unsafe { self.shape(o.fixedtype, e, &mut ent) };
        ent
    }

    unsafe fn shape(&self, ty: u32, e: &sys::Dwg_Object_Entity, ent: &mut Entity) -> Shape {
        use sys::*;
        let tio = &e.tio;
        unsafe {
            if tio.UNUSED.is_null() {
                return Shape::Other;
            }
            match ty {
                DWG_OBJECT_TYPE_DWG_TYPE__3DFACE => {
                    let f = &*tio._3DFACE;
                    Shape::Face3d([p3(f.corner1), p3(f.corner2), p3(f.corner3), p3(f.corner4)])
                }
                // TRACE has the same layout as SOLID
                DWG_OBJECT_TYPE_DWG_TYPE_SOLID | DWG_OBJECT_TYPE_DWG_TYPE_TRACE => {
                    let s = &*tio.SOLID;
                    (ent.thickness, ent.extrusion) = (s.thickness, Some(p3(s.extrusion)));
                    let c = |p: BITCODE_2RD| [p.x, p.y, 0.0];
                    Shape::Solid {
                        corners: [c(s.corner1), c(s.corner2), c(s.corner3), c(s.corner4)],
                        elevation: s.elevation,
                    }
                }
                DWG_OBJECT_TYPE_DWG_TYPE_POLYLINE_PFACE => {
                    let p = &*tio.POLYLINE_PFACE;
                    self.polyline(
                        0,
                        0.0,
                        0,
                        0,
                        p.vertex,
                        p.num_owned,
                        p.first_vertex,
                        p.last_vertex,
                    )
                }
                DWG_OBJECT_TYPE_DWG_TYPE_POLYLINE_MESH => {
                    let p = &*tio.POLYLINE_MESH;
                    self.polyline(
                        p.flag,
                        0.0,
                        p.num_m_verts,
                        p.num_n_verts,
                        p.vertex,
                        p.num_owned,
                        p.first_vertex,
                        p.last_vertex,
                    )
                }
                DWG_OBJECT_TYPE_DWG_TYPE_POLYLINE_2D => {
                    let p = &*tio.POLYLINE_2D;
                    (ent.thickness, ent.extrusion) = (p.thickness, Some(p3(p.extrusion)));
                    let mut shape = self.polyline(
                        p.flag,
                        p.elevation,
                        0,
                        0,
                        p.vertex,
                        p.num_owned,
                        p.first_vertex,
                        p.last_vertex,
                    );
                    if let Shape::Polyline { widths, .. } = &mut shape {
                        *widths = [p.start_width, p.end_width];
                    }
                    shape
                }
                DWG_OBJECT_TYPE_DWG_TYPE_POLYLINE_3D => {
                    let p = &*tio.POLYLINE_3D;
                    ent.extrusion = Some(p3(p.extrusion));
                    self.polyline(
                        p.flag as u16,
                        0.0,
                        0,
                        0,
                        p.vertex,
                        p.num_owned,
                        p.first_vertex,
                        p.last_vertex,
                    )
                }
                DWG_OBJECT_TYPE_DWG_TYPE_VERTEX_2D => {
                    let v = &*tio.VERTEX_2D;
                    Shape::Vertex {
                        point: p3(v.point),
                        bulge: v.bulge,
                        flag: v.flag,
                        widths: [v.start_width, v.end_width],
                    }
                }
                DWG_OBJECT_TYPE_DWG_TYPE_VERTEX_3D
                | DWG_OBJECT_TYPE_DWG_TYPE_VERTEX_MESH
                | DWG_OBJECT_TYPE_DWG_TYPE_VERTEX_PFACE => {
                    let v = &*tio.VERTEX_3D; // the same struct for all three
                    Shape::Vertex {
                        point: p3(v.point),
                        bulge: 0.0,
                        flag: v.flag,
                        widths: [0.0; 2],
                    }
                }
                DWG_OBJECT_TYPE_DWG_TYPE_VERTEX_PFACE_FACE => {
                    Shape::PfaceFace((*tio.VERTEX_PFACE_FACE).vertind)
                }
                DWG_OBJECT_TYPE_DWG_TYPE_MESH => {
                    let m = &*tio.MESH;
                    Shape::Mesh {
                        vertices: array(m.vertex, m.num_vertex)
                            .iter()
                            .map(|&p| p3(p))
                            .collect(),
                        faces: array(m.faces, m.num_faces).to_vec(),
                    }
                }
                DWG_OBJECT_TYPE_DWG_TYPE_LINE => {
                    let l = &*tio.LINE;
                    (ent.thickness, ent.extrusion) = (l.thickness, Some(p3(l.extrusion)));
                    Shape::Line {
                        start: p3(l.start),
                        end: p3(l.end),
                    }
                }
                DWG_OBJECT_TYPE_DWG_TYPE_CIRCLE => {
                    let c = &*tio.CIRCLE;
                    (ent.thickness, ent.extrusion) = (c.thickness, Some(p3(c.extrusion)));
                    Shape::Circle {
                        center: p3(c.center),
                        radius: c.radius,
                    }
                }
                DWG_OBJECT_TYPE_DWG_TYPE_ARC => {
                    let a = &*tio.ARC;
                    (ent.thickness, ent.extrusion) = (a.thickness, Some(p3(a.extrusion)));
                    Shape::Arc {
                        center: p3(a.center),
                        radius: a.radius,
                        start_angle: a.start_angle,
                        end_angle: a.end_angle,
                    }
                }
                DWG_OBJECT_TYPE_DWG_TYPE_ELLIPSE => {
                    let el = &*tio.ELLIPSE;
                    ent.extrusion = Some(p3(el.extrusion));
                    Shape::Ellipse {
                        center: p3(el.center),
                        major: p3(el.sm_axis),
                        ratio: el.axis_ratio,
                        start_angle: el.start_angle,
                        end_angle: el.end_angle,
                    }
                }
                DWG_OBJECT_TYPE_DWG_TYPE_LWPOLYLINE => {
                    let l = &*tio.LWPOLYLINE;
                    (ent.thickness, ent.extrusion) = (l.thickness, Some(p3(l.extrusion)));
                    Shape::LwPolyline {
                        flag: l.flag,
                        elevation: l.elevation,
                        points: array(l.points, l.num_points)
                            .iter()
                            .map(|p| [p.x, p.y])
                            .collect(),
                        bulges: array(l.bulges, l.num_bulges).to_vec(),
                        const_width: l.const_width,
                        widths: array(l.widths, l.num_widths)
                            .iter()
                            .map(|w| [w.start, w.end])
                            .collect(),
                    }
                }
                DWG_OBJECT_TYPE_DWG_TYPE_SPLINE => {
                    let s = &*tio.SPLINE;
                    let ctrl = array(s.ctrl_pts, s.num_ctrl_pts);
                    Shape::Spline {
                        flag: s.flag,
                        degree: s.degree as usize,
                        ctrl: ctrl.iter().map(|p| [p.x, p.y, p.z]).collect(),
                        // w is only stored for weighted splines
                        weights: ctrl
                            .iter()
                            .map(|p| if s.weighted != 0 { p.w } else { 1.0 })
                            .collect(),
                        knots: array(s.knots, s.num_knots).to_vec(),
                        fit: array(s.fit_pts, s.num_fit_pts)
                            .iter()
                            .map(|&p| p3(p))
                            .collect(),
                    }
                }
                DWG_OBJECT_TYPE_DWG_TYPE_INSERT => {
                    let i = &*tio.INSERT;
                    ent.extrusion = Some(p3(i.extrusion));
                    Shape::Insert {
                        block: handle(i.block_header),
                        ins_pt: p3(i.ins_pt),
                        scale: p3(i.scale),
                        rotation: i.rotation,
                        cols: i.num_cols,
                        rows: i.num_rows,
                        col_spacing: i.col_spacing,
                        row_spacing: i.row_spacing,
                    }
                }
                DWG_OBJECT_TYPE_DWG_TYPE_MINSERT => {
                    let i = &*tio.MINSERT;
                    ent.extrusion = Some(p3(i.extrusion));
                    Shape::Insert {
                        block: handle(i.block_header),
                        ins_pt: p3(i.ins_pt),
                        scale: p3(i.scale),
                        rotation: i.rotation,
                        cols: i.num_cols,
                        rows: i.num_rows,
                        col_spacing: i.col_spacing,
                        row_spacing: i.row_spacing,
                    }
                }
                DWG_OBJECT_TYPE_DWG_TYPE__3DSOLID
                | DWG_OBJECT_TYPE_DWG_TYPE_BODY
                | DWG_OBJECT_TYPE_DWG_TYPE_REGION => self.acis(&*tio._3DSOLID),
                // the surface entities start with the same ACIS fields
                DWG_OBJECT_TYPE_DWG_TYPE_PLANESURFACE
                | DWG_OBJECT_TYPE_DWG_TYPE_EXTRUDEDSURFACE
                | DWG_OBJECT_TYPE_DWG_TYPE_LOFTEDSURFACE
                | DWG_OBJECT_TYPE_DWG_TYPE_REVOLVEDSURFACE
                | DWG_OBJECT_TYPE_DWG_TYPE_SWEPTSURFACE
                | DWG_OBJECT_TYPE_DWG_TYPE_NURBSURFACE => self.acis(&*tio._3DSOLID),
                _ => Shape::Other,
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    unsafe fn polyline(
        &self,
        flag: u16,
        elevation: f64,
        m: u16,
        n: u16,
        vertex: *mut sys::BITCODE_H,
        count: u32,
        first: sys::BITCODE_H,
        last: sys::BITCODE_H,
    ) -> Shape {
        unsafe {
            let vertices = array(vertex, count).iter().map(|&h| handle(h)).collect();
            Shape::Polyline {
                flag,
                elevation,
                widths: [0.0; 2],
                m,
                n,
                vertices,
                first: handle(first),
                last: handle(last),
            }
        }
    }

    unsafe fn acis(&self, s: &sys::Dwg_Entity__3DSOLID) -> Shape {
        let data = if s.acis_empty != 0 || s.acis_data.is_null() {
            None
        } else if s.version < 2 {
            // SAT text, decrypted by LibreDWG
            let text = unsafe { CStr::from_ptr(s.acis_data.cast()) }.to_bytes();
            Some(AcisData::Sat(text.iter().map(|&b| b as char).collect()))
        } else {
            Some(AcisData::Sab(
                unsafe { array(s.acis_data, s.sab_size) }.to_vec(),
            ))
        };
        let wires = unsafe { array(s.wires, s.num_wires) }
            .iter()
            .map(|w| {
                unsafe { array(w.points, w.num_points) }
                    .iter()
                    .map(|&p| p3(p))
                    .collect()
            })
            .collect();
        Shape::Acis { data, wires }
    }
}
