# Changelog

## 2026-11-06

- scripts/build-dll.sh builds dwg2gltf.dll for Windows without the build machine's paths in it
- QA runs compare only drawings whose GLB changed, start with the slowest drawings and use all cores when memory allows
- First public release
- C interface: dwg2gltf.dll / libdwg2gltf.so (`crates/dwg2gltf-capi`, `include/dwg2gltf.h`) with the scene as arrays for native viewers (meshes, instance tree, materials, textures, warnings, bounds) and the GLB
- Visible-edge extraction per mesh (boundary, non-manifold and crease edges), used by the C interface
- Delphi unit for the C interface (`bindings/delphi/Dwg2Gltf.pas`), C and Delphi examples, `scripts/test-delphi.sh`
