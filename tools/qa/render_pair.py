# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
"""Render GLBs for the comparison from fixed cameras, one Blender process per batch (tools/qa/qa.py):

    blender -b --factory-startup -P tools/qa/render_pair.py -- spec.json

spec.json: {"mode": "eng" | "present", "res": 1024, "samples": 96,
            "jobs": [{"glbs": {"ref": a.glb, "ours": b.glb},
                      "renders": [{"glb": "ref", "view": "front34", "box": [x0, y0, z0, x1, y1, z1],
                                   "out": "/abs/file.jpg" | "/abs/file.png"}, ...]}, ...]}
Each job loads its GLBs once; a render shows only its GLB, with the camera and lights placed on
`box` (Blender axes, Z up; the caller passes the same box for both sides of a pair, so both are
framed identically). Outputs that exist are skipped; files are written atomically.
- eng: Eevee, studio light, material colours and textures, backface culling on every material, so
  faces that point away from the camera show as holes. JPEG.
- present: Cycles on the GPU (seed 0), soft studio light, shadow-catcher floor, transparent film
  (PNG with alpha; qa.py puts the light background behind it), Standard view transform.
"""
import json
import math
import os
import sys
import time

import bpy
import mathutils

VIEWS = {  # direction from the target to the camera, Blender axes (glTF front +Z is Blender -Y)
    "front34": (0.95, -1.35, 0.72),
    "back34": (-0.95, 1.35, 0.72),
    "side": (1.0, -0.12, 0.28),
    "top": (0.12, -0.55, 1.0),
    "front": (0.0, -1.0, 0.18),
}


def log(*a):
    print("[render_pair]", *a, flush=True)


def setup_gpu():
    try:
        prefs = bpy.context.preferences.addons["cycles"].preferences
        for kind in ("OPTIX", "CUDA"):
            try:
                prefs.compute_device_type = kind
                prefs.get_devices()
                devs = [d for d in prefs.devices if d.type == kind]
                if devs:
                    for d in prefs.devices:
                        d.use = d.type == kind
                    return kind
            except TypeError:
                continue
    except Exception as e:
        log("no GPU:", e)
    return None


def clear():
    for o in list(bpy.data.objects):
        bpy.data.objects.remove(o, do_unlink=True)
    for coll in (bpy.data.meshes, bpy.data.materials, bpy.data.images, bpy.data.textures, bpy.data.cameras,
                 bpy.data.lights, bpy.data.node_groups, bpy.data.curves):
        for x in list(coll):
            if x.users == 0:
                coll.remove(x)
    for c in list(bpy.data.collections):
        bpy.data.collections.remove(c)
    bpy.data.orphans_purge(do_recursive=True)


def load(path, name):
    before = set(bpy.context.scene.objects)
    bpy.ops.import_scene.gltf(filepath=path)
    new = [o for o in bpy.context.scene.objects if o not in before]
    coll = bpy.data.collections.new(name)
    bpy.context.scene.collection.children.link(coll)
    for o in new:
        for c in o.users_collection:
            c.objects.unlink(o)
        coll.objects.link(o)
    return new


def bbox(objs):
    pts = [o.matrix_world @ mathutils.Vector(c) for o in objs if o.type == "MESH" and len(o.data.polygons)
           for c in o.bound_box]
    if not pts:
        return None
    lo = mathutils.Vector([min(p[i] for p in pts) for i in range(3)])
    hi = mathutils.Vector([max(p[i] for p in pts) for i in range(3)])
    return lo, hi


def overlap(a, b):
    if not a or not b:
        return 0.0
    inter = 1.0
    va = vb = 1.0
    for i in range(3):
        sa, sb = max(a[1][i] - a[0][i], 1e-6), max(b[1][i] - b[0][i], 1e-6)
        inter *= max(0.0, min(a[1][i], b[1][i]) - max(a[0][i], b[0][i]))
        va *= sa
        vb *= sb
    return inter / max(va, vb)


def place_camera(cam, box, direction, aspect, margin=0.92):
    lo, hi = box
    c = (lo + hi) / 2
    d = mathutils.Vector(direction).normalized()
    rot = (-d).to_track_quat("-Z", "Y")
    cam.rotation_euler = rot.to_euler()
    m = rot.to_matrix()
    tx = math.tan(cam.data.angle_x / 2) * margin if aspect >= 1 else math.tan(cam.data.angle / 2) * margin
    ty = tx / aspect
    D = 0.0
    for i in range(8):
        corner = mathutils.Vector((hi.x if i & 1 else lo.x, hi.y if i & 2 else lo.y, hi.z if i & 4 else lo.z))
        q = m.transposed() @ (corner - c)  # camera space, looking down -Z, target at the origin
        D = max(D, abs(q.x) / tx + q.z, abs(q.y) / ty + q.z)
    D = max(D, (hi - lo).length * 0.05)
    cam.location = c + d * D
    r = (hi - lo).length
    cam.data.clip_start = max(D - r * 2, D * 0.001, 1e-4)
    cam.data.clip_end = D + r * 2


