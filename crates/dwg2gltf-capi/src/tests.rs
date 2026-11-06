// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! The C interface called from Rust, and the struct layouts checked against
//! include/dwg2gltf.h (and the Delphi unit, when Free Pascal is installed).

use super::*;
use std::fmt::Write as _;
use std::path::PathBuf;
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn bricscad(name: &str) -> PathBuf {
    root().join("tests/bricscad/dwg/v27").join(name)
}

fn last_error() -> String {
    unsafe { CStr::from_ptr(dwg_last_error()) }
        .to_string_lossy()
        .into_owned()
}

/// A scene, freed when dropped.
struct Scene(*mut DwgScene);

impl Drop for Scene {
    fn drop(&mut self) {
        unsafe { dwg_scene_free(self.0) };
    }
}

impl Scene {
    fn data(&self) -> &DwgSceneData {
        unsafe { &*dwg_scene_data(self.0) }
    }

    fn meshes(&self) -> &[DwgMesh] {
        let d = self.data();
        unsafe { std::slice::from_raw_parts(d.meshes, d.mesh_count as usize) }
    }

    fn nodes(&self) -> &[DwgNode] {
        let d = self.data();
        unsafe { std::slice::from_raw_parts(d.nodes, d.node_count as usize) }
    }

    fn instances(&self) -> &[DwgInstance] {
        let d = self.data();
        unsafe { std::slice::from_raw_parts(d.instances, d.instance_count as usize) }
    }

    fn glb(&self) -> &[u8] {
        let (mut data, mut len) = (std::ptr::null(), 0);
        assert_eq!(
            unsafe { dwg_scene_to_glb(self.0, &mut data, &mut len) },
            DWG_OK
        );
        unsafe { std::slice::from_raw_parts(data, len) }
    }
}

fn open(path: &Path, options: Option<&DwgOptions>) -> Result<Scene, (i32, String)> {
    let c = CString::new(path.to_str().unwrap()).unwrap();
    let mut out = std::ptr::null_mut();
    let opts = options.map_or(std::ptr::null(), |o| o as *const _);
    match unsafe { dwg_scene_open_file(c.as_ptr(), opts, &mut out) } {
        DWG_OK => {
            assert!(!out.is_null());
            assert_eq!(last_error(), "");
            Ok(Scene(out))
        }
        code => {
            assert!(out.is_null());
            Err((code, last_error()))
        }
    }
}

fn str_of(p: *const c_char) -> String {
    unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned()
}

#[test]
fn version_and_options() {
    assert_eq!(str_of(dwg_version()), env!("CARGO_PKG_VERSION"));
    assert_eq!(dwg_abi_version(), DWG_ABI);
    let mut o = DwgOptions {
        size: 0,
        flags: 99,
        units: 5,
        crease_angle: 1.0,
        max_instances: 1,
        max_triangles: 1,
        max_line_points: 1,
        reserved: 1,
    };
    unsafe { dwg_options_init(&mut o) };
    assert_eq!(o, default_options());
    assert_eq!(o.size as usize, size_of::<DwgOptions>());
    unsafe { dwg_options_init(std::ptr::null_mut()) };

    let defaults = scene_options(&o).unwrap();
    let d = Options {
        allow_empty: true,
        ..Options::default()
    };
    let c = &defaults.convert;
    assert_eq!(
        (
            c.lines,
            c.plan_symbols,
            c.units,
            c.auto_units,
            c.y_up,
            c.compress,
            c.allow_empty
        ),
        (
            d.lines,
            d.plan_symbols,
            d.units,
            d.auto_units,
            d.y_up,
            d.compress,
            d.allow_empty
        )
    );
    assert_eq!(c.limits, Limits::default());
    assert_eq!(defaults.edges, Some(DEFAULT_CREASE_DEG));

    let with = |f: &dyn Fn(&mut DwgOptions)| {
        let mut o = default_options();
        f(&mut o);
        scene_options(&o)
    };
    let o = with(&|o| {
        o.flags = DWG_OPT_NO_LINES | DWG_OPT_PLAN_SYMBOLS | DWG_OPT_NO_EDGES | DWG_OPT_Z_UP;
        o.units = DWG_UNITS_AUTO;
        o.max_triangles = 7;
    })
    .unwrap();
    assert!(!o.convert.lines && o.convert.plan_symbols && !o.convert.y_up);
    assert!(o.convert.auto_units && o.convert.units.is_none() && o.edges.is_none());
    assert_eq!(o.convert.limits.triangles, 7);
    assert_eq!(with(&|o| o.units = 4).unwrap().convert.units, Some(4));
    assert_eq!(with(&|o| o.crease_angle = 45.0).unwrap().edges, Some(45.0));
    assert!(
        !with(&|o| o.flags = DWG_OPT_EMPTY_IS_ERROR)
            .unwrap()
            .convert
            .allow_empty
    );
    for bad in [
        with(&|o| o.units = 25),
        with(&|o| o.units = -2),
        with(&|o| o.flags = 64),
        with(&|o| o.crease_angle = -1.0),
        with(&|o| o.crease_angle = f32::NAN),
        with(&|o| o.crease_angle = 181.0),
    ] {
        assert_eq!(bad.err().unwrap().0, DWG_ERR_ARGUMENT);
    }
}

