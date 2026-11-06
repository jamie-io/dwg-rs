# Changelog

## 2026-11-06

- QA: `qa.py run --list FILE` checks only the listed DWGs, `--settle SECONDS` skips DWGs still being written, `--group-names TSV` shows display names for groups in the report and collages
- QA: results of older runs with a `folder` column are read as `group`, so diffs work across them
- QA: tools can add their own sections to a run's report (`report.build(extra_sections=...)`, `report.add_section`)
- The drawing builder of the integration tests (`Doc`, `tempdir`) is in `tests/common/mod.rs`, usable by other test crates
- Drawings with many entities with material mappers convert much faster: each texture projection frame is found again by a hash lookup instead of a comparison with every frame so far
- Pre-2004 drawings with many polyface meshes or polylines convert in seconds instead of minutes: the vertices of each polyline are no longer looked up by scanning the whole drawing
- scripts/build-dll.sh builds dwg2gltf.dll for Windows without the build machine's paths in it
- QA runs compare only drawings whose GLB changed, start with the slowest drawings and use all cores when memory allows
- C interface: dwg2gltf.dll / libdwg2gltf.so (`crates/dwg2gltf-capi`, `include/dwg2gltf.h`) with the scene as arrays for native viewers (meshes, instance tree, materials, textures, warnings, bounds) and the GLB
- Visible-edge extraction per mesh (boundary, non-manifold and crease edges), used by the C interface
- Delphi unit for the C interface (`bindings/delphi/Dwg2Gltf.pas`), C and Delphi examples, `scripts/test-delphi.sh`
- Demo site for GitHub Pages: open DWG files in 3D in the browser (converted locally by the WebAssembly loader), with a gallery of test drawings; built by `scripts/build-pages.sh`
- The WebAssembly module no longer contains the build machine's source paths
- First public release
