# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
"""Minimal GLB reader for the comparison tools: world-space triangles only.

Line and point primitives are left out, so the wire geometry dwg2gltf writes
does not count as a surface or a material. Returns numpy arrays.
"""
import io
import json
import struct

import numpy as np

CT = {5120: np.int8, 5121: np.uint8, 5122: np.int16, 5123: np.uint16, 5125: np.uint32, 5126: np.float32}
NC = {"SCALAR": 1, "VEC2": 2, "VEC3": 3, "VEC4": 4, "MAT4": 16}


class Glb:
    """tris (T,3,3) float64 in meters as stored; mat (T,) material index or -1;
    materials: list of dict(name, rgb (linear), alpha, textured, tex_rgb (linear mean or None),
    color (linear rgb factor * texture mean))."""

    def __init__(self, path, texture_means=True):
        raw = open(path, "rb").read()
        if raw[:4] != b"glTF":
            raise ValueError("not a GLB")
        off, js, binc = 12, None, b""
        while off + 8 <= len(raw):
            n, kind = struct.unpack("<II", raw[off:off + 8])
            chunk = raw[off + 8:off + 8 + n]
            if kind == 0x4E4F534A:
                js = json.loads(chunk)
            elif kind == 0x004E4942:
                binc = chunk
            off += 8 + n
        self.j, self.bin = js, binc
        self.generator = js.get("asset", {}).get("generator", "")
        self.extensions = js.get("extensionsUsed", [])
        self.line_prims = 0
        tris, mats, uvd = [], [], []
        scene = js.get("scenes", [{}])[js.get("scene", 0)] if js.get("scenes") else {"nodes": list(range(len(js.get("nodes", []))))}
        stack = [(n, np.eye(4)) for n in scene.get("nodes", [])]
        while stack:
            ni, parent = stack.pop()
            node = js["nodes"][ni]
            m = parent @ node_matrix(node)
            if "mesh" in node:
                for p in js["meshes"][node["mesh"]]["primitives"]:
                    mode = p.get("mode", 4)
                    if mode not in (4, 5, 6):
                        self.line_prims += 1
                        continue
                    pos = self.accessor(p["attributes"]["POSITION"]).astype(np.float64)
                    idx = self.accessor(p["indices"]).reshape(-1).astype(np.int64) if "indices" in p else np.arange(len(pos))
                    f = faces(idx, mode)
                    if not len(f):
                        continue
                    w = pos @ m[:3, :3].T + m[:3, 3]
                    t = w[f]
                    if np.linalg.det(m[:3, :3]) < 0:  # a mirroring node flips the winding
                        t = t[:, ::-1]
                    tris.append(t)
                    mats.append(np.full(len(f), p.get("material", -1), np.int32))
                    if "TEXCOORD_0" in p["attributes"]:
                        uv = self.accessor(p["attributes"]["TEXCOORD_0"]).astype(np.float64)[f]
                        e1, e2 = uv[:, 1] - uv[:, 0], uv[:, 2] - uv[:, 0]
                        ua = np.abs(e1[:, 0] * e2[:, 1] - e1[:, 1] * e2[:, 0]) / 2
                        wa = np.linalg.norm(np.cross(t[:, 1] - t[:, 0], t[:, 2] - t[:, 0]), axis=1) / 2
                        uvd.append(np.sqrt(ua / np.maximum(wa, 1e-30)))
                    else:
                        uvd.append(np.full(len(f), np.nan))
            for c in node.get("children", []):
                stack.append((c, m))
        self.tris = np.concatenate(tris) if tris else np.zeros((0, 3, 3))
        self.mat = np.concatenate(mats) if mats else np.zeros(0, np.int32)
        self.materials = [self.material(m, texture_means) for m in js.get("materials", [])]
        # texture repeats per meter of surface: UV density times the material's texture transform scale
        d = np.concatenate(uvd) if uvd else np.zeros(0)
        sc = np.array([mm["tex_scale"] for mm in self.materials] + [1.0])
        self.uv_density = d * sc[np.where(self.mat < 0, len(self.materials), self.mat)] if len(d) else d

    def accessor(self, i):
        a = self.j["accessors"][i]
        dt, nc = np.dtype(CT[a["componentType"]]), NC[a["type"]]
        cnt = a["count"]
        if "bufferView" not in a:
            return np.zeros((cnt, nc), dt)
        bv = self.j["bufferViews"][a["bufferView"]]
        start = bv.get("byteOffset", 0) + a.get("byteOffset", 0)
        stride = bv.get("byteStride", 0) or dt.itemsize * nc
        buf = np.frombuffer(self.bin, np.uint8, count=stride * (cnt - 1) + dt.itemsize * nc, offset=start)
        if stride == dt.itemsize * nc:
            out = buf.view(dt).reshape(cnt, nc)
        else:
            out = np.lib.stride_tricks.as_strided(buf, (cnt, nc * dt.itemsize), (stride, 1)).copy().view(dt).reshape(cnt, nc)
        if a.get("normalized") and dt.kind in "iu":
            out = np.maximum(out / np.iinfo(dt).max, -1.0)
        return out

    def image_mean(self, tex_index):
        try:
            from PIL import Image
            tex = self.j["textures"][tex_index]
            src = tex.get("source")
            for e in tex.get("extensions", {}).values():
                src = e.get("source", src)
            img = self.j["images"][src]
            if "bufferView" in img:
                bv = self.j["bufferViews"][img["bufferView"]]
                data = self.bin[bv.get("byteOffset", 0):bv.get("byteOffset", 0) + bv["byteLength"]]
            else:
                return None
            im = Image.open(io.BytesIO(data)).convert("RGB")
            im.thumbnail((64, 64))
            a = np.asarray(im, np.float64) / 255.0
            self._tex_std = float(a.mean(2).std())  # contrast in sRGB luminance
            return srgb_to_linear(a).reshape(-1, 3).mean(0)
        except Exception:
            return None

    def material(self, m, texture_means):
        pbr = m.get("pbrMetallicRoughness", {})
        f = pbr.get("baseColorFactor", [1, 1, 1, 1])
        tex = pbr.get("baseColorTexture")
        if tex is None:  # KHR_materials_pbrSpecularGlossiness etc.
            sg = m.get("extensions", {}).get("KHR_materials_pbrSpecularGlossiness")
            if sg:
                f = sg.get("diffuseFactor", f)
                tex = sg.get("diffuseTexture")
        self._tex_std = 0.0
        tm = self.image_mean(tex["index"]) if (tex is not None and texture_means) else None
        rgb = np.array(f[:3], np.float64)
        tt = (tex or {}).get("extensions", {}).get("KHR_texture_transform", {}).get("scale", [1, 1])
        return dict(name=m.get("name", ""), rgb=rgb, alpha=float(f[3]) if len(f) > 3 else 1.0,
                    textured=tex is not None, tex_rgb=tm, tex_std=self._tex_std, normal_map="normalTexture" in m, tex_scale=float(np.sqrt(abs(tt[0] * tt[1]))),
                    color=rgb * (tm if tm is not None else 1.0),
                    unlit="KHR_materials_unlit" in m.get("extensions", {}))


