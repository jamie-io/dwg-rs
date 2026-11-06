#!/usr/bin/env bash
# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
# Build the static demo site (GitHub Pages) into OUT_DIR: the three.js
# loader with its WebAssembly module, three.js itself, the page in
# packages/three-dwg-loader/pages/ and the sample drawings listed in its
# gallery.json, with thumbnails rendered in headless Chrome.
#
#   scripts/build-pages.sh OUT_DIR [--no-thumbs] [--chrome PATH]
#
# OUT_DIR is emptied first (it must be empty or an earlier build; a .git
# entry is kept), so it can be a worktree of the gh-pages branch:
#
#   git worktree add ../dwg-rs-gh-pages gh-pages
#   scripts/build-pages.sh ../dwg-rs-gh-pages
#
# Builds the WebAssembly module (scripts/build-wasm.sh) when it is missing and
# installs the pinned three.js (npm ci) when node_modules is missing. The site
# uses relative paths only, so it works under any prefix (/dwg-rs/ on Pages).
# Try it locally with any static server, for example:
#
#   python3 -m http.server -d OUT_DIR 8080
set -euo pipefail
start=$PWD
cd "$(dirname "$0")/.."

if [ $# -lt 1 ] || [ "${1#--}" != "$1" ]; then
    sed -n '4,22p' "$0" | sed 's/^# \{0,1\}//'
    exit 2
fi

pkg=packages/three-dwg-loader
if [ ! -f "$pkg/wasm/dwg2gltf.wasm" ]; then
    scripts/build-wasm.sh
fi
if [ ! -d "$pkg/node_modules/three" ]; then
    (cd "$pkg" && npm ci)
fi
# from the caller's directory: OUT_DIR may be relative to it
build=$PWD/$pkg/pages/build.mjs
cd "$start"
node "$build" "$@"