#[test]
fn options_size_is_honoured() {
    // an older caller's struct, cut after `units`: the rest is default
    let mut o = default_options();
    o.size = 12;
    o.units = 4;
    o.crease_angle = 1000.0; // past `size`: not read
    let read = unsafe { read_options(&o) }.unwrap();
    assert_eq!(read.convert.units, Some(4));
    assert_eq!(read.edges, Some(DEFAULT_CREASE_DEG));
    o.size = 4;
    assert_eq!(
        unsafe { read_options(&o) }.err().unwrap().0,
        DWG_ERR_ARGUMENT
    );
    assert!(unsafe { read_options(std::ptr::null()) }.is_ok());
}

#[test]
fn errors() {
    let mut out = std::ptr::null_mut();
    let missing = CString::new("/nonexistent/drawing.dwg").unwrap();
    let code = unsafe { dwg_scene_open_file(missing.as_ptr(), std::ptr::null(), &mut out) };
    assert_eq!(code, DWG_ERR_IO);
    assert!(out.is_null() && last_error().contains("cannot read"));
    let code = unsafe { dwg_scene_open_file(std::ptr::null(), std::ptr::null(), &mut out) };
    assert_eq!(code, DWG_ERR_ARGUMENT);
    let code =
        unsafe { dwg_scene_open_file(missing.as_ptr(), std::ptr::null(), std::ptr::null_mut()) };
    assert_eq!(code, DWG_ERR_ARGUMENT);
    let junk = b"this is not a drawing ".repeat(10);
    let code =
        unsafe { dwg_scene_open_memory(junk.as_ptr(), junk.len(), std::ptr::null(), &mut out) };
    assert_eq!(code, DWG_ERR_READ, "{}", last_error());
    assert!(out.is_null());
    let code = unsafe { dwg_scene_open_memory(std::ptr::null(), 0, std::ptr::null(), &mut out) };
    assert_eq!(code, DWG_ERR_ARGUMENT);
    let (mut data, mut len) = (std::ptr::null(), 0);
    assert_eq!(
        unsafe { dwg_scene_to_glb(std::ptr::null(), &mut data, &mut len) },
        DWG_ERR_ARGUMENT
    );
    assert!(unsafe { dwg_scene_data(std::ptr::null()) }.is_null());
    unsafe { dwg_scene_free(std::ptr::null_mut()) };
}

#[test]
fn panics_become_codes() {
    let code = guard(|| panic!("boom"));
    assert_eq!(code, DWG_ERR_INTERNAL);
    assert!(last_error().contains("boom"));
    let r: Result<(), _> = on_big_stack(|| panic!("deep"));
    assert_eq!(r.unwrap_err().0, DWG_ERR_INTERNAL);
}