def scene_eng(sc, res):
    # Eevee, not Workbench: Workbench ignores the Mapping node the glTF importer builds for
    # KHR_texture_transform, so a reference that uses it would show the wrong texture scale
    sc.render.engine = "BLENDER_EEVEE"
    try:
        sc.eevee.taa_render_samples = 16
    except AttributeError:
        pass
    w = bpy.data.worlds.new("w")
    w.use_nodes = True
    bg = w.node_tree.nodes.get("Background")
    bg.inputs[0].default_value = (0.82, 0.83, 0.86, 1.0)
    bg.inputs[1].default_value = 0.45
    sc.world = w
    sc.render.film_transparent = False
    sc.render.resolution_x = sc.render.resolution_y = res
    sc.view_settings.view_transform = "AgX"  # highlights roll off: white parts keep their shape


def scene_present(sc, res, samples):
    sc.render.engine = "CYCLES"
    kind = setup_gpu()
    sc.cycles.device = "GPU" if kind else "CPU"
    sc.cycles.samples = samples
    sc.cycles.use_denoising = True
    try:
        sc.cycles.denoiser = "OPTIX" if kind == "OPTIX" else "OPENIMAGEDENOISE"
    except TypeError:
        pass
    sc.cycles.max_bounces = 8
    sc.cycles.transparent_max_bounces = 16
    sc.render.film_transparent = True
    sc.render.resolution_x = sc.render.resolution_y = res
    sc.view_settings.view_transform = "Standard"
    sc.view_settings.look = "None"
    sc.view_settings.exposure = 0.0
    w = bpy.data.worlds.new("w")
    w.use_nodes = True
    bg = w.node_tree.nodes.get("Background")
    bg.inputs[0].default_value = (1.0, 1.0, 1.0, 1.0)
    bg.inputs[1].default_value = 0.35
    sc.world = w
    return kind


def add_studio(box, floor=True, gain=1.0):
    lo, hi = box
    c = (lo + hi) / 2
    r = max((hi - lo).length / 2, 1e-3)
    made = []

    def area(name, direction, size, energy):
        L = bpy.data.lights.new(name, "AREA")
        L.shape = "DISK"
        L.size = size * r
        L.energy = energy * r * r * gain
        o = bpy.data.objects.new(name, L)
        d = mathutils.Vector(direction).normalized()
        o.location = c + d * r * 4
        o.rotation_euler = (-d).to_track_quat("-Z", "Y").to_euler()
        bpy.context.scene.collection.objects.link(o)
        made.append(o)

    area("key", (-1.0, -1.2, 1.6), 3.0, 200)
    area("fill", (1.4, -0.6, 0.6), 4.0, 60)
    area("rim", (0.3, 1.5, 1.2), 2.5, 90)
    if not floor:
        return made
    # shadow-catcher floor under the lower box
    bpy.ops.mesh.primitive_plane_add(size=r * 40, location=(c.x, c.y, lo.z))
    floor = bpy.context.active_object
    floor.is_shadow_catcher = True
    made.append(floor)
    return made


def render(path):
    bpy.context.scene.render.filepath = path
    bpy.ops.render.render(write_still=True)


def main():
    a = sys.argv[sys.argv.index("--") + 1:]
    spec = json.load(open(a[0]))
    mode, res = spec.get("mode", "eng"), int(spec.get("res", 768))
    samples = int(spec.get("samples", 96))
    bpy.ops.wm.read_factory_settings(use_empty=True)
    sc = bpy.context.scene
    if mode == "present":
        log("cycles device", scene_present(sc, res, samples))
        sc.cycles.seed = 0
        sc.cycles.use_animated_seed = False
        sc.render.image_settings.file_format = "PNG"
        sc.render.image_settings.color_mode = "RGBA"
    else:
        scene_eng(sc, res)
        sc.render.image_settings.file_format = "JPEG"
        sc.render.image_settings.color_mode = "RGB"
        sc.render.image_settings.quality = 90
    for k, job in enumerate(spec["jobs"]):
        want = [r for r in job["renders"] if not os.path.exists(r["out"])]
        if not want:
            continue
        t0 = time.time()
        try:
            clear()
            objs = {name: load(path, name) for name, path in job["glbs"].items()
                    if any(r["glb"] == name for r in want)}
        except Exception as e:
            log("import failed", job["glbs"], e)
            continue
        if mode == "eng":  # faces that point away from the camera show as holes
            for ob in objs.values():
                for o in ob:
                    for slot in getattr(o, "material_slots", []):
                        if slot.material:
                            slot.material.use_backface_culling = True
        cam = bpy.data.objects.new("cam", bpy.data.cameras.new("cam"))
        cam.data.lens = 50
        sc.collection.objects.link(cam)
        sc.camera = cam
        lights = []
        for r in want:
            b = r.get("box")
            box = (mathutils.Vector(b[:3]), mathutils.Vector(b[3:])) if b else bbox(objs[r["glb"]])
            if not box:
                continue
            for name, ob in objs.items():
                for o in ob:
                    o.hide_render = name != r["glb"]
            for o in lights:
                bpy.data.objects.remove(o, do_unlink=True)
            lights = add_studio(box, floor=mode == "present", gain=1.0 if mode == "present" else 0.5)
            place_camera(cam, box, VIEWS[r["view"]], 1.0)
            os.makedirs(os.path.dirname(r["out"]), exist_ok=True)
            ext = os.path.splitext(r["out"])[1]
            tmp = r["out"][:-len(ext)] + ".tmp" + ext
            render(tmp)
            if os.path.exists(tmp):
                os.replace(tmp, r["out"])
        log(f"{k + 1}/{len(spec['jobs'])} {time.time() - t0:.1f}s {len(want)} renders")


main()
