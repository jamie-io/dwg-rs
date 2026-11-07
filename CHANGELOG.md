# Changelog

## 2026-11-07

- Materials in DWG R14 to 2004 files are read from their `RTMaterial` XDATA; such drawings came out untextured and white
- Texture images embedded on their own (EDWG_TEXTURE_DICT, without a material archive) are used; such materials came out untextured. Textures of AutoCAD materials without a material archive are projected in drawing units, as AutoCAD applies their transform (they were 1000x too large in millimeter drawings)
- Delphi viewer example: silhouette lines, order-independent x-ray, line widths and sizes that follow the DPI, snapping to arc midpoints and circle centers, drawings converted on a worker thread, themed controls

## 2026-11-06

- QA: `qa.py run --list FILE` checks only the listed DWGs, `--settle SECONDS` skips DWGs still being written, `--group-names TSV` shows display names for groups in the report and collages
- QA: results of older runs with a `folder` column are read as `group`, so diffs work across them
- QA: tools can add their own sections to a run's report (`report.build(extra_sections=...)`, `report.add_section`)
- The drawing builder of the integration tests (`Doc`, `tempdir`) is in `tests/common/mod.rs`, usable by other test crates
- Drawings with many entities with material mappers convert much faster: each texture projection frame is found again by a hash lookup instead of a comparison with every frame so far
- Pre-2004 drawings with many polyface meshes or polylines convert in seconds instead of minutes: the vertices of each polyline are no longer looked up by scanning the whole drawing
- Delphi example: a Direct3D 11 viewer on dwg2gltf.dll (`examples/delphi/viewer`) with shaded, edge, hidden-line, wireframe and x-ray modes, instanced drawing, measuring with snapping, material presets and a block tree, built and tested by `scripts/build-delphi-viewer.sh`
- ACIS curved faces take far fewer triangles where the surface parameterisation is stretched (spline surfaces, some tori): refinement works in the surface's own metric, and conversions of such drawings run many times faster
- ACIS faces whose boundary runs along a spline's domain edge no longer show gaps from folded triangles along that edge
- scripts/build-dll.sh builds dwg2gltf.dll for Windows without the build machine's paths in it
- QA runs compare only drawings whose GLB changed, start with the slowest drawings and use all cores when memory allows
- C interface: dwg2gltf.dll / libdwg2gltf.so (`crates/dwg2gltf-capi`, `include/dwg2gltf.h`) with the scene as arrays for native viewers (meshes, instance tree, materials, textures, warnings, bounds) and the GLB
- Visible-edge extraction per mesh (boundary, non-manifold and crease edges), used by the C interface
- Delphi unit for the C interface (`bindings/delphi/Dwg2Gltf.pas`), C and Delphi examples, `scripts/test-delphi.sh`
- Demo site for GitHub Pages: open DWG files in 3D in the browser (converted locally by the WebAssembly loader), with a gallery of test drawings; built by `scripts/build-pages.sh`
- The WebAssembly module no longer contains the build machine's source paths
- First public release
