# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
# Tile PNGs two per row: blender -b -P tools/qa/sheet.py -- out.png a.png b.png ...
import bpy, sys
a=sys.argv[sys.argv.index("--")+1:]; out=a[0]; files=a[1:]
imgs=[bpy.data.images.load(f) for f in files]
w,h=imgs[0].size; cols=2; rows=(len(imgs)+1)//2
px=[0.15]*(w*cols*h*rows*4)
for k,im in enumerate(imgs):
    p=list(im.pixels); ox=(k%cols)*w; oy=(rows-1-k//cols)*h
    for y in range(h):
        d=((oy+y)*w*cols+ox)*4; px[d:d+w*4]=p[y*w*4:(y+1)*w*4]
o=bpy.data.images.new("s",w*cols,h*rows); o.pixels=px; o.filepath_raw=out; o.file_format="PNG"; o.save()
