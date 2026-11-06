// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! dwg2gltf as a shared library with a C interface (`include/dwg2gltf.h`).
//!
//! A scene is converted exactly as for a GLB file: the converter fills a
//! [`GltfWriter`], and this crate copies the writer's meshes, nodes,
//! materials and textures into flat arrays (the GLB is what
//! [`dwg_scene_to_glb`] returns, byte for byte what the command line tool
//! writes). Each mesh gets its visible edges from [`converter::edges`].
//!
//! Every exported function catches panics; errors are status codes, with a
//! message from [`dwg_last_error`]. Conversion runs on its own thread with a
//! large stack, so deep block nesting does not depend on the caller's stack.

use std::cell::RefCell;
use std::ffi::{CStr, CString, c_char};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::OnceLock;
use std::sync::atomic::{AtomicUsize, Ordering};

use converter::edges::{DEFAULT_CREASE_DEG, visible_edges};
use converter::{Document, Error, GltfWriter, Limits, Options, dwg};
use serde_json::{Map, Value};

/// Bumped when a struct or function changes incompatibly; growing a struct
/// at its end (see the `size` fields) does not change it.
pub const DWG_ABI: u32 = 1;

pub const DWG_OK: i32 = 0;
/// A NULL pointer, or invalid options.
pub const DWG_ERR_ARGUMENT: i32 = 1;
/// The file could not be read.
pub const DWG_ERR_IO: i32 = 2;
/// LibreDWG could not decode the drawing.
pub const DWG_ERR_READ: i32 = 3;
pub const DWG_ERR_NO_MODEL_SPACE: i32 = 4;
/// Nothing to show, with [`DWG_OPT_EMPTY_IS_ERROR`].
pub const DWG_ERR_EMPTY: i32 = 5;
/// A non-finite number in the result (a converter bug).
pub const DWG_ERR_NON_FINITE: i32 = 6;
/// Anything else, including a panic in the converter.
pub const DWG_ERR_INTERNAL: i32 = 7;

/// [`DwgOptions::flags`]. All flags clear is the default.
pub const DWG_OPT_NO_LINES: u32 = 1;
pub const DWG_OPT_PLAN_SYMBOLS: u32 = 2;
pub const DWG_OPT_NO_EDGES: u32 = 4;
pub const DWG_OPT_Z_UP: u32 = 8;
pub const DWG_OPT_EMPTY_IS_ERROR: u32 = 16;

/// [`DwgOptions::units`]: 0 = the drawing's INSUNITS, 1..=24 = that
/// INSUNITS code instead, [`DWG_UNITS_AUTO`] = what `--units auto` does.
pub const DWG_UNITS_DRAWING: i32 = 0;
pub const DWG_UNITS_AUTO: i32 = -1;

/// [`DwgSubmesh::topology`].
pub const DWG_TRIANGLES: u32 = 0;
pub const DWG_LINES: u32 = 1;

/// [`DwgMaterial::flags`].
pub const DWG_MAT_DOUBLE_SIDED: u32 = 1;
pub const DWG_MAT_BLEND: u32 = 2;
pub const DWG_MAT_UNLIT: u32 = 4;

/// [`DwgSceneData::flags`].
pub const DWG_SCENE_Z_UP: u32 = 1;
pub const DWG_SCENE_HAS_BOUNDS: u32 = 2;

#[repr(C)]
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct DwgOptions {
    /// `sizeof(DwgOptions)` as the caller knows it; fields past it take
    /// their defaults.
    pub size: u32,
    pub flags: u32,
    pub units: i32,
    /// Crease angle of the visible edges in degrees; 0 = 30.
    pub crease_angle: f32,
    /// Limits per drawing; 0 = the built-in default.
    pub max_instances: u32,
    pub max_triangles: u32,
    pub max_line_points: u32,
    pub reserved: u32,
}

#[repr(C)]
pub struct DwgSubmesh {
    pub topology: u32,
    pub material: u32,
    pub first_index: u32,
    pub index_count: u32,
    pub first_vertex: u32,
    pub vertex_count: u32,
}

#[repr(C)]
pub struct DwgMesh {
    pub name: *const c_char,
    pub positions: *const f32,
    pub normals: *const f32,
    pub uvs: *const f32,
    pub indices: *const u32,
    pub submeshes: *const DwgSubmesh,
    pub edges: *const u32,
    pub vertex_count: u32,
    pub index_count: u32,
    pub submesh_count: u32,
    pub edge_count: u32,
    pub triangle_count: u32,
    pub line_count: u32,
    pub bounds_min: [f32; 3],
    pub bounds_max: [f32; 3],
}

