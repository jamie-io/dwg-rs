// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

import { GLTFExporter } from 'three/addons/exporters/GLTFExporter.js';

import { DWGError } from './core.js';
import { copyBuffer, WorkerPool } from './WorkerPool.js';

export { DWGError };

const DEFAULT_OPTIONS = {
	/** Drawing units: 'mm', 'cm', 'm', 'in', 'ft', ... (three.js meters are scaled). */
	units: 'mm',
	/** DWG version: 'r2018', 'r2013' or 'r2010'. */
	version: 'r2018',
	/** Milliseconds before an export is given up (0: no limit). */
	timeout: 0,
};

/**
 * An exporter for AutoCAD DWG files: three's GLTFExporter writes the object
 * as GLB, and gltf2dwg, compiled to WebAssembly, encodes it in a worker.
 * Meshes become MESH entities, materials layers, DWG materials and embedded
 * OFML material archives (OFML applications show the textures; AutoCAD the
 * colors).
 *
 * ```js
 * const dwg = await new DWGExporter().parseAsync( object );
 * ```
 */
class DWGExporter {

	constructor() {

		this.wasmPath = new URL( '../wasm/dwg2gltf.wasm', import.meta.url ).href;
		this.workerURL = null; // null: ./DWGWorker.js next to this file
		this.workerLimit = 1;
		this.options = { ...DEFAULT_OPTIONS };
		this._pool = new WorkerPool( this, 'DWGExporter' );

	}

	/** Where dwg2gltf.wasm is (default: ../wasm/dwg2gltf.wasm next to this file). */
	setWasmPath( path ) {

		this.wasmPath = path;
		this._pool.forgetModule();
		return this;

	}

	/** Where DWGWorker.js is, when a bundler moves it. Must be same-origin. */
	setWorkerURL( url ) {

		this.workerURL = url;
		return this;

	}

	/** Change the defaults (see DEFAULT_OPTIONS above). */
	setOptions( options ) {

		Object.assign( this.options, checked( options ) );
		return this;

	}

	/** The DWG file of `object` (an ArrayBuffer). `options` override the exporter's. */
	async parseAsync( object, options = {} ) {

		let glb;
		try {

			glb = await new GLTFExporter().parseAsync( object, { binary: true } );

		} catch ( e ) {

			throw new DWGError( 'gltf', `GLTFExporter cannot write the object: ${e.message}`, { cause: e } );

		}

		return ( await this.convert( glb, options ) ).dwg;

	}

	/**
	 * Encode a GLB file: `{ dwg, report, stderr, convertMs, memoryBytes }`.
	 * The caller's buffer is copied, not detached.
	 */
	async convert( glb, options = {} ) {

		const opts = { ...this.options, ...checked( options ) };
		const data = copyBuffer( glb );
		const request = { units: opts.units, version: opts.version };
		const { dwg, report, stderr, convertMs, memoryBytes } = await this._pool.run( { type: 'export', glb: data, options: request }, [ data ], opts.timeout );
		return { dwg, report, stderr, convertMs, memoryBytes };

	}

	/** Compile the WebAssembly module now instead of on the first export. */
	preload() {

		this._pool.preload();
		return this;

	}

	/** Stop the worker; pending exports fail. */
	dispose() {

		this._pool.dispose();
		return this;

	}

}

function checked( options ) {

	for ( const key of Object.keys( options ) ) {

		if ( ! ( key in DEFAULT_OPTIONS ) ) throw new DWGError( 'options', `DWGExporter: unknown option ${key}` );

	}

	return options;

}

export { DWGExporter };
