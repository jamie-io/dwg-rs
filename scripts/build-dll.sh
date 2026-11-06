#!/usr/bin/env bash
# Build dwg2gltf.dll for 64-bit Windows (x86_64-pc-windows-gnu) on Linux.
# Needs the Rust target (rustup target add x86_64-pc-windows-gnu) and the
# MinGW-w64 cross compiler (x86_64-w64-mingw32-gcc).
# Source paths in panic messages and C __FILE__ strings are rewritten to
# /dwg-rs, so the DLL does not carry the build machine's directories.
#   scripts/build-dll.sh            -> target/x86_64-pc-windows-gnu/release/dwg2gltf.dll
set -euo pipefail
cd "$(dirname "$0")/.."
target=x86_64-pc-windows-gnu
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }--remap-path-prefix=$PWD=/dwg-rs"
export CFLAGS_x86_64_pc_windows_gnu="${CFLAGS_x86_64_pc_windows_gnu:+$CFLAGS_x86_64_pc_windows_gnu }-ffile-prefix-map=$PWD=/dwg-rs"
cargo build --release --offline --target "$target" -p dwg2gltf-capi
echo "${CARGO_TARGET_DIR:-target}/$target/release/dwg2gltf.dll"