#[repr(C)]
pub struct DwgMaterial {
    pub name: *const c_char,
    pub base_color: [f32; 4],
    pub metallic: f32,
    pub roughness: f32,
    pub texture: i32,
    pub normal_texture: i32,
    pub flags: u32,
    pub normal_uv_rotation: f32,
    pub normal_uv_offset: [f32; 2],
    pub normal_uv_scale: [f32; 2],
}

#[repr(C)]
pub struct DwgTexture {
    pub name: *const c_char,
    pub mime_type: *const c_char,
    pub data: *const u8,
    pub size: u64,
}

#[repr(C)]
pub struct DwgNode {
    pub name: *const c_char,
    pub block_name: *const c_char,
    pub handle: *const c_char,
    pub parent: i32,
    pub mesh: i32,
    pub local: [f64; 16],
    pub world: [f64; 16],
}

#[repr(C)]
pub struct DwgInstance {
    pub node: u32,
    pub mesh: u32,
    pub world: [f32; 16],
}

#[repr(C)]
pub struct DwgSceneData {
    pub size: u32,
    pub flags: u32,
    pub mesh_count: u32,
    pub material_count: u32,
    pub texture_count: u32,
    pub node_count: u32,
    pub instance_count: u32,
    pub warning_count: u32,
    pub meshes: *const DwgMesh,
    pub materials: *const DwgMaterial,
    pub textures: *const DwgTexture,
    pub nodes: *const DwgNode,
    pub instances: *const DwgInstance,
    pub warnings: *const *const c_char,
    pub summary: *const c_char,
    pub bounds_min: [f64; 3],
    pub bounds_max: [f64; 3],
    pub unit_scale: f64,
    pub insunits: i32,
    pub reserved: u32,
    pub triangles_stored: u64,
    pub triangles_drawn: u64,
    pub line_segments: u64,
    pub block_instances: u64,
    pub edge_count: u64,
}

/// What a `DwgScene*` points to. The arrays the `Dwg*` structs point into
/// are owned here; none of them is touched after the scene is built.
pub struct DwgScene {
    data: DwgSceneData,
    _meshes: Vec<DwgMesh>,
    _mesh_data: Vec<MeshData>,
    _submeshes: Vec<Vec<DwgSubmesh>>,
    _materials: Vec<DwgMaterial>,
    _textures: Vec<DwgTexture>,
    _nodes: Vec<DwgNode>,
    _instances: Vec<DwgInstance>,
    _strings: Vec<CString>,
    _warnings: Vec<*const c_char>,
    /// Texture data points into its binary buffer.
    writer: GltfWriter,
    glb: OnceLock<Result<Vec<u8>, String>>,
}

// The raw pointers point into the scene's own, immutable data.
unsafe impl Send for DwgScene {}
unsafe impl Sync for DwgScene {}

thread_local! {
    static LAST_ERROR: RefCell<CString> = RefCell::new(CString::default());
}

fn set_error(message: &str) {
    LAST_ERROR.with_borrow_mut(|e| *e = c_string(message));
}

fn c_string(s: &str) -> CString {
    CString::new(s.replace('\0', "")).expect("no NUL left")
}

/// Run `f`, turning a panic into [`DWG_ERR_INTERNAL`] and an error into its
/// message; the last error is cleared on success.
fn guard(f: impl FnOnce() -> Result<(), (i32, String)>) -> i32 {
    let result = catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|p| Err(panic_failure(p)));
    match result {
        Ok(()) => {
            set_error("");
            DWG_OK
        }
        Err((code, message)) => {
            set_error(&message);
            code
        }
    }
}

fn panic_failure(payload: Box<dyn std::any::Any + Send>) -> (i32, String) {
    let what = payload
        .downcast_ref::<&str>()
        .map(|s| s.to_string())
        .or_else(|| payload.downcast_ref::<String>().cloned())
        .unwrap_or_else(|| "unknown".into());
    (DWG_ERR_INTERNAL, format!("internal error: {what}"))
}

// == exported functions =====================================================

