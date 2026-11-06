// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! examples/c/scene_info.c compiled against include/dwg2gltf.h, linked
//! with the shared library, and run on BricsCAD test drawings.

use std::path::{Path, PathBuf};
use std::process::Command;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The directory with libdwg2gltf.so (or .dylib) of this build profile,
/// built first if it is not there.
fn library_dir() -> PathBuf {
    // target/<profile>/deps/c_example-<hash>
    let exe = std::env::current_exe().unwrap();
    let dir = exe.parent().unwrap().parent().unwrap().to_path_buf();
    let lib = dir.join(format!(
        "{}dwg2gltf{}",
        std::env::consts::DLL_PREFIX,
        std::env::consts::DLL_SUFFIX
    ));
    if !lib.exists() {
        let release = dir.file_name().is_some_and(|n| n == "release");
        let mut cargo = Command::new(std::env::var("CARGO").unwrap_or("cargo".into()));
        cargo.args(["build", "--offline", "-p", "dwg2gltf-capi"]);
        if release {
            cargo.arg("--release");
        }
        assert!(cargo.status().unwrap().success());
    }
    assert!(lib.exists(), "{} missing", lib.display());
    dir
}

#[test]
fn scene_info_example() {
    if cfg!(windows) {
        return; // the test links like a Unix linker
    }
    let lib = library_dir();
    let out = std::env::temp_dir().join(format!("dwg2gltf-scene-info-{}", std::process::id()));
    std::fs::create_dir_all(&out).unwrap();
    let exe = out.join("scene_info");
    let status = Command::new(std::env::var("CC").unwrap_or("cc".into()))
        .args(["-std=c99", "-Wall", "-Wextra", "-Werror", "-pedantic", "-I"])
        .arg(root().join("include"))
        .arg(root().join("examples/c/scene_info.c"))
        .arg("-L")
        .arg(&lib)
        .args(["-ldwg2gltf", "-o"])
        .arg(&exe)
        .status()
        .expect("a C compiler (cc)");
    assert!(status.success());
    let dwgs = [
        "primitive/box/box_AC1032.dwg",
        "fillet/fillet_small/fillet_small_AC1032.dwg",
        "sweep/sweep_circle_helix/sweep_circle_helix_AC1032.dwg",
        "sheet/sheet_mesh/sheet_mesh_AC1032.dwg",
    ]
    .map(|d| root().join("tests/bricscad/dwg/v27").join(d));
    let run = Command::new(&exe)
        .args(&dwgs)
        .env("LD_LIBRARY_PATH", &lib)
        .env("DYLD_LIBRARY_PATH", &lib)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&run.stdout);
    println!("{text}");
    assert!(
        run.status.success(),
        "{text}{}",
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(text.matches("  meshes: ").count(), dwgs.len());
    assert_eq!(text.matches("  glb: ").count(), dwgs.len());
    // the box: 12 triangles and 12 visible edges
    assert!(text.contains("triangles: 12 stored, 12 drawn; line segments: 0; visible edges: 12"));
    // a missing file fails with a message
    let run = Command::new(&exe)
        .arg(out.join("missing.dwg"))
        .env("LD_LIBRARY_PATH", &lib)
        .env("DYLD_LIBRARY_PATH", &lib)
        .output()
        .unwrap();
    assert!(!run.status.success());
    assert!(String::from_utf8_lossy(&run.stderr).contains("error 2: cannot read"));
    let _ = std::fs::remove_dir_all(out);
}

/// examples/delphi/SceneInfo.dpr built with Free Pascal against the shared
/// library and run (the Delphi unit's declarations at run time); skipped
/// without fpc.
#[test]
fn scene_info_pascal_example() {
    if cfg!(windows) || Command::new("fpc").arg("-iV").output().is_err() {
        eprintln!("fpc not found: Pascal example not run");
        return;
    }
    let lib = library_dir();
    let out = std::env::temp_dir().join(format!("dwg2gltf-scene-info-pas-{}", std::process::id()));
    std::fs::create_dir_all(&out).unwrap();
    let exe = out.join("sceneinfo");
    let build = Command::new("fpc")
        .args(["-Sew", "-vw"])
        .arg(format!("-Fu{}", root().join("bindings/delphi").display()))
        .arg(format!("-Fl{}", lib.display()))
        .arg(format!("-FU{}", out.display()))
        .arg(format!("-o{}", exe.display()))
        .arg(root().join("examples/delphi/SceneInfo.dpr"))
        .output()
        .unwrap();
    assert!(
        build.status.success(),
        "{}",
        String::from_utf8_lossy(&build.stdout)
    );
    let run = Command::new(&exe)
        .arg(root().join("tests/bricscad/dwg/v27/primitive/box/box_AC1032.dwg"))
        .env("LD_LIBRARY_PATH", &lib)
        .output()
        .unwrap();
    let text = String::from_utf8_lossy(&run.stdout);
    println!("{text}");
    assert!(run.status.success(), "{text}");
    assert!(text.contains("triangles: 12 stored, 12 drawn; line segments: 0; visible edges: 12"));
    assert!(text.contains("  glb: "));
    let _ = std::fs::remove_dir_all(out);
}