/// Every edge index is a vertex of its mesh, every index a vertex of its
/// submesh, and parents come first.
fn check_consistency(s: &Scene) {
    let d = s.data();
    for m in s.meshes() {
        let n = m.vertex_count as usize;
        let idx = unsafe { std::slice::from_raw_parts(m.indices, m.index_count as usize) };
        let subs = unsafe { std::slice::from_raw_parts(m.submeshes, m.submesh_count as usize) };
        for sm in subs {
            assert!(sm.material < d.material_count);
            let range = sm.first_vertex..sm.first_vertex + sm.vertex_count;
            let a = sm.first_index as usize;
            for &i in &idx[a..a + sm.index_count as usize] {
                assert!(range.contains(&i));
            }
        }
        let edges = unsafe { std::slice::from_raw_parts(m.edges, 2 * m.edge_count as usize) };
        assert!(edges.iter().all(|&e| (e as usize) < n));
        let normals = unsafe { std::slice::from_raw_parts(m.normals, 3 * n) };
        assert!(normals.iter().all(|c| c.is_finite()));
    }
    for (i, node) in s.nodes().iter().enumerate() {
        assert!(node.parent < i as i32);
        assert!(node.mesh < d.mesh_count as i32);
    }
}

#[test]
fn box_scene() {
    let s = open(&bricscad("primitive/box/box_AC1032.dwg"), None).unwrap();
    let d = s.data();
    assert_eq!(d.size as usize, size_of::<DwgSceneData>());
    check_consistency(&s);
    assert!(d.mesh_count >= 1 && d.instance_count >= 1);
    assert_eq!(d.flags & DWG_SCENE_HAS_BOUNDS, DWG_SCENE_HAS_BOUNDS);
    // a box: 12 triangles, 12 visible edges
    assert_eq!(d.triangles_stored, 12);
    assert_eq!(d.triangles_drawn, 12);
    assert_eq!(d.edge_count, 12, "{}", str_of(d.summary));
    let root = &s.nodes()[0];
    assert_eq!(root.parent, -1);
    assert_eq!(str_of(root.name), "ModelSpace");
    // the instance matrix is the node's world matrix, and the scene bounds
    // are the mesh bounds through it
    let inst = &s.instances()[0];
    let node = &s.nodes()[inst.node as usize];
    assert_eq!(inst.world, node.world.map(|v| v as f32));
    let m = &s.meshes()[inst.mesh as usize];
    let w = &node.world;
    let corners: Vec<[f64; 3]> = (0..8)
        .map(|c| {
            let p: [f64; 3] = std::array::from_fn(|k| {
                (if c >> k & 1 == 0 {
                    m.bounds_min[k]
                } else {
                    m.bounds_max[k]
                }) as f64
            });
            std::array::from_fn(|r| w[r] * p[0] + w[4 + r] * p[1] + w[8 + r] * p[2] + w[12 + r])
        })
        .collect();
    for k in 0..3 {
        let lo = corners.iter().map(|p| p[k]).fold(f64::INFINITY, f64::min);
        let hi = corners
            .iter()
            .map(|p| p[k])
            .fold(f64::NEG_INFINITY, f64::max);
        assert!(
            (lo - d.bounds_min[k]).abs() < 1e-5,
            "{k}: {lo} {:?}",
            d.bounds_min
        );
        assert!(
            (hi - d.bounds_max[k]).abs() < 1e-5,
            "{k}: {hi} {:?}",
            d.bounds_max
        );
    }
}

#[test]
fn glb_is_the_converters() {
    for name in [
        "primitive/box/box_AC1032.dwg",
        "fillet/fillet_small/fillet_small_AC1018.dwg",
        "sheet/sheet_mesh/sheet_mesh_AC1027.dwg",
    ] {
        let path = bricscad(name);
        let s = open(&path, None).unwrap();
        let glb = s.glb();
        let doc = Document::new(dwg::read(&path).unwrap(), Some(&path));
        let (writer, _) = converter::convert_document(&doc, &Options::default()).unwrap();
        assert!(glb == writer.to_glb().unwrap().as_slice(), "{name}");
        // made once
        assert_eq!(s.glb().as_ptr(), s.glb().as_ptr());
    }
}