/// The library version (UTF-8, static).
#[unsafe(no_mangle)]
pub extern "C" fn dwg_version() -> *const c_char {
    static VERSION: &CStr =
        match CStr::from_bytes_with_nul(concat!(env!("CARGO_PKG_VERSION"), "\0").as_bytes()) {
            Ok(v) => v,
            Err(_) => panic!("version has a NUL"),
        };
    VERSION.as_ptr()
}

#[unsafe(no_mangle)]
pub extern "C" fn dwg_abi_version() -> u32 {
    DWG_ABI
}

/// The message of the last failed call on this thread ("" after a call that
/// succeeded), valid until the next call on this thread.
#[unsafe(no_mangle)]
pub extern "C" fn dwg_last_error() -> *const c_char {
    LAST_ERROR.with_borrow(|e| e.as_ptr())
}

/// Fill `options` with the defaults (all zero but `size`).
///
/// # Safety
/// `options` is NULL or points to a `DwgOptions`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dwg_options_init(options: *mut DwgOptions) {
    if !options.is_null() {
        unsafe { options.write(default_options()) };
    }
}

/// Convert the DWG file at `path` (UTF-8). Material images are looked up
/// next to it.
///
/// # Safety
/// `path` is a NUL-terminated string; `options` is NULL or points to a
/// `DwgOptions` of at least its `size`; `out` points to a `DwgScene*`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dwg_scene_open_file(
    path: *const c_char,
    options: *const DwgOptions,
    out: *mut *mut DwgScene,
) -> i32 {
    guard(|| {
        let out = out_ptr(out)?;
        if path.is_null() {
            return Err(argument("path is NULL"));
        }
        let path = unsafe { CStr::from_ptr(path) }
            .to_str()
            .map_err(|_| argument("path is not UTF-8"))?
            .to_owned();
        let options = unsafe { read_options(options) }?;
        let data =
            std::fs::read(&path).map_err(|e| (DWG_ERR_IO, format!("cannot read {path}: {e}")))?;
        let scene = on_big_stack(move || {
            let drawing =
                dwg::read_bytes(data, &path).map_err(|e| (DWG_ERR_READ, e.to_string()))?;
            build_scene(Document::new(drawing, Some(Path::new(&path))), &options)
        })?;
        unsafe { out.write(Box::into_raw(Box::new(scene))) };
        Ok(())
    })
}

/// Convert a DWG file image of `len` bytes. Material images kept in
/// separate files are left out.
///
/// # Safety
/// `data` points to `len` readable bytes; `options` and `out` as for
/// [`dwg_scene_open_file`].
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dwg_scene_open_memory(
    data: *const u8,
    len: usize,
    options: *const DwgOptions,
    out: *mut *mut DwgScene,
) -> i32 {
    guard(|| {
        let out = out_ptr(out)?;
        if data.is_null() {
            return Err(argument("data is NULL"));
        }
        let options = unsafe { read_options(options) }?;
        let data = unsafe { std::slice::from_raw_parts(data, len) }.to_vec();
        let scene = on_big_stack(move || {
            let drawing =
                dwg::read_bytes(data, "drawing.dwg").map_err(|e| (DWG_ERR_READ, e.to_string()))?;
            build_scene(
                Document::with_texture_files(drawing, Box::new(|_| None)),
                &options,
            )
        })?;
        unsafe { out.write(Box::into_raw(Box::new(scene))) };
        Ok(())
    })
}

/// Release a scene and everything its data points to. NULL is ignored.
///
/// # Safety
/// `scene` is NULL or comes from `dwg_scene_open_*` and was not freed.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dwg_scene_free(scene: *mut DwgScene) {
    if !scene.is_null() {
        let _ = catch_unwind(AssertUnwindSafe(|| drop(unsafe { Box::from_raw(scene) })));
    }
}

/// The scene's arrays; NULL for a NULL scene.
///
/// # Safety
/// `scene` is NULL or a live scene.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dwg_scene_data(scene: *const DwgScene) -> *const DwgSceneData {
    match unsafe { scene.as_ref() } {
        Some(s) => &s.data,
        None => std::ptr::null(),
    }
}

