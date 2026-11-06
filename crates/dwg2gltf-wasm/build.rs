// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

fn main() {
    if std::env::var("CARGO_CFG_TARGET_ARCH").as_deref() == Ok("wasm32") {
        // ACIS tessellation recurses deeper than Rust's default 1 MiB stack
        // allows; with the stack below the data, an overflow traps instead
        // of overwriting statics
        println!("cargo::rustc-link-arg-cdylib=-zstack-size=8388608");
        println!("cargo::rustc-link-arg-cdylib=--stack-first");
    }
    println!("cargo::rerun-if-changed=build.rs");
}