#[test]
fn memory_and_file_agree() {
    let path = bricscad("fillet/fillet_small/fillet_small_AC1032.dwg");
    let a = open(&path, None).unwrap();
    let bytes = std::fs::read(&path).unwrap();
    let mut out = std::ptr::null_mut();
    let code =
        unsafe { dwg_scene_open_memory(bytes.as_ptr(), bytes.len(), std::ptr::null(), &mut out) };
    assert_eq!(code, DWG_OK);
    let b = Scene(out);
    assert!(a.glb() == b.glb());
    assert_eq!(a.data().edge_count, b.data().edge_count);
    assert_eq!(a.data().triangles_stored, b.data().triangles_stored);
}

#[test]
fn options_reach_the_converter() {
    let path = bricscad("primitive/box/box_AC1032.dwg");
    let mut o = default_options();
    o.flags = DWG_OPT_NO_EDGES | DWG_OPT_Z_UP;
    let s = open(&path, Some(&o)).unwrap();
    let d = s.data();
    assert_eq!(d.edge_count, 0);
    assert!(s.meshes().iter().all(|m| m.edge_count == 0));
    assert_eq!(d.flags & DWG_SCENE_Z_UP, DWG_SCENE_Z_UP);
    let y = open(&path, None).unwrap();
    // Z-up and Y-up: the same box, turned
    let (zd, yd) = (d, y.data());
    assert!((zd.bounds_max[2] - yd.bounds_max[1]).abs() < 1e-6);
    assert!((zd.unit_scale - yd.unit_scale).abs() < 1e-12);
    // units in millimeters scale the scene down
    o.flags = 0;
    o.units = 4;
    let mm = open(&path, Some(&o)).unwrap();
    assert!((mm.data().unit_scale - 0.001).abs() < 1e-12);
}

#[test]
fn sheet_and_sweep_edges() {
    // an open sheet has its outline as boundary edges
    let s = open(&bricscad("sheet/sheet_mesh/sheet_mesh_AC1032.dwg"), None).unwrap();
    check_consistency(&s);
    assert!(s.data().edge_count > 0);
    for name in [
        "sweep/sweep_circle_helix/sweep_circle_helix_AC1032.dwg",
        "fillet/fillet_cushion/fillet_cushion_AC1032.dwg",
    ] {
        let s = open(&bricscad(name), None).unwrap();
        check_consistency(&s);
        let d = s.data();
        // far fewer visible edges than triangles: smooth surfaces show none
        assert!(
            d.edge_count > 0 && d.edge_count < d.triangles_stored,
            "{name}"
        );
    }
}

// == layout ===================================================================

type Layout = Vec<(&'static str, usize, Vec<(&'static str, usize)>)>;

macro_rules! layout {
    ($($ty:ident { $($field:ident),* $(,)? })*) => {
        vec![$((
            stringify!($ty),
            size_of::<$ty>(),
            vec![$((stringify!($field), std::mem::offset_of!($ty, $field))),*],
        )),*]
    };
}

fn layout() -> Layout {
    layout! {
        DwgOptions { size, flags, units, crease_angle, max_instances, max_triangles,
            max_line_points, reserved }
        DwgSubmesh { topology, material, first_index, index_count, first_vertex, vertex_count }
        DwgMesh { name, positions, normals, uvs, indices, submeshes, edges, vertex_count,
            index_count, submesh_count, edge_count, triangle_count, line_count, bounds_min,
            bounds_max }
        DwgMaterial { name, base_color, metallic, roughness, texture, normal_texture, flags,
            normal_uv_rotation, normal_uv_offset, normal_uv_scale }
        DwgTexture { name, mime_type, data, size }
        DwgNode { name, block_name, handle, parent, mesh, local, world }
        DwgInstance { node, mesh, world }
        DwgSceneData { size, flags, mesh_count, material_count, texture_count, node_count,
            instance_count, warning_count, meshes, materials, textures, nodes, instances,
            warnings, summary, bounds_min, bounds_max, unit_scale, insunits, reserved,
            triangles_stored, triangles_drawn, line_segments, block_instances, edge_count }
    }
}