/// The scene as a GLB file, as `dwg2gltf` writes it with the same options.
/// Made on the first call and kept with the scene.
///
/// # Safety
/// `scene` is a live scene; `data` and `len` point to writable variables.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dwg_scene_to_glb(
    scene: *const DwgScene,
    data: *mut *const u8,
    len: *mut usize,
) -> i32 {
    guard(|| {
        let scene = unsafe { scene.as_ref() }.ok_or_else(|| argument("scene is NULL"))?;
        if data.is_null() || len.is_null() {
            return Err(argument("data or len is NULL"));
        }
        let glb = scene
            .glb
            .get_or_init(|| scene.writer.clone().to_glb().map_err(|e| e.to_string()))
            .as_ref()
            .map_err(|e| (DWG_ERR_NON_FINITE, e.clone()))?;
        unsafe {
            data.write(glb.as_ptr());
            len.write(glb.len());
        }
        Ok(())
    })
}

// == options ================================================================

fn default_options() -> DwgOptions {
    DwgOptions {
        size: size_of::<DwgOptions>() as u32,
        flags: 0,
        units: DWG_UNITS_DRAWING,
        crease_angle: 0.0,
        max_instances: 0,
        max_triangles: 0,
        max_line_points: 0,
        reserved: 0,
    }
}

fn argument(message: &str) -> (i32, String) {
    (DWG_ERR_ARGUMENT, message.into())
}

fn out_ptr(out: *mut *mut DwgScene) -> Result<*mut *mut DwgScene, (i32, String)> {
    if out.is_null() {
        return Err(argument("out is NULL"));
    }
    unsafe { out.write(std::ptr::null_mut()) };
    Ok(out)
}

/// The options a viewer scene is converted with, and the crease angle.
#[derive(Clone)]
struct SceneOptions {
    convert: Options,
    edges: Option<f64>,
}

/// The caller's options, its `size` bytes of them; NULL for the defaults.
unsafe fn read_options(p: *const DwgOptions) -> Result<SceneOptions, (i32, String)> {
    let mut o = default_options();
    if !p.is_null() {
        let size = unsafe { p.cast::<u32>().read_unaligned() } as usize;
        if size < 8 {
            return Err(argument("DwgOptions.size is too small"));
        }
        let n = size.min(size_of::<DwgOptions>());
        unsafe {
            std::ptr::copy_nonoverlapping(p.cast::<u8>(), (&raw mut o).cast::<u8>(), n);
        }
    }
    scene_options(&o)
}

fn scene_options(o: &DwgOptions) -> Result<SceneOptions, (i32, String)> {
    let known = DWG_OPT_NO_LINES
        | DWG_OPT_PLAN_SYMBOLS
        | DWG_OPT_NO_EDGES
        | DWG_OPT_Z_UP
        | DWG_OPT_EMPTY_IS_ERROR;
    if o.flags & !known != 0 {
        return Err(argument(&format!(
            "unknown option flags 0x{:x}",
            o.flags & !known
        )));
    }
    let (units, auto_units) = match o.units {
        DWG_UNITS_DRAWING => (None, false),
        DWG_UNITS_AUTO => (None, true),
        1..=24 => (Some(o.units), false),
        u => return Err(argument(&format!("units {u} is not an INSUNITS code"))),
    };
    let crease = match o.crease_angle {
        0.0 => DEFAULT_CREASE_DEG,
        a if a > 0.0 && a <= 180.0 => a as f64,
        a => return Err(argument(&format!("crease angle {a} is not in 0..180"))),
    };
    let defaults = Limits::default();
    let limit = |v: u32, d: usize| if v == 0 { d } else { v as usize };
    let flag = |f: u32| o.flags & f != 0;
    Ok(SceneOptions {
        convert: Options {
            lines: !flag(DWG_OPT_NO_LINES),
            plan_symbols: flag(DWG_OPT_PLAN_SYMBOLS),
            units,
            auto_units,
            y_up: !flag(DWG_OPT_Z_UP),
            compress: false,
            allow_empty: !flag(DWG_OPT_EMPTY_IS_ERROR),
            limits: Limits {
                instances: limit(o.max_instances, defaults.instances),
                triangles: limit(o.max_triangles, defaults.triangles),
                line_points: limit(o.max_line_points, defaults.line_points),
            },
        },
        edges: (!flag(DWG_OPT_NO_EDGES)).then_some(crease),
    })
}

