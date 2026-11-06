#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
# Cross-build dwg2gltf.dll, compile the Direct3D 11 viewer example
# (examples/delphi/viewer) and its picking test with the Delphi Win64
# compiler, run the picking test under Wine and render every drawing in
# every mode into PNG files.
#
#   DCC64='wine /path/to/bin/dcc64.exe -NSSystem;Winapi -U<lib> -R<lib>' scripts/build-delphi-viewer.sh [FILE.dwg...]
#
# DCC64 is the compiler command with the options it needs to find the RTL
# (run through `eval`, so it may quote), as for scripts/test-delphi.sh.
# Output goes to target/delphi-viewer/: DwgViewer.exe, PickingTest.exe,
# dwg2gltf.dll and <drawing>-<mode>.png. NO_RUN=1 only compiles. Rendering
# under Wine needs a display (Wine's Direct3D 11 draws through OpenGL or
# Vulkan); without one, run the test only with NO_RENDER=1.
set -euo pipefail
cd "$(dirname "$0")/.."

if [ -z "${DCC64:-}" ]; then
    echo "error: set DCC64 to the dcc64 command (see the top of this script)" >&2
    exit 1
fi
export WINEDEBUG="${WINEDEBUG:--all}"
target=x86_64-pc-windows-gnu
target_dir="${CARGO_TARGET_DIR:-target}"
out="$target_dir/delphi-viewer"
rm -rf "$out"
mkdir -p "$out"

scripts/build-dll.sh >/dev/null
cp "$target_dir/$target/release/dwg2gltf.dll" "$out/"

out_dir=$(winepath -w "$(realpath "$out")")
compile() {
    (cd examples/delphi/viewer && eval "$DCC64" -B -Q -E"'$out_dir'" -NU"'$out_dir'" "$1") |
        iconv -f latin1 -t utf-8
    [ "${PIPESTATUS[0]}" -eq 0 ]
}
compile DwgViewer.dpr
compile PickingTest.dpr
[ -n "${NO_RUN:-}" ] && exit 0

if [ "$#" -eq 0 ]; then
    set -- tests/bricscad/dwg/v27/primitive/box/box_AC1032.dwg \
        tests/bricscad/dwg/v27/fillet/fillet_small/fillet_small_AC1032.dwg \
        tests/bricscad/dwg/v27/sweep/sweep_circle_helix/sweep_circle_helix_AC1032.dwg \
        tests/bricscad/dwg/v27/sheet/sheet_mesh/sheet_mesh_AC1032.dwg \
        tests/bricscad/dwg/v27/trim/trim_cylinder_hole/trim_cylinder_hole_AC1032.dwg
fi
files=()
for f in "$@"; do
    files+=("$(winepath -w "$f")")
done
(cd "$out" && wine PickingTest.exe "${files[@]}") | tr -d '\r'
[ -n "${NO_RENDER:-}" ] && exit 0

for f in "${files[@]}"; do
    name=$(basename "${f//\\//}" .dwg)
    (cd "$out" && wine DwgViewer.exe "$f" --screenshot "$name.png") | tr -d '\r'
    for mode in shaded edges hidden wireframe triangles xray; do
        [ -s "$out/$name-$mode.png" ] || { echo "error: $name-$mode.png missing" >&2; exit 1; }
    done
done
echo "screenshots: $out/*.png"