const CONSTANTS: &[(&str, i64)] = &[
    ("DWG_ABI", DWG_ABI as i64),
    ("DWG_OK", DWG_OK as i64),
    ("DWG_ERR_ARGUMENT", DWG_ERR_ARGUMENT as i64),
    ("DWG_ERR_IO", DWG_ERR_IO as i64),
    ("DWG_ERR_READ", DWG_ERR_READ as i64),
    ("DWG_ERR_NO_MODEL_SPACE", DWG_ERR_NO_MODEL_SPACE as i64),
    ("DWG_ERR_EMPTY", DWG_ERR_EMPTY as i64),
    ("DWG_ERR_NON_FINITE", DWG_ERR_NON_FINITE as i64),
    ("DWG_ERR_INTERNAL", DWG_ERR_INTERNAL as i64),
    ("DWG_OPT_NO_LINES", DWG_OPT_NO_LINES as i64),
    ("DWG_OPT_PLAN_SYMBOLS", DWG_OPT_PLAN_SYMBOLS as i64),
    ("DWG_OPT_NO_EDGES", DWG_OPT_NO_EDGES as i64),
    ("DWG_OPT_Z_UP", DWG_OPT_Z_UP as i64),
    ("DWG_OPT_EMPTY_IS_ERROR", DWG_OPT_EMPTY_IS_ERROR as i64),
    ("DWG_UNITS_DRAWING", DWG_UNITS_DRAWING as i64),
    ("DWG_UNITS_AUTO", DWG_UNITS_AUTO as i64),
    ("DWG_TRIANGLES", DWG_TRIANGLES as i64),
    ("DWG_LINES", DWG_LINES as i64),
    ("DWG_MAT_DOUBLE_SIDED", DWG_MAT_DOUBLE_SIDED as i64),
    ("DWG_MAT_BLEND", DWG_MAT_BLEND as i64),
    ("DWG_MAT_UNLIT", DWG_MAT_UNLIT as i64),
    ("DWG_SCENE_Z_UP", DWG_SCENE_Z_UP as i64),
    ("DWG_SCENE_HAS_BOUNDS", DWG_SCENE_HAS_BOUNDS as i64),
];

/// "Struct size N" and "Struct.field N" lines, then "NAME value" lines.
fn expected_layout(with_constants: bool) -> String {
    let mut out = String::new();
    for (ty, size, fields) in layout() {
        writeln!(out, "{ty} size {size}").unwrap();
        for (f, off) in fields {
            writeln!(out, "{ty}.{f} {off}").unwrap();
        }
    }
    if with_constants {
        for (name, v) in CONSTANTS {
            writeln!(out, "{name} {v}").unwrap();
        }
    }
    out
}