/// Run `f` on a thread with a 256 MiB stack (reserved, not committed):
/// conversion recurses once per block nesting level and ACIS subtype level,
/// and a host application's thread may have only 1 MiB.
fn on_big_stack<T: Send + 'static>(
    f: impl FnOnce() -> Result<T, (i32, String)> + Send + 'static,
) -> Result<T, (i32, String)> {
    let thread = std::thread::Builder::new()
        .name("dwg2gltf".into())
        .stack_size(256 << 20)
        .spawn(move || catch_unwind(AssertUnwindSafe(f)).unwrap_or_else(|p| Err(panic_failure(p))))
        .map_err(|e| (DWG_ERR_INTERNAL, format!("cannot start a thread: {e}")))?;
    thread.join().unwrap_or_else(|p| Err(panic_failure(p)))
}

// == building the scene =====================================================

fn convert_error(e: Error) -> (i32, String) {
    let code = match &e {
        Error::Read(_) => DWG_ERR_READ,
        Error::NoModelSpace => DWG_ERR_NO_MODEL_SPACE,
        Error::Empty(_) => DWG_ERR_EMPTY,
        Error::NonFinite => DWG_ERR_NON_FINITE,
        Error::Io(_) => DWG_ERR_IO,
        _ => DWG_ERR_INTERNAL,
    };
    (code, e.to_string())
}

/// One mesh's arrays.
#[derive(Default)]
struct MeshData {
    positions: Vec<f32>,
    normals: Vec<f32>,
    uvs: Option<Vec<f32>>,
    indices: Vec<u32>,
    edges: Vec<u32>,
}

/// Reads the writer's accessors.
struct Buffers<'a> {
    doc: &'a Map<String, Value>,
    bin: &'a [u8],
}

impl Buffers<'_> {
    /// The bytes of accessor `index`, its component type and count.
    fn accessor(&self, index: &Value) -> Result<(&[u8], u64, usize), String> {
        let bad = || format!("unexpected accessor {index}");
        let acc = &self.doc["accessors"][index.as_u64().ok_or_else(bad)? as usize];
        let view = &self.doc["bufferViews"][acc["bufferView"].as_u64().ok_or_else(bad)? as usize];
        let start = view["byteOffset"].as_u64().unwrap_or(0) as usize
            + acc["byteOffset"].as_u64().unwrap_or(0) as usize;
        let len = view["byteLength"].as_u64().ok_or_else(bad)? as usize;
        let count = acc["count"].as_u64().ok_or_else(bad)? as usize;
        if view.get("byteStride").is_some() {
            return Err(bad());
        }
        let bytes = self.bin.get(start..start + len).ok_or_else(bad)?;
        Ok((bytes, acc["componentType"].as_u64().unwrap_or(0), count))
    }

    fn floats(&self, index: &Value, n: usize) -> Result<Vec<f32>, String> {
        let (bytes, ctype, count) = self.accessor(index)?;
        if ctype != 5126 || bytes.len() < count * n * 4 {
            return Err(format!("accessor {index} is not float"));
        }
        Ok(bytes[..count * n * 4]
            .chunks_exact(4)
            .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
            .collect())
    }

    fn indices(&self, index: &Value) -> Result<Vec<u32>, String> {
        let (bytes, ctype, count) = self.accessor(index)?;
        match ctype {
            5123 if bytes.len() >= count * 2 => Ok(bytes[..count * 2]
                .chunks_exact(2)
                .map(|b| u16::from_le_bytes([b[0], b[1]]) as u32)
                .collect()),
            5125 if bytes.len() >= count * 4 => Ok(bytes[..count * 4]
                .chunks_exact(4)
                .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
                .collect()),
            _ => Err(format!("accessor {index} is not an index list")),
        }
    }
}

/// A converted mesh before the C structs point into it.
struct BuiltMesh {
    name: String,
    data: MeshData,
    submeshes: Vec<DwgSubmesh>,
    triangles: u32,
    lines: u32,
}

