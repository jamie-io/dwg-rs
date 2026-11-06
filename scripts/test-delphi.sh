#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
# Cross-build dwg2gltf.dll for Windows x64, compile the Delphi unit's layout
# check and examples/delphi/SceneInfo.dpr with the Delphi Win64 compiler,
# and run both under Wine.
#
#   DCC64='wine /path/to/bin/dcc64.exe -NSSystem;Winapi -U<lib> -R<lib>' scripts/test-delphi.sh [FILE.dwg...]
#
# DCC64 is the compiler command with the options it needs to find the RTL
# (it is run through `eval`, so it may quote). Without files, a few BricsCAD
# test drawings are used. Needs the Rust target x86_64-pc-windows-gnu,
# mingw-w64 (x86_64-w64-mingw32-gcc), Wine, and a C compiler for the layout
# reference.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ -z "${DCC64:-}" ]; then
    echo "error: set DCC64 to the dcc64 command (see the top of this script)" >&2
    exit 1
fi
export WINEDEBUG="${WINEDEBUG:--all}"
target=x86_64-pc-windows-gnu
target_dir="${CARGO_TARGET_DIR:-target}"
out="$target_dir/delphi-test"
rm -rf "$out"
mkdir -p "$out"

scripts/build-dll.sh >/dev/null
cp "$target_dir/$target/release/dwg2gltf.dll" "$out/"

# LayoutCheck.dpr and its expected output, generated from the Rust structs
DWG_LAYOUT_OUT="$(realpath "$out")" cargo test --release --offline -p dwg2gltf-capi --lib \
    layout_matches_delphi_unit -- --quiet

unit_dir=$(winepath -w "$PWD/bindings/delphi")
out_dir=$(winepath -w "$(realpath "$out")")
# compile DIR PROGRAM.dpr: the executable goes to $out
compile() {
    (cd "$1" && eval "$DCC64" -B -Q -U"'$unit_dir'" -E"'$out_dir'" -NU"'$out_dir'" "$2") |
        iconv -f latin1 -t utf-8
    [ "${PIPESTATUS[0]}" -eq 0 ]
}
compile "$out" LayoutCheck.dpr
compile examples/delphi SceneInfo.dpr

(cd "$out" && wine LayoutCheck.exe) | tr -d '\r' > "$out/layout-delphi.txt"
diff -u "$out/layout.txt" "$out/layout-delphi.txt"
echo "layout: Delphi records match the Rust structs"

if [ "$#" -eq 0 ]; then
    set -- tests/bricscad/dwg/v27/primitive/box/box_AC1032.dwg \
        tests/bricscad/dwg/v27/fillet/fillet_small/fillet_small_AC1032.dwg \
        tests/bricscad/dwg/v27/sweep/sweep_circle_helix/sweep_circle_helix_AC1032.dwg \
        tests/bricscad/dwg/v27/sheet/sheet_mesh/sheet_mesh_AC1032.dwg
fi
files=()
for f in "$@"; do
    files+=("$(winepath -w "$f")")
done
(cd "$out" && wine SceneInfo.exe "${files[@]}") | tr -d '\r'
