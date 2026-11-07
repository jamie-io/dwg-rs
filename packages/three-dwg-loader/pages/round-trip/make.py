#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
"""Make the round-trip samples of the demo site: glTF sample models from
Khronos, with their images re-encoded at most 1024 px wide (JPEG when
opaque), written to DWG by gltf2dwg. Next to each DWG, <id>.json records
where the model comes from and what the source holds, for the page to
compare the round trip with.

    python3 packages/three-dwg-loader/pages/round-trip/make.py [--gltf2dwg PATH]

Downloads the models (pinned to one commit of glTF-Sample-Assets) into a
temporary folder. Needs Pillow and a release build of gltf2dwg
(cargo build --release).
"""

import argparse
import io
import json
import struct
import subprocess
import sys
import tempfile
import urllib.request
from pathlib import Path

from PIL import Image

HERE = Path(__file__).resolve().parent
REPO = HERE.parents[3]
sys.path.insert(0, str(REPO / "tests/bricscad"))
from glb import read_mesh  # noqa: E402

COMMIT = "8f492390ddc7a2ae221a1369ef560589cf8c70fb"
BASE = f"https://raw.githubusercontent.com/KhronosGroup/glTF-Sample-Assets/{COMMIT}/Models"
IMAGE_SIZE = 1024

MODELS = {
    "lantern": {"name": "Lantern", "license": "CC0-1.0", "author": "Microsoft"},
    "water-bottle": {"name": "WaterBottle", "license": "CC0-1.0", "author": "Microsoft"},
}

MAPS = {
    "base color": ("pbrMetallicRoughness", "baseColorTexture"),
    "metallic-roughness": ("pbrMetallicRoughness", "metallicRoughnessTexture"),
    "normal": (None, "normalTexture"),
    "occlusion": (None, "occlusionTexture"),
    "emissive": (None, "emissiveTexture"),
}


def read_glb(data):
    length = struct.unpack_from("<I", data, 12)[0]
    return json.loads(data[20 : 20 + length]), data[20 + length + 8 :]


def write_glb(doc, binary):
    text = json.dumps(doc, separators=(",", ":")).encode()
    text += b" " * (-len(text) % 4)
    binary += b"\0" * (-len(binary) % 4)
    total = 12 + 8 + len(text) + 8 + len(binary)
    return (
        struct.pack("<III", 0x46546C67, 2, total)
        + struct.pack("<II", len(text), 0x4E4F534A)
        + text
        + struct.pack("<II", len(binary), 0x004E4942)
        + binary
    )


def shrink(data):
    """The GLB with its images at most IMAGE_SIZE wide."""
    doc, binary = read_glb(data)
    images = {img["bufferView"]: i for i, img in enumerate(doc.get("images", [])) if "bufferView" in img}
    out = bytearray()
    for v, view in enumerate(doc["bufferViews"]):
        start = view.get("byteOffset", 0)
        chunk = binary[start : start + view["byteLength"]]
        if v in images:
            image = Image.open(io.BytesIO(chunk))
            image.thumbnail((IMAGE_SIZE, IMAGE_SIZE), Image.LANCZOS)
            buffer = io.BytesIO()
            opaque = image.mode in ("RGB", "L") or (image.mode == "RGBA" and image.getextrema()[3][0] == 255)
            if opaque:
                image.convert("RGB").save(buffer, "JPEG", quality=86, optimize=True)
                doc["images"][images[v]]["mimeType"] = "image/jpeg"
            else:
                image.save(buffer, "PNG", optimize=True)
                doc["images"][images[v]]["mimeType"] = "image/png"
            chunk = buffer.getvalue()
        out += b"\0" * (-len(out) % 4)
        view["byteOffset"] = len(out)
        view["byteLength"] = len(chunk)
        out += chunk
    doc["buffers"][0]["byteLength"] = len(out)
    return write_glb(doc, bytes(out))


def maps(doc):
    """The texture maps each material of `doc` uses."""
    result = []
    for material in doc.get("materials", []):
        used = []
        for label, (group, key) in MAPS.items():
            where = material.get(group, {}) if group else material
            if key in where:
                used.append(label)
        result.append({"name": material.get("name", ""), "maps": used, "extensions": sorted(material.get("extensions", {}))})
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--gltf2dwg", default=str(REPO / "target/release/gltf2dwg"))
    args = parser.parse_args()
    git = ["git", "-C", str(REPO)]
    commit = subprocess.run([*git, "rev-parse", "--short=7", "HEAD"], capture_output=True, text=True, check=True).stdout.strip()
    if subprocess.run([*git, "status", "--porcelain", "--", "crates"], capture_output=True, text=True, check=True).stdout:
        commit += " with uncommitted changes to the converter"
    with tempfile.TemporaryDirectory() as tmp:
        for sample, model in MODELS.items():
            url = f"{BASE}/{model['name']}/glTF-Binary/{model['name']}.glb"
            with urllib.request.urlopen(url) as response:
                original = response.read()
            glb = Path(tmp) / f"{sample}.glb"
            glb.write_bytes(shrink(original))
            dwg = HERE / f"{sample}.dwg"
            run = subprocess.run([args.gltf2dwg, str(glb), "-o", str(dwg)], capture_output=True, text=True, check=True)
            doc, _ = read_glb(glb.read_bytes())
            mesh = read_mesh(glb)
            lo, hi = mesh.bounds()
            record = {
                "title": model["name"],
                "source": f"https://github.com/KhronosGroup/glTF-Sample-Assets/tree/{COMMIT}/Models/{model['name']}",
                "license": model["license"],
                "author": model["author"],
                "images": f"re-encoded at most {IMAGE_SIZE} px wide",
                "writer": f"gltf2dwg, dwg-rs {commit}",
                "written": [line.strip() for line in run.stderr.splitlines() + run.stdout.splitlines() if line.startswith("  ")],
                "triangles": len(mesh.triangles),
                "area": mesh.area(),
                "bounds": [list(lo), list(hi)],
                "materials": maps(doc),
            }
            (HERE / f"{sample}.json").write_text(json.dumps(record, indent="\t") + "\n")
            print(f"{dwg.name}: {dwg.stat().st_size} bytes, {record['triangles']} triangles")


if __name__ == "__main__":
    main()