fn build_mesh(buffers: &Buffers, mesh: &Value) -> Result<BuiltMesh, String> {
    let mut out = BuiltMesh {
        name: mesh["name"].as_str().unwrap_or("").into(),
        data: MeshData::default(),
        submeshes: vec![],
        triangles: 0,
        lines: 0,
    };
    let prims = mesh["primitives"].as_array().cloned().unwrap_or_default();
    let textured = prims
        .iter()
        .any(|p| p["attributes"].get("TEXCOORD_0").is_some());
    let d = &mut out.data;
    if textured {
        d.uvs = Some(vec![]);
    }
    for p in &prims {
        let attrs = &p["attributes"];
        let positions = buffers.floats(&attrs["POSITION"], 3)?;
        let n = positions.len() / 3;
        let first_vertex = (d.positions.len() / 3) as u32;
        d.positions.extend_from_slice(&positions);
        match attrs.get("NORMAL") {
            Some(a) => d.normals.extend(buffers.floats(a, 3)?),
            None => d.normals.resize(d.positions.len(), 0.0),
        }
        if let Some(uvs) = &mut d.uvs {
            match attrs.get("TEXCOORD_0") {
                Some(a) => uvs.extend(buffers.floats(a, 2)?),
                None => uvs.resize(n * 2 + first_vertex as usize * 2, 0.0),
            }
        }
        if d.normals.len() != d.positions.len()
            || d.uvs
                .as_ref()
                .is_some_and(|u| u.len() / 2 != d.positions.len() / 3)
        {
            return Err(format!("mesh {}: attribute counts differ", out.name));
        }
        let indices = buffers.indices(&p["indices"])?;
        if indices.iter().any(|&i| i as usize >= n) {
            return Err(format!("mesh {}: index out of range", out.name));
        }
        let first_index = d.indices.len() as u32;
        d.indices.extend(indices.iter().map(|&i| i + first_vertex));
        let topology = match p["mode"].as_u64() {
            Some(1) => DWG_LINES,
            _ => DWG_TRIANGLES,
        };
        if topology == DWG_LINES {
            out.lines += indices.len() as u32 / 2;
        } else {
            out.triangles += indices.len() as u32 / 3;
        }
        out.submeshes.push(DwgSubmesh {
            topology,
            material: p["material"].as_u64().unwrap_or(0) as u32,
            first_index,
            index_count: indices.len() as u32,
            first_vertex,
            vertex_count: n as u32,
        });
    }
    Ok(out)
}

/// The visible edges of each mesh's triangles, meshes shared out over the
/// available cores.
fn add_edges(meshes: &mut [BuiltMesh], crease: f64) {
    let threads = std::thread::available_parallelism()
        .map_or(1, |n| n.get())
        .min(meshes.len());
    let next = AtomicUsize::new(0);
    let shared: &[BuiltMesh] = meshes;
    let results: Vec<Vec<(usize, Vec<u32>)>> = std::thread::scope(|s| {
        let workers: Vec<_> = (0..threads)
            .map(|_| {
                s.spawn(|| {
                    let mut done = vec![];
                    loop {
                        let i = next.fetch_add(1, Ordering::Relaxed);
                        let Some(m) = shared.get(i) else { break };
                        let positions: Vec<[f32; 3]> = m
                            .data
                            .positions
                            .chunks_exact(3)
                            .map(|p| [p[0], p[1], p[2]])
                            .collect();
                        let triangles: Vec<u32> = m
                            .submeshes
                            .iter()
                            .filter(|s| s.topology == DWG_TRIANGLES)
                            .flat_map(|s| {
                                let a = s.first_index as usize;
                                m.data.indices[a..a + s.index_count as usize]
                                    .iter()
                                    .copied()
                            })
                            .collect();
                        let normals: Vec<[f32; 3]> = m
                            .data
                            .normals
                            .chunks_exact(3)
                            .map(|n| [n[0], n[1], n[2]])
                            .collect();
                        let edges = visible_edges(&positions, Some(&normals), &triangles, crease);
                        done.push((i, edges.into_iter().flatten().collect()));
                    }
                    done
                })
            })
            .collect();
        workers
            .into_iter()
            .map(|w| w.join().unwrap_or_default())
            .collect()
    });
    for (i, edges) in results.into_iter().flatten() {
        meshes[i].data.edges = edges;
    }
}

/// Column-major 4x4 product a·b.
fn mul(a: &[f64; 16], b: &[f64; 16]) -> [f64; 16] {
    std::array::from_fn(|i| {
        let (c, r) = (i / 4, i % 4);
        (0..4).map(|k| a[k * 4 + r] * b[c * 4 + k]).sum()
    })
}

const IDENTITY: [f64; 16] = [
    1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0, 0.0, 0.0, 0.0, 0.0, 1.0,
];

