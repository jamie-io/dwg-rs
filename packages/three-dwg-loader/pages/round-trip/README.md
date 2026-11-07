# Round-trip samples

DWG files written by `gltf2dwg` from glTF sample models of Khronos
([glTF-Sample-Assets](https://github.com/KhronosGroup/glTF-Sample-Assets),
CC0), for the demo page. They show the round trip glTF to DWG to glTF, not how
other programs write DWG; the drawings of other programs are the BricsCAD test
drawings in `tests/bricscad`.

| File | Model | License |
| --- | --- | --- |
| `lantern.dwg` | Lantern, Microsoft | CC0-1.0 |
| `water-bottle.dwg` | WaterBottle, Microsoft | CC0-1.0 |

Each `<name>.json` records the source (pinned to one commit), the converter
commit that wrote the DWG, and what the source holds: triangles, area,
extents and the texture maps of each material. The page compares the read-back
model with these.

The images are re-encoded at most 1024 px wide (JPEG when opaque) before
writing, to keep the files small. To make them again (needs Pillow and
`cargo build --release`):

```sh
python3 packages/three-dwg-loader/pages/round-trip/make.py
```
