#!/bin/sh
# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
# Recreate libredwg/ from a LibreDWG master snapshot: the decoder and encoder
# sources, with the patches applied. Maintainers only; builds use the
# checked-in copy. Run gen-bindings.sh afterwards if a patch changes
# include/dwg.h.
#
# There is no release with the fixes after 0.14 yet (NEWS: "0.14.1"), so this
# takes a commit's GitHub archive. GitHub does not promise that archive bytes
# stay identical forever; if the checksum ever fails for this commit, compare
# the extracted tree with a git checkout of it before updating SHA256.
set -eu
COMMIT=4f2aef33865e0d5bf3774acc439953fba15dea3e # master, 2026-11-05
SHA256=5fa221f3df8b8cf7677232c0105899f5f16e93d23ae9444ab9e3bfb45eee93e4
URL="https://github.com/LibreDWG/libredwg/archive/${COMMIT}.tar.gz"
HERE="$(cd "$(dirname "$0")" && pwd)"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

curl -fsSL -o "$WORK/src.tar.gz" "$URL"
echo "${SHA256}  $WORK/src.tar.gz" | shasum -a 256 -c -
tar -xzf "$WORK/src.tar.gz" -C "$WORK"
SRC="$WORK/libredwg-${COMMIT}"

DEST="$HERE/libredwg"
rm -rf "$DEST"
mkdir -p "$DEST/include" "$DEST/src/codepages" "$DEST/programs"
cp "$SRC/COPYING" "$SRC/AUTHORS" "$SRC/NEWS" "$DEST/"
echo "$COMMIT" > "$DEST/COMMIT"
cp "$SRC/include/dwg.h" "$SRC/include/dwg_api.h" "$DEST/include/"
cp "$SRC/programs/my_stat.h" "$DEST/programs/"
# the library sources of build.rs, and every header and spec they include
for f in dwg common codepages bits logging decode decode2 decode_r11 decode_r2007 \
         reedsolomon print free hash dynapi classes dwg_api objects geom \
         encode encode2 dxfclasses; do
    cp "$SRC/src/$f.c" "$DEST/src/"
done
cp "$SRC"/src/*.h "$SRC"/src/*.spec "$SRC"/src/*.inc "$DEST/src/"
cp "$SRC"/src/codepages/*.h "$DEST/src/codepages/"

for patch in acds-keep-section add-mesh-material r2010-section-sizes entity-material \
             embedded-mtext-reactors surface-acis; do
    patch -s -p1 -d "$DEST" < "$HERE/patches/libredwg-${patch}.patch"
done
echo "Vendored LibreDWG ${COMMIT} into $DEST"
