# Changelog

## 2026-11-07

- Delphi viewer example: silhouette lines, order-independent x-ray, line widths and sizes that follow the DPI, snapping to arc midpoints and circle centers, drawings converted on a worker thread, themed controls

## 2026-11-06

- Delphi example: a Direct3D 11 viewer on dwg2gltf.dll (`examples/delphi/viewer`) with shaded, edge, hidden-line, wireframe and x-ray modes, instanced drawing, measuring with snapping, material presets and a block tree, built and tested by `scripts/build-delphi-viewer.sh`
- scripts/build-dll.sh builds dwg2gltf.dll for Windows without the build machine's paths in it
- QA runs compare only drawings whose GLB changed, start with the slowest drawings and use all cores when memory allows
- C interface: dwg2gltf.dll / libdwg2gltf.so (`crates/dwg2gltf-capi`, `include/dwg2gltf.h`) with the scene as arrays for native viewers (meshes, instance tree, materials, textures, warnings, bounds) and the GLB
- Visible-edge extraction per mesh (boundary, non-manifold and crease edges), used by the C interface
- Delphi unit for the C interface (`bindings/delphi/Dwg2Gltf.pas`), C and Delphi examples, `scripts/test-delphi.sh`
- Demo site for GitHub Pages: open DWG files in 3D in the browser (converted locally by the WebAssembly loader), with a gallery of test drawings; built by `scripts/build-pages.sh`
- The WebAssembly module no longer contains the build machine's source paths
- First public release