fn build_scene(doc: Document, options: &SceneOptions) -> Result<DwgScene, (i32, String)> {
    let (writer, stats) =
        converter::convert_document(&doc, &options.convert).map_err(convert_error)?;
    let internal = |e: String| (DWG_ERR_INTERNAL, e);
    let gdoc = writer.document();
    let buffers = Buffers {
        doc: gdoc,
        bin: writer.binary(),
    };
    let list = |key: &str| {
        gdoc.get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    let mut strings: Vec<CString> = vec![];
    let mut string = |s: &str| -> *const c_char {
        let c = c_string(s);
        let p = c.as_ptr();
        strings.push(c);
        p
    };

    // meshes
    let mut built: Vec<BuiltMesh> = list("meshes")
        .iter()
        .map(|m| build_mesh(&buffers, m))
        .collect::<Result<_, _>>()
        .map_err(internal)?;
    if let Some(crease) = options.edges {
        add_edges(&mut built, crease);
    }

    // nodes: parents before their children
    let gnodes = list("nodes");
    let roots = gdoc["scenes"][0]["nodes"]
        .as_array()
        .cloned()
        .unwrap_or_default();
    let mut nodes: Vec<DwgNode> = vec![];
    let mut stack: Vec<(usize, i32)> = roots
        .iter()
        .rev()
        .filter_map(|n| Some((n.as_u64()? as usize, -1)))
        .collect();
    while let Some((i, parent)) = stack.pop() {
        let n = gnodes
            .get(i)
            .ok_or_else(|| internal(format!("node {i} is missing")))?;
        if nodes.len() > gnodes.len() {
            return Err(internal("the node tree has a cycle".into()));
        }
        let local = match n["matrix"].as_array() {
            Some(m) if m.len() == 16 => std::array::from_fn(|k| m[k].as_f64().unwrap_or(0.0)),
            _ => IDENTITY,
        };
        let world = match parent {
            -1 => local,
            p => mul(&nodes[p as usize].world, &local),
        };
        let me = nodes.len() as i32;
        nodes.push(DwgNode {
            name: string(n["name"].as_str().unwrap_or("")),
            block_name: string(n["extras"]["block"].as_str().unwrap_or("")),
            handle: string(n["extras"]["handle"].as_str().unwrap_or("")),
            parent,
            mesh: n["mesh"].as_u64().map_or(-1, |m| m as i32),
            local,
            world,
        });
        for c in n["children"].as_array().into_iter().flatten().rev() {
            if let Some(c) = c.as_u64() {
                stack.push((c as usize, me));
            }
        }
    }
    let instances: Vec<DwgInstance> = nodes
        .iter()
        .enumerate()
        .filter(|(_, n)| n.mesh >= 0 && (n.mesh as usize) < built.len())
        .map(|(i, n)| DwgInstance {
            node: i as u32,
            mesh: n.mesh as u32,
            world: n.world.map(|v| v as f32),
        })
        .collect();

    // materials and textures
    let materials: Vec<DwgMaterial> = list("materials")
        .iter()
        .map(|m| {
            let pbr = &m["pbrMetallicRoughness"];
            let f = |v: &Value, d: f64| v.as_f64().unwrap_or(d) as f32;
            let color = &pbr["baseColorFactor"];
            let normal = &m["normalTexture"];
            let t = &normal["extensions"]["KHR_texture_transform"];
            let mut flags = 0;
            if m["doubleSided"].as_bool() == Some(true) {
                flags |= DWG_MAT_DOUBLE_SIDED;
            }
            if m["alphaMode"].as_str() == Some("BLEND") {
                flags |= DWG_MAT_BLEND;
            }
            if m["extensions"].get("KHR_materials_unlit").is_some() {
                flags |= DWG_MAT_UNLIT;
            }
            DwgMaterial {
                name: string(m["name"].as_str().unwrap_or("")),
                base_color: std::array::from_fn(|k| f(&color[k], 1.0)),
                metallic: f(&pbr["metallicFactor"], 1.0),
                roughness: f(&pbr["roughnessFactor"], 1.0),
                texture: pbr["baseColorTexture"]["index"]
                    .as_i64()
                    .map_or(-1, |t| t as i32),
                normal_texture: normal["index"].as_i64().map_or(-1, |t| t as i32),
                flags,
                normal_uv_rotation: f(&t["rotation"], 0.0),
                normal_uv_offset: [f(&t["offset"][0], 0.0), f(&t["offset"][1], 0.0)],
                normal_uv_scale: [f(&t["scale"][0], 1.0), f(&t["scale"][1], 1.0)],
            }
        })
        .collect();
    let images = list("images");
    let mut textures: Vec<DwgTexture> = vec![];
    for t in list("textures") {
        let image = t["source"]
            .as_u64()
            .and_then(|i| images.get(i as usize))
            .ok_or_else(|| internal("texture without image".into()))?;
        let view = &gdoc["bufferViews"][image["bufferView"].as_u64().unwrap_or(u64::MAX) as usize];
        let start = view["byteOffset"].as_u64().unwrap_or(0) as usize;
        let len = view["byteLength"].as_u64().unwrap_or(0) as usize;
        let bytes = buffers
            .bin
            .get(start..start + len)
            .ok_or_else(|| internal("texture outside the buffer".into()))?;
        textures.push(DwgTexture {
            name: string(image["name"].as_str().unwrap_or("")),
            mime_type: string(image["mimeType"].as_str().unwrap_or("")),
            data: bytes.as_ptr(),
            size: bytes.len() as u64,
        });
    }

    let summary = string(&stats.to_string());
    let warnings: Vec<*const c_char> = stats.warnings.iter().map(|w| string(w)).collect();
    let mut meshes = vec![];
    let mut mesh_data = vec![];
    let mut submeshes = vec![];
    for m in built {
        let (lo, hi) = m.data.positions.chunks_exact(3).fold(
            ([f32::INFINITY; 3], [f32::NEG_INFINITY; 3]),
            |(lo, hi), p| {
                (
                    std::array::from_fn(|k| lo[k].min(p[k])),
                    std::array::from_fn(|k| hi[k].max(p[k])),
                )
            },
        );
        let d = &m.data;
        meshes.push(DwgMesh {
            name: string(&m.name),
            positions: d.positions.as_ptr(),
            normals: d.normals.as_ptr(),
            uvs: d.uvs.as_ref().map_or(std::ptr::null(), |u| u.as_ptr()),
            indices: d.indices.as_ptr(),
            submeshes: m.submeshes.as_ptr(),
            edges: d.edges.as_ptr(),
            vertex_count: (d.positions.len() / 3) as u32,
            index_count: d.indices.len() as u32,
            submesh_count: m.submeshes.len() as u32,
            edge_count: (d.edges.len() / 2) as u32,
            triangle_count: m.triangles,
            line_count: m.lines,
            bounds_min: lo,
            bounds_max: hi,
        });
        mesh_data.push(m.data);
        submeshes.push(m.submeshes);
    }

    let bounds = writer.bounds();
    let (bounds_min, bounds_max) = bounds.unwrap_or(([0.0; 3], [0.0; 3]));
    let unit_scale = match nodes.first() {
        // the root node holds the scale (and the turn to Y-up)
        Some(root) => (0..3).map(|k| root.local[k].powi(2)).sum::<f64>().sqrt(),
        None => doc.unit_scale(options.convert.units),
    };
    let mut flags = 0;
    if !options.convert.y_up {
        flags |= DWG_SCENE_Z_UP;
    }
    if bounds.is_some() {
        flags |= DWG_SCENE_HAS_BOUNDS;
    }
    let data = DwgSceneData {
        size: size_of::<DwgSceneData>() as u32,
        flags,
        mesh_count: meshes.len() as u32,
        material_count: materials.len() as u32,
        texture_count: textures.len() as u32,
        node_count: nodes.len() as u32,
        instance_count: instances.len() as u32,
        warning_count: warnings.len() as u32,
        meshes: meshes.as_ptr(),
        materials: materials.as_ptr(),
        textures: textures.as_ptr(),
        nodes: nodes.as_ptr(),
        instances: instances.as_ptr(),
        warnings: warnings.as_ptr(),
        summary,
        bounds_min,
        bounds_max,
        unit_scale,
        insunits: doc.insunits(),
        reserved: 0,
        triangles_stored: stats.triangles as u64,
        triangles_drawn: instances
            .iter()
            .map(|i| meshes[i.mesh as usize].triangle_count as u64)
            .sum(),
        line_segments: stats.lines as u64,
        block_instances: stats.instances as u64,
        edge_count: meshes.iter().map(|m| m.edge_count as u64).sum(),
    };
    Ok(DwgScene {
        data,
        _meshes: meshes,
        _mesh_data: mesh_data,
        _submeshes: submeshes,
        _materials: materials,
        _textures: textures,
        _nodes: nodes,
        _instances: instances,
        _strings: strings,
        _warnings: warnings,
        writer,
        glb: OnceLock::new(),
    })
}

#[cfg(test)]
mod tests;
