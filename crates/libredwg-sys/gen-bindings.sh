#!/bin/sh
# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
# Regenerate the bindings after changing the vendored headers.
# Maintainers only (needs bindgen-cli and libclang); builds use the
# checked-in files.
#
#   ./gen-bindings.sh          src/bindings.rs: 64-bit little-endian targets
#                              (the layout is the same on all of them, which
#                              the generated layout assertions check at
#                              compile time)
#   ./gen-bindings.sh wasm32   src/bindings_wasm32.rs: wasm32-wasip1, with
#                              the wasi-sdk in $WASI_SDK_PATH
#
# With $WASI_SDK_PATH set, its libclang serves both (LIBCLANG_PATH=
# $WASI_SDK_PATH/lib). Its default target is wasm32 and it has no host
# headers, so the host bindings get rustc's host triple and $HOST_SYSROOT
# (default /, Linux; on macOS use $(xcrun --show-sdk-path)), and both get
# its resource dir (stddef.h).
set -eu
cd "$(dirname "$0")"
case "${1:-host}" in
host)
    out=src/bindings.rs
    set --
    if [ -n "${WASI_SDK_PATH:-}" ]; then
        set -- --target="$(rustc -vV | sed -n 's/^host: //p')" \
            --sysroot="${HOST_SYSROOT:-/}"
    fi
    ;;
wasm32)
    : "${WASI_SDK_PATH:?set WASI_SDK_PATH to the wasi-sdk directory}"
    out=src/bindings_wasm32.rs
    # wasm defaults to hidden visibility, and bindgen skips hidden
    # functions (the shim's)
    set -- --target=wasm32-wasip1 -fvisibility=default \
        --sysroot="${WASI_SYSROOT:-$WASI_SDK_PATH/share/wasi-sysroot}"
    ;;
*)
    echo "usage: $0 [host|wasm32]" >&2
    exit 2
    ;;
esac
if [ -n "${WASI_SDK_PATH:-}" ]; then
    set -- "$@" -resource-dir "$("$WASI_SDK_PATH/bin/clang" -print-resource-dir)"
fi
bindgen shim/shim.h -o "$out" \
    --rust-edition 2024 --rust-target 1.85 \
    --allowlist-function 'dwg2gltf_.*|dwg_free|dwg_resbuf_value_type|dwg_add_handleref|dwg_add_u8_input|dwg_model_space_object' \
    --allowlist-function 'dwg_add_(LAYER|MATERIAL|MESH|LINE|INSERT|BLOCK_HEADER|BLOCK|ENDBLK|DICTIONARY|DICTIONARY_item|xdictionary)' \
    --allowlist-function 'dwg_add_XRECORD(|_bool|_real|_chunks|_reals)' \
    --allowlist-type 'Dwg_Data|DWG_OBJECT_TYPE|DWG_OBJECT_SUPERTYPE|Dwg_Version_Type|DWG_ERROR|dwg_point_3d' \
    --no-doc-comments --merge-extern-blocks \
    -- -Ilibredwg/include -Ishim -DHAVE_WCHAR_H -DSIZEOF_WCHAR_T=4 "$@"
