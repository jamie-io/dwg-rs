# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
# Render a GLB to PNG with headless Blender (Workbench, studio light):
#   blender -b -P tools/qa/render.py -- in.glb out.png [view_dir_x,y,z]
# Use absolute paths. Frames only objects with faces (lines are ignored).
import bpy, sys, math, mathutils
a = sys.argv[sys.argv.index("--") + 1:]
bpy.ops.wm.read_factory_settings(use_empty=True)
bpy.ops.import_scene.gltf(filepath=a[0])
objs = [o for o in bpy.context.scene.objects if o.type == "MESH" and len(o.data.polygons)]
objs = objs or [o for o in bpy.context.scene.objects if o.type in ("MESH", "CURVE")]
pts = [o.matrix_world @ mathutils.Vector(c) for o in objs for c in o.bound_box]
if not pts: sys.exit(0)
lo = mathutils.Vector([min(p[i] for p in pts) for i in range(3)]); hi = mathutils.Vector([max(p[i] for p in pts) for i in range(3)])
c = (lo + hi) / 2; r = max((hi - lo).length / 2, 1e-6)
cam = bpy.data.objects.new("cam", bpy.data.cameras.new("cam")); bpy.context.scene.collection.objects.link(cam)
d = mathutils.Vector(tuple(float(x) for x in a[2].split(","))).normalized() if len(a) > 2 else mathutils.Vector((1.0, -1.3, 0.9)).normalized()
cam.location = c + d * r * 2.6; cam.rotation_euler = (c - cam.location).to_track_quat('-Z', 'Y').to_euler()
cam.data.clip_end = r * 100; cam.data.clip_start = r / 1000
sc = bpy.context.scene; sc.camera = cam
w = bpy.data.worlds.new('w'); w.color = (0.55, 0.55, 0.6); sc.world = w
sc.display.shading.background_type = 'WORLD' if hasattr(sc.display.shading, 'background_type') else None
sc.render.engine = 'BLENDER_WORKBENCH'; sc.display.shading.light = 'STUDIO'; sc.display.shading.color_type = 'TEXTURE'
sc.display.shading.show_cavity = True
sc.render.resolution_x, sc.render.resolution_y = 900, 700
sc.render.filepath = a[1]; bpy.ops.render.render(write_still=True)
