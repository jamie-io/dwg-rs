#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the WebAssembly module of the three.js DWG loader into
# packages/three-dwg-loader/wasm/dwg2gltf.wasm (not in git).
#
#   scripts/build-wasm.sh          # release build
#   scripts/build-wasm.sh --dev    # dev profile (overflow checks), as
#                                  # wasm/dwg2gltf.dev.wasm, for the tests
#
# Needs the Rust target (rustup target add wasm32-wasip1) and wasi-sdk
# (https://github.com/WebAssembly/wasi-sdk/releases, unpacked; set
# WASI_SDK_PATH, default ~/opt/wasi-sdk). wasm-opt (binaryen) shrinks the
# release build when it is on PATH or in $BINARYEN_PATH/bin.
set -euo pipefail
cd "$(dirname "$0")/.."

export WASI_SDK_PATH="${WASI_SDK_PATH:-$HOME/opt/wasi-sdk}"
export WASI_SYSROOT="${WASI_SYSROOT:-$WASI_SDK_PATH/share/wasi-sysroot}"
if [ ! -x "$WASI_SDK_PATH/bin/wasm32-wasip1-clang" ]; then
    echo "error: no wasi-sdk in $WASI_SDK_PATH (set WASI_SDK_PATH)" >&2
    exit 1
fi

# source paths end up in the module (panic locations, C __FILE__): make them
# relative to the repository instead of this checkout
export RUSTFLAGS="${RUSTFLAGS:+$RUSTFLAGS }--remap-path-prefix=$PWD=/dwg-rs"
export CFLAGS_wasm32_wasip1="${CFLAGS_wasm32_wasip1:+$CFLAGS_wasm32_wasip1 }-ffile-prefix-map=$PWD=/dwg-rs"

out_dir=packages/three-dwg-loader/wasm
mkdir -p "$out_dir"
if [ "${1:-}" = "--dev" ]; then
    cargo build -p dwg2gltf-wasm --target wasm32-wasip1 --offline
    cp target/wasm32-wasip1/debug/dwg2gltf_wasm.wasm "$out_dir/dwg2gltf.dev.wasm"
    ls -l "$out_dir/dwg2gltf.dev.wasm"
    exit 0
fi

cargo build -p dwg2gltf-wasm --target wasm32-wasip1 --release --offline
src=target/wasm32-wasip1/release/dwg2gltf_wasm.wasm
out=$out_dir/dwg2gltf.wasm
wasm_opt=$(command -v wasm-opt || true)
if [ -z "$wasm_opt" ] && [ -x "${BINARYEN_PATH:-$HOME/opt/binaryen}/bin/wasm-opt" ]; then
    wasm_opt=${BINARYEN_PATH:-$HOME/opt/binaryen}/bin/wasm-opt
fi
if [ -n "$wasm_opt" ]; then
    # the features come from the module's target_features section
    "$wasm_opt" -O3 --strip-debug --strip-producers "$src" -o "$out"
else
    echo "note: wasm-opt not found; copying the module unoptimized" >&2
    cp "$src" "$out"
fi
node -e '
const fs = require("fs"), zlib = require("zlib");
const b = fs.readFileSync(process.argv[1]);
const kb = (n) => (n / 1024).toFixed(0) + " KB";
console.log(`${process.argv[1]}: ${kb(b.length)}, gzip ${kb(zlib.gzipSync(b, { level: 9 }).length)}, brotli ${kb(zlib.brotliCompressSync(b).length)}`);
' "$out"
