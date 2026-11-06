# three-dwg-loader

A [three.js](https://threejs.org) loader for AutoCAD DWG files. It runs
dwg2gltf, compiled to WebAssembly, in a worker. The converter is the same
code as the command line tool: OFML materials, textures, UVs and normals,
ACIS solids, MESH, polyfaces, block instances and series DWGs. three.js's
`GLTFLoader` then builds the scene. three.js itself is not patched.

```js
import { DWGLoader } from 'three-dwg-loader';

const loader = new DWGLoader();
const object = await loader.loadAsync( 'chair.dwg' ); // meters, Y up
scene.add( object );
console.log( object.userData.dwg.summary ); // what was converted and skipped
```

`DWGExporter` goes the other way: three.js objects to DWG, with the
`gltf2dwg` code (see [Export](#export)).

## Build

The `.wasm` file is built, not checked in. You need the `wasm32-wasip1`
Rust target and [wasi-sdk](https://github.com/WebAssembly/wasi-sdk/releases)
(unpacked to `~/opt/wasi-sdk`, or set `WASI_SDK_PATH`). binaryen's
`wasm-opt` is optional.

```sh
rustup target add wasm32-wasip1
scripts/build-wasm.sh           # -> packages/three-dwg-loader/wasm/dwg2gltf.wasm
```

The module is about 10 MB, or 1.3 MB served with brotli (2.4 MB with gzip).
Install the package into an app from this folder, for example with
`npm install ../dwg-rs/packages/three-dwg-loader`, or with `npm pack`
and the tarball. three.js (r160 or later) is a peer dependency.

## Use

`DWGLoader` works like the other three.js loaders: `load`, `loadAsync`,
`parse`, `parseAsync`, a `LoadingManager`, `setPath`, `setResourcePath`,
`setRequestHeader` and `setWithCredentials`. TypeScript finds its types
(`src/DWGLoader.d.ts`; they use `@types/three`).

| Method | |
| --- | --- |
| `loadAsync( url )` | Fetches and converts a DWG and returns its `Object3D` |
| `parseAsync( buffer, path?, options? )` | Converts an `ArrayBuffer` or typed array. The buffer is copied, never detached. `options.name` names the object |
| `convert( buffer, options? )` | `{ blocks: [{ name, glb }], report, ... }`: the GLB files without building three.js objects (to save or parse later) |
| `setOptions( options )` | Defaults for every file (see below) |
| `setWasmPath( url )` | Where `dwg2gltf.wasm` is. Default: `../wasm/dwg2gltf.wasm` next to `src/` |
| `setWorkerURL( url )` | Where `DWGWorker.js` is, if a bundler moves it. Must be same-origin |
| `setWorkerLimit( n )` | Conversions at the same time, one worker each. Default 2 |
| `setGLTFLoader( loader )` | Your own `GLTFLoader` |
| `preload()` | Compile the module now instead of on the first file |
| `dispose()` | Stop the workers |

Options:

| Option | Default | |
| --- | --- | --- |
| `lines` | `true` | Wire geometry (lines, arcs, polylines) as `LineSegments` |
| `planSymbols` | `false` | OFML 2D plan symbols (outlines on `..._D2_...` layers) |
| `units` | `null` | Override the drawing's units: `'mm'`, `'cm'`, `'dm'`, `'m'`, `'km'`, `'in'`, `'ft'`, `'yd'`, `'mi'`. `'auto'`: the drawing's, but meters when it would be less than 1 cm across in millimeters, centimeters, decimeters, inches, feet or yards; a series DWG is judged by its largest article, also when `only` asks for some. Use it for OFML DWGs, which are often stored in meters but say millimeters (see the [main README](../../README.md#drawings-in-meters-that-say-millimeters)) |
| `zUp` | `false` | Keep the DWG's Z-up axes |
| `blocks` | `'auto'` | Series DWGs (below). `true`: one object per article. `false`: model space only, like the command line without `--seriesdwg` |
| `only` | `null` | Series DWGs: convert only these articles (block names) |
| `textures` | `'auto'` | Image files that DWG materials name. `'auto'` fetches them from next to the DWG when its location is known (`load`, or `parseAsync` with a `path`) and keeps PNG and JPEG answers only. `{ 'wood.jpg': ArrayBuffer }` supplies them. `false` skips them. Embedded OFML material archives carry their textures, so they need none of this |
| `compress` | `false` | Quantized, meshopt-compressed GLBs (`convert()`). The loader adds three's `MeshoptDecoder` when needed |
| `timeout` | `0` | Milliseconds before a conversion is abandoned and its worker replaced. `0` means no limit |

### The result

- A drawing gives a `Group` (`gltf.scene`), named after the file.
- A **series DWG** (a block library: model space empty, every article a block
  that nothing inserts) gives a `Group` with one child per article. Each
  child is named after its block (`child.userData.dwgBlock`), and its base
  point is at the origin. With `blocks: 'auto'`, a drawing counts as a
  series DWG when model space is empty and such blocks exist.
- `object.userData.dwg` has the following fields:
  - `summary`: the command line tool's text.
  - `stats`: `converted`, `skipped`, `repaired`, `warnings`, `triangles`, `instances`.
  - `warnings`: the converter's warnings (also in `stats`), such as a
    drawing less than 1 cm across in its units. They name the command line
    options: `--units m` is `units: 'm'` here, `--seriesdwg` is
    `blocks: true`. Nothing logs them.
  - `mode`: `'model'` or `'series'`.
  - `blocks`, `empty`, `missingTextures`.
  - `modelSpaceContent`: whether model space has anything to show (a
    series DWG's is usually empty).
  - `timings`: `convertMs` and `parseMs`.
  - `memoryBytes`: the WebAssembly memory used.

### Errors

Failures reject with a `DWGError` (exported) whose `code` is one of the
following. It also carries `stderr` (the converter's output) and `report`.

| `code` | |
| --- | --- |
| `read` | LibreDWG cannot decode the file (not a DWG, damaged, or an unsupported version) |
| `options` | An unknown or invalid option, or `only` naming a block that is not an article |
| `no-model-space`, `no-series-blocks` | The drawing has no model space, or `blocks: true` was used on a drawing without articles |
| `panic`, `crash`, `out-of-memory` | The converter failed (a bug; please keep the file). The next file gets a fresh instance |
| `timeout` | The `timeout` option ran out |
| `worker` | The worker or `dwg2gltf.wasm` could not be loaded |
| `gltf` | three.js could not read the converted file (export: `GLTFExporter` could not write the object) |
| `input`, `write` | Export: the GLB was refused (e.g. Draco or KTX2), or LibreDWG could not encode it |

## Export

```js
import { DWGExporter } from 'three-dwg-loader/exporter';

const exporter = new DWGExporter();
const dwg = await exporter.parseAsync( object ); // an ArrayBuffer

// save it
const url = URL.createObjectURL( new Blob( [ dwg ], { type: 'application/acad' } ) );
Object.assign( document.createElement( 'a' ), { href: url, download: 'model.dwg' } ).click();
setTimeout( () => URL.revokeObjectURL( url ) );
```

three's `GLTFExporter` writes the object as GLB, and the module encodes it
the way `gltf2dwg` does. Each mesh becomes a `MESH` entity. Each material
becomes a layer in its color, a DWG material and an embedded OFML material
archive with the texture. OFML applications show the textures; AutoCAD and
BricsCAD show the colors. What `GLTFExporter` cannot write (custom shader materials, for
example) comes out without its texture.

`setOptions` (or the second argument) takes:

| Option | Default | |
| --- | --- | --- |
| `units` | `'mm'` | Drawing units; three.js meters are scaled to them |
| `version` | `'r2018'` | `'r2018'`, `'r2013'` or `'r2010'` |
| `timeout` | `0` | Milliseconds before an export is given up (0: no limit) |

`convert( glb )` encodes a GLB file you already have and resolves to
`{ dwg, report, stderr, convertMs, memoryBytes }`; `report.summary` is
`gltf2dwg`'s text. `setWasmPath`, `setWorkerURL`, `preload` and `dispose`
work as on the loader, and the exporter uses the same worker and module
files. LibreDWG writes uncompressed files, so even a small drawing takes
about 300 KB.

## Deploying

- Serve `dwg2gltf.wasm` as `application/wasm` so it compiles while it
  downloads. Other MIME types still work, but compile only after the
  download.
- A Content Security Policy needs `script-src 'wasm-unsafe-eval'`.
- The worker is a module worker (Chrome 80, Firefox 114, Safari 15).
- The worker and the `.wasm` are located with
  `new URL( ..., import.meta.url )`, the pattern Vite and webpack 5 bundle
  (not tried here yet). Vite's dependency pre-bundling moves files and
  breaks such URLs: add `optimizeDeps: { exclude: [ 'three-dwg-loader' ] }`.
  Without a bundler, or if it misses them, serve `src/` and `wasm/` as they
  are, or point `setWorkerURL` (to `DWGWorker.js`, with `core.js` and
  `wasi.js` next to it) and `setWasmPath` at your copies.
- Angular (`@angular/build:application`) can copy them with `assets`
  entries in `angular.json`, then `setWorkerURL( 'assets/dwg/DWGWorker.js' )`
  and `setWasmPath( 'assets/dwg/dwg2gltf.wasm' )`. Relative URLs follow the
  page's `<base href>`.

  ```json
  { "glob": "*.js", "input": "node_modules/three-dwg-loader/src", "output": "assets/dwg" },
  { "glob": "dwg2gltf.wasm", "input": "node_modules/three-dwg-loader/wasm", "output": "assets/dwg" }
  ```
- The package and the module are GPL-3.0-or-later: dwg2gltf and LibreDWG
  are. An app that ships them must offer their source.

## Demo and tests

```sh
npm install                       # three.js and the glTF validator, for the demo and tests
npm run demo                      # http://localhost:8080/demo/ (drop a DWG and its texture images)
npm test                          # needs wasm/dwg2gltf.wasm; compares with target/release/dwg2gltf
DWG_WASM=wasm/dwg2gltf.dev.wasm npm test   # after scripts/build-wasm.sh --dev (overflow checks)
```

The demo also loads URLs: `demo/?url=/path/to/file.dwg`.

The public demo site (GitHub Pages: the page in `pages/`, the loader, the
module, three.js and a gallery of the BricsCAD test drawings with
thumbnails rendered in headless Chrome) is built with
`scripts/build-pages.sh <out-dir>`.

The tests convert a generated series DWG
(`crates/dwg2gltf/examples/series_fixture.rs`) and every DWG in a local
`samples/` folder (not in git; or `DWG2GLTF_SAMPLES=<dir>`; the tests that
need one are skipped without it), and compare them with the
command line tool. The summaries must be equal and the GLB files the same,
up to float32 rounding. The libm differs between glibc and wasi-libc, so a
few vertices can be one ulp apart. The tests also cover broken files,
options and determinism. The export tests write generated boxes as DWG and
read them back (size, units, version, textures), feed the exporter broken
input, and run `DWGExporter` end to end with the worker in the same process.

Speed is about 1.3 to 1.5 times the native tool's time in Node. On two
example drawings:

| Drawing | Native | Node | Chrome (worker) | Memory |
| --- | --- | --- | --- | --- |
| Sofa (2.1 MB, 34,000 triangles, textured) | 130 ms | 190 ms | 320 ms | 75 MB |
| Chair (0.8 MB, ACIS solids) | 100 ms | 135 ms | 170 ms | 23 MB |

The Chrome times include the transfer to and from the worker and were taken
in a background tab. Building the three.js objects takes another 10 to 20 ms.
