# Changelog

## 2026-11-06

- ACIS curved faces take far fewer triangles where the surface parameterisation is stretched (spline surfaces, some tori): refinement works in the surface's own metric, and conversions of such drawings run many times faster
- scripts/build-dll.sh builds dwg2gltf.dll for Windows without the build machine's paths in it
- QA runs compare only drawings whose GLB changed, start with the slowest drawings and use all cores when memory allows
- C interface: dwg2gltf.dll / libdwg2gltf.so (`crates/dwg2gltf-capi`, `include/dwg2gltf.h`) with the scene as arrays for native viewers (meshes, instance tree, materials, textures, warnings, bounds) and the GLB
- Visible-edge extraction per mesh (boundary, non-manifold and crease edges), used by the C interface
- Delphi unit for the C interface (`bindings/delphi/Dwg2Gltf.pas`), C and Delphi examples, `scripts/test-delphi.sh`
- Demo site for GitHub Pages: open DWG files in 3D in the browser (converted locally by the WebAssembly loader), with a gallery of test drawings; built by `scripts/build-pages.sh`
- The WebAssembly module no longer contains the build machine's source paths
- First public release
