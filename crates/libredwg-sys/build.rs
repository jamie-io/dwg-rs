// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Compiles the vendored LibreDWG sources (decoder and encoder) into a static
//! library. Only a C compiler is needed: config.h is written here instead of
//! running LibreDWG's configure script.

use std::env;
use std::fs;
use std::path::PathBuf;

/// LibreDWG's libredwg_la_SOURCES without the DXF/JSON in- and output.
const SOURCES: &[&str] = &[
    "dwg.c",
    "common.c",
    "codepages.c",
    "bits.c",
    "logging.c",
    "decode.c",
    "decode2.c",
    "decode_r11.c",
    "decode_r2007.c",
    "reedsolomon.c",
    "print.c",
    "free.c",
    "hash.c",
    "dynapi.c",
    "classes.c",
    "dwg_api.c",
    "objects.c",
    "geom.c",
    "encode.c",
    "encode2.c",
    "dxfclasses.c",
];

fn main() {
    let root = PathBuf::from(env::var("CARGO_MANIFEST_DIR").unwrap());
    let out = PathBuf::from(env::var("OUT_DIR").unwrap());
    let src = root.join("libredwg/src");
    let target_os = env::var("CARGO_CFG_TARGET_OS").unwrap();
    let target_env = env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let pointer_width = env::var("CARGO_CFG_TARGET_POINTER_WIDTH").unwrap();
    let wasm = env::var("CARGO_CFG_TARGET_ARCH").unwrap() == "wasm32";

    // src/bindings.rs was generated for 64-bit little-endian targets,
    // src/bindings_wasm32.rs for wasm32-wasip1
    if env::var("CARGO_CFG_TARGET_ENDIAN").unwrap() != "little" || !(pointer_width == "64" || wasm)
    {
        panic!("libredwg-sys supports 64-bit little-endian targets and wasm32 only");
    }

    let windows = target_os == "windows";
    let msvc = target_env == "msvc";
    let size_t: usize = pointer_width.parse::<usize>().unwrap() / 8;
    fs::write(out.join("config.h"), config_h(windows, msvc, size_t)).unwrap();

    let mut build = cc::Build::new();
    build
        .include(&out)
        .include(root.join("libredwg/include"))
        .include(&src)
        .include(root.join("shim"))
        .files(SOURCES.iter().map(|f| src.join(f)))
        .file(root.join("shim/shim.c"))
        // third-party code: its warnings are not actionable here
        .warnings(false)
        .flag_if_supported("-w");
    if msvc {
        build.flag("/w").define("_CRT_SECURE_NO_WARNINGS", None);
    }
    build.compile("redwg");

    for dir in ["libredwg", "shim"] {
        println!("cargo:rerun-if-changed={}", root.join(dir).display());
    }
    println!("cargo:rerun-if-changed=build.rs");
}

/// The subset of configure's results that the library sources check.
fn config_h(windows: bool, msvc: bool, size_t: usize) -> String {
    let size_t = format!("SIZEOF_SIZE_T {size_t}");
    let mut defs = vec![
        // master snapshot 4f2aef33 (libredwg/COMMIT), after 0.14
        "PACKAGE_VERSION \"0.14.1-4f2aef33\"",
        "LIBREDWG_SO_VERSION \"0:14:0\"",
        // release behaviour: bit_nan() is 0.0, a failed encode frees its
        // buffer, no development-only LWPOLYLINE point limit
        "IS_RELEASE 1",
        "USE_WRITE 1",
        "DISABLE_DXF 1",
        "DISABLE_JSON 1",
        "DISABLE_BINDINGS 1",
        &size_t,
        "HAVE_STDDEF_H 1",
        "HAVE_STDINT_H 1",
        "HAVE_INTTYPES_H 1",
        "HAVE_STDLIB_H 1",
        "HAVE_STRING_H 1",
        "HAVE_CTYPE_H 1",
        "HAVE_LIMITS_H 1",
        "HAVE_FLOAT_H 1",
        "HAVE_SYS_TYPES_H 1",
        "HAVE_SYS_STAT_H 1",
        "HAVE_WCHAR_H 1",
        "HAVE_WCTYPE_H 1",
        "HAVE_STRNLEN 1",
        "HAVE_ALLOCA 1",
    ];
    if windows {
        defs.extend(["SIZEOF_WCHAR_T 2", "HAVE_WCSNLEN 1", "HAVE_MALLOC_H 1"]);
    } else {
        defs.extend([
            "SIZEOF_WCHAR_T 4",
            "HAVE_UNISTD_H 1",
            "HAVE_STRINGS_H 1",
            "HAVE_STRCASECMP 1",
            "AX_STRCASECMP_HEADER <strings.h>",
            "HAVE_ALLOCA_H 1",
            "HAVE_LIBGEN_H 1",
            "HAVE_BASENAME 1",
            "HAVE_GMTIME_R 1",
            "HAVE_MEMMEM 1",
            "HAVE_SYS_PARAM_H 1",
            "HAVE_SYS_TIME_H 1",
        ]);
    }
    if msvc {
        defs.push("restrict __restrict");
    }
    let mut h = String::from(
        "/* written by libredwg-sys/build.rs */\n#ifndef CONFIG_H\n#define CONFIG_H 1\n",
    );
    for d in defs {
        h += &format!("#define {d}\n");
    }
    h + "#endif\n"
}