def node_matrix(n):
    if "matrix" in n:
        return np.array(n["matrix"], np.float64).reshape(4, 4).T
    m = np.eye(4)
    t, r, s = n.get("translation", [0, 0, 0]), n.get("rotation", [0, 0, 0, 1]), n.get("scale", [1, 1, 1])
    x, y, z, w = r
    rot = np.array([[1 - 2 * (y * y + z * z), 2 * (x * y - z * w), 2 * (x * z + y * w)],
                    [2 * (x * y + z * w), 1 - 2 * (x * x + z * z), 2 * (y * z - x * w)],
                    [2 * (x * z - y * w), 2 * (y * z + x * w), 1 - 2 * (x * x + y * y)]])
    m[:3, :3] = rot * np.array(s)[None, :]
    m[:3, 3] = t
    return m


def faces(idx, mode):
    if mode == 4:
        n = len(idx) // 3 * 3
        return idx[:n].reshape(-1, 3)
    if mode == 5:
        f = np.stack([idx[:-2], idx[1:-1], idx[2:]], 1)
        f[1::2] = f[1::2][:, [1, 0, 2]]
        return f
    if mode == 6:
        return np.stack([np.full(len(idx) - 2, idx[0]), idx[1:-1], idx[2:]], 1)
    return np.zeros((0, 3), np.int64)


def srgb_to_linear(c):
    c = np.asarray(c, np.float64)
    return np.where(c <= 0.04045, c / 12.92, ((c + 0.055) / 1.055) ** 2.4)


def linear_to_srgb(c):
    c = np.clip(np.asarray(c, np.float64), 0, 1)
    return np.where(c <= 0.0031308, c * 12.92, 1.055 * c ** (1 / 2.4) - 0.055)
