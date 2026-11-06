#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 Jamie Jahn
# SPDX-License-Identifier: GPL-3.0-or-later
"""Write DwgViewer.res: DwgViewer.manifest as the application manifest
(resource type RT_MANIFEST = 24, id 1), in the 32-bit .res format the
Delphi linker reads. Saves needing a resource compiler:
    python3 make_res.py   (next to DwgViewer.manifest)"""
import os
import struct

here = os.path.dirname(os.path.abspath(__file__))


def entry(type_id, name_id, data, flags=0x1030, language=0x0409):
    # header: sizes, type and name as 0xFFFF + ordinal, then the fixed part
    header = struct.pack('<HHHH', 0xFFFF, type_id, 0xFFFF, name_id)
    header += struct.pack('<IHHII', 0, flags, language, 0, 0)
    out = struct.pack('<II', len(data), 8 + len(header)) + header + data
    return out + b'\0' * (-len(out) % 4)


with open(os.path.join(here, 'DwgViewer.manifest'), 'rb') as f:
    manifest = f.read()
res = entry(0, 0, b'', flags=0, language=0)  # the empty entry every .res starts with
res += entry(24, 1, manifest)
with open(os.path.join(here, 'DwgViewer.res'), 'wb') as f:
    f.write(res)