/// The directory with libdwg2gltf.so of this build profile, built first if
/// it is not there (`cargo test` builds the test harness, not the cdylib).
fn library_dir() -> PathBuf {
    // target/<profile>/deps/dwg2gltf-<hash>
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
    let lib = dir.join(format!(
        "{}dwg2gltf{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    if !lib.exists() {
        let mut cargo = Command::new(std::env::var("CARGO").unwrap_or("cargo".into()));
        cargo.args(["build", "--offline", "-p", "dwg2gltf-capi"]);
        if dir.file_name().is_some_and(|n| n == "release") {
            cargo.arg("--release");
        }
        assert!(cargo.status().unwrap().success());
    }
    assert!(lib.exists(), "{} missing", lib.display());
    dir
}

fn scratch(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("dwg2gltf-capi-{}-{name}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// The same lines from the C header, compiled with -Wall -Werror -pedantic.
#[test]
fn layout_matches_header() {
    let mut c = String::from(
        "#include <stdio.h>\n#include <stddef.h>\n#include \"dwg2gltf.h\"\nint main(void) {\n",
    );
    for (ty, _, fields) in layout() {
        writeln!(
            c,
            "printf(\"{ty} size %lu\\n\", (unsigned long)sizeof({ty}));"
        )
        .unwrap();
        for (f, _) in fields {
            writeln!(
                c,
                "printf(\"{ty}.{f} %lu\\n\", (unsigned long)offsetof({ty}, {f}));"
            )
            .unwrap();
        }
    }
    for (name, _) in CONSTANTS {
        writeln!(c, "printf(\"{name} %lld\\n\", (long long)({name}));").unwrap();
    }
    c.push_str("return 0;\n}\n");
    let dir = scratch("layout");
    std::fs::write(dir.join("layout.c"), c).unwrap();
    let exe = dir.join("layout");
    let status = Command::new(std::env::var("CC").unwrap_or("cc".into()))
        .args(["-std=c99", "-Wall", "-Wextra", "-Werror", "-pedantic", "-I"])
        .arg(root().join("include"))
        .arg(dir.join("layout.c"))
        .arg("-o")
        .arg(&exe)
        .status()
        .expect("a C compiler (cc)");
    assert!(status.success());
    let out = Command::new(&exe).output().unwrap();
    assert_eq!(
        String::from_utf8(out.stdout).unwrap(),
        expected_layout(true)
    );
    // also valid C++
    if let Ok(status) = Command::new("c++")
        .args(["-fsyntax-only", "-Wall", "-Werror", "-x", "c++", "-I"])
        .arg(root().join("include"))
        .arg(dir.join("layout.c"))
        .status()
    {
        assert!(status.success());
    }
    let _ = std::fs::remove_dir_all(dir);
}

/// A Pascal program printing the same lines from the Delphi unit
/// (`bindings/delphi/Dwg2Gltf.pas`).
fn pascal_layout_program() -> String {
    let mut p = String::from(
        "program LayoutCheck;\n{$IFDEF MSWINDOWS}{$APPTYPE CONSOLE}{$ENDIF}\n\
         {$IFDEF FPC}{$MODE DELPHIUNICODE}{$ENDIF}\nuses Dwg2Gltf;\nbegin\n",
    );
    for (ty, _, fields) in layout() {
        let t = format!("T{ty}");
        writeln!(p, "  WriteLn('{ty} size ', SizeOf({t}));").unwrap();
        for (f, _) in fields {
            writeln!(p, "  WriteLn('{ty}.{f} ', NativeUInt(@P{ty}(nil)^.{f}));").unwrap();
        }
    }
    for (name, _) in CONSTANTS {
        writeln!(p, "  WriteLn('{name} ', {name});").unwrap();
    }
    p.push_str("end.\n");
    p
}

/// With `DWG_LAYOUT_OUT=<dir>`, writes LayoutCheck.dpr and the expected
/// output layout.txt there (for scripts/test-delphi.sh); with Free Pascal
/// installed, compiles and runs it.
#[test]
fn layout_matches_delphi_unit() {
    let program = pascal_layout_program();
    if let Some(dir) = std::env::var_os("DWG_LAYOUT_OUT") {
        let dir = PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("LayoutCheck.dpr"), &program).unwrap();
        std::fs::write(dir.join("layout.txt"), expected_layout(true)).unwrap();
    }
    if Command::new("fpc").arg("-iV").output().is_err() {
        eprintln!("fpc not found: Delphi unit layout not checked");
        return;
    }
    let lib = library_dir();
    let dir = scratch("pascal");
    std::fs::write(dir.join("LayoutCheck.dpr"), &program).unwrap();
    let out = Command::new("fpc")
        .args(["-Sew", "-vw"])
        .arg(format!("-Fl{}", lib.display()))
        .arg(format!("-Fu{}", root().join("bindings/delphi").display()))
        .arg(format!("-FU{}", dir.display()))
        .arg(format!("-o{}", dir.join("layoutcheck").display()))
        .arg(dir.join("LayoutCheck.dpr"))
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stdout)
    );
    let run = Command::new(dir.join("layoutcheck"))
        .env("LD_LIBRARY_PATH", &lib)
        .output()
        .unwrap();
    assert_eq!(
        String::from_utf8(run.stdout).unwrap().replace("\r\n", "\n"),
        expected_layout(true)
    );
    let _ = std::fs::remove_dir_all(dir);
}
