// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

import { FileLoader, Group, Loader, LoaderUtils } from 'three';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';

import { DWGError } from './core.js';
import { copyBuffer, WorkerPool } from './WorkerPool.js';

export { DWGError };

const DEFAULT_OPTIONS = {
	/** Wire geometry (lines, arcs, polylines). */
	lines: true,
	/** OFML 2D plan symbols (outlines on "..._D2_..." layers). */
	planSymbols: false,
	/** Drawing units instead of the file's INSUNITS: 'mm', 'cm', 'm', 'in', 'ft', ...
	 * 'auto': the file's, but meters when the drawing would be less than 1 cm
	 * across in millimeters, centimeters, decimeters, inches, feet or yards
	 * (OFML geometry stored in meters). */
	units: null,
	/** Keep the DWG's Z-up axes (three.js is Y-up). */
	zUp: false,
	/** Series DWGs (block libraries): 'auto' converts one per article when
	 * model space is empty; true always does, false never. */
	blocks: 'auto',
	/** Series DWGs: convert only these articles (block names). */
	only: null,
	/** Image files of DWG materials: 'auto' fetches them next to the DWG
	 * (when its location is known: load(), or parseAsync with a path), an
	 * object supplies them ({'wood.jpg': ArrayBuffer}), false skips them. */
	textures: 'auto',
	/** Quantized, meshopt-compressed geometry (smaller GLBs from convert()). */
	compress: false,
	/** Milliseconds before a conversion is given up (0: no limit). */
	timeout: 0,
};

const WASM_OPTIONS = [ 'lines', 'planSymbols', 'units', 'zUp', 'compress', 'only' ];

/** PNG or JPEG, which glTF can embed. */
function isImage( data ) {

	const b = new Uint8Array( data, 0, Math.min( 8, data.byteLength ) );
	const png = [ 0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a ].every( ( v, i ) => b[ i ] === v );
	const jpeg = b[ 0 ] === 0xff && b[ 1 ] === 0xd8 && b[ 2 ] === 0xff;
	return png || jpeg;

}

/**
 * A loader for AutoCAD DWG files: dwg2gltf, compiled to WebAssembly, turns
 * the drawing into glTF in a worker, and GLTFLoader builds the scene. The
 * result is a Group (model space) or a Group with one child per article
 * (series DWGs); `userData.dwg` tells what was converted.
 *
 * ```js
 * const loader = new DWGLoader();
 * const object = await loader.loadAsync( 'chair.dwg' );
 * scene.add( object );
 * ```
 */
class DWGLoader extends Loader {

	constructor( manager ) {

		super( manager );
		this.wasmPath = new URL( '../wasm/dwg2gltf.wasm', import.meta.url ).href;
		this.workerURL = null; // null: ./DWGWorker.js next to this file
		this.workerLimit = 2;
		this.options = { ...DEFAULT_OPTIONS };
		this.gltfLoader = null;
		this._pool = new WorkerPool( this, 'DWGLoader' );

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

	/** How many conversions run at the same time (one worker each). */
	setWorkerLimit( limit ) {

		this.workerLimit = Math.max( 1, limit );
		return this;

	}

	/** Change the defaults (see DEFAULT_OPTIONS above). */
	setOptions( options ) {

		for ( const key of Object.keys( options ) ) {

			if ( ! ( key in DEFAULT_OPTIONS ) ) throw new DWGError( 'options', `DWGLoader: unknown option ${key}` );

		}

		Object.assign( this.options, options );
		return this;

	}

	/** The GLTFLoader that builds the scenes (to share its decoders and settings). */
	setGLTFLoader( gltfLoader ) {

		this.gltfLoader = gltfLoader;
		return this;

	}

	load( url, onLoad, onProgress, onError ) {

		const scope = this;
		let resourcePath;
		if ( this.resourcePath !== '' ) {

			resourcePath = this.resourcePath;

		} else if ( this.path !== '' ) {

			resourcePath = LoaderUtils.resolveURL( LoaderUtils.extractUrlBase( url ), this.path );

		} else {

			resourcePath = LoaderUtils.extractUrlBase( url );

		}

		this.manager.itemStart( url );
		const _onError = ( e ) => {

			if ( onError ) onError( e );
			else console.error( e );
			scope.manager.itemError( url );
			scope.manager.itemEnd( url );

		};

		const name = decodeURIComponent( url.split( /[?#]/ )[ 0 ].split( '/' ).pop() );
		const loader = new FileLoader( this.manager );
		loader.setPath( this.path );
		loader.setResponseType( 'arraybuffer' );
		loader.setRequestHeader( this.requestHeader );
		loader.setWithCredentials( this.withCredentials );
		loader.load( url, ( data ) => {

			this.parseAsync( data, resourcePath, { name } ).then( ( object ) => {

				onLoad( object );
				scope.manager.itemEnd( url );

			}, _onError );

		}, onProgress, _onError );

	}

	/**
	 * Convert a DWG file and build its three.js object. `path` is where
	 * material image files are fetched from (textures: 'auto').
	 */
	parse( data, path, onLoad, onError ) {

		this.parseAsync( data, path ).then( onLoad, onError ?? console.error );

	}

	/**
	 * Promise of the three.js object. `options` override the loader's for
	 * this file; `name` (the file name) names the object and error messages.
	 */
	async parseAsync( data, path = this.resourcePath, options = {} ) {

		const opts = { ...this.options, ...options };
		const name = options.name ?? 'drawing.dwg';
		const result = await this.convert( data, { ...opts, name, path } );
		const t0 = performance.now();
		const gltfLoader = await this._gltfLoader( opts.compress );
		const scenes = [];
		for ( const { name: block, glb } of result.blocks ) {

			try {

				scenes.push( ( await gltfLoader.parseAsync( glb, '' ) ).scene );

			} catch ( e ) {

				throw new DWGError( 'gltf', `three.js cannot read the converted ${block}: ${e.message}`, { report: result.report, cause: e } );

			}

		}

		const report = result.report;
		const stem = name.replace( /\.dwg$/i, '' );
		let object;
		if ( report.mode === 'model' ) {

			object = scenes[ 0 ];

		} else {

			object = new Group();
			result.blocks.forEach( ( { name: block }, i ) => {

				scenes[ i ].name = block;
				scenes[ i ].userData.dwgBlock = block;
				object.add( scenes[ i ] );

			} );

		}

		object.name = stem;
		object.userData.dwg = {
			mode: report.mode,
			summary: report.summary,
			stats: report.stats,
			warnings: report.stats.warnings,
			blocks: result.blocks.map( ( b ) => b.name ),
			empty: report.empty,
			modelSpaceContent: report.modelSpaceContent,
			missingTextures: report.missingTextures,
			timings: { convertMs: result.convertMs, parseMs: performance.now() - t0 },
			memoryBytes: result.memoryBytes,
		};
		return object;

	}

	/**
	 * Convert without building three.js objects: `{ blocks: [{ name, glb }],
	 * report, stderr, convertMs, memoryBytes }`, each `glb` an ArrayBuffer
	 * (e.g. to save, or to parse later). Options as for parseAsync, plus
	 * `path` for textures: 'auto'.
	 */
	async convert( data, options = {} ) {

		const opts = { ...this.options, ...options };
		let request = { name: opts.name ?? 'drawing.dwg' };
		for ( const key of WASM_OPTIONS ) request[ key ] = opts[ key ];
		request.mode = opts.blocks === true ? 'series' : opts.blocks === false ? 'model' : 'auto';
		// the module reads them as JSON anyway; this also unwraps proxies
		// (reactive state), which postMessage cannot clone
		request = JSON.parse( JSON.stringify( request ) );

		let textures = [];
		if ( opts.textures && typeof opts.textures === 'object' ) {

			textures = Object.entries( opts.textures ).map( ( [ name, file ] ) => ( { name, data: copyBuffer( file ) } ) );

		}

		let result = await this._run( copyBuffer( data ), request, textures, opts.timeout );
		const missing = result.report.missingTextures;
		// a buffer without a location (path '') has nothing next to it
		if ( opts.textures === 'auto' && missing.length > 0 && typeof opts.path === 'string' && opts.path !== '' ) {

			const found = await this._fetchTextures( missing, opts.path );
			if ( found.length > 0 ) result = await this._run( copyBuffer( data ), request, found, opts.timeout );

		}

		const { report, glbs, stderr, convertMs, memoryBytes } = result;
		return { blocks: glbs, report, stderr, convertMs, memoryBytes };

	}

	/** Compile the WebAssembly module now instead of on the first file. */
	preload() {

		this._pool.preload();
		return this;

	}

	/** Stop the workers; pending conversions fail. */
	dispose() {

		this._pool.dispose();
		return this;

	}

	// -- internals --------------------------------------------------------

	async _gltfLoader( compressed ) {

		if ( this.gltfLoader === null ) this.gltfLoader = new GLTFLoader( this.manager );
		if ( compressed && this.gltfLoader.meshoptDecoder === null ) {

			const { MeshoptDecoder } = await import( 'three/addons/libs/meshopt_decoder.module.js' );
			this.gltfLoader.setMeshoptDecoder( MeshoptDecoder );

		}

		return this.gltfLoader;

	}

	async _fetchTextures( names, path ) {

		const init = {
			credentials: this.withCredentials ? 'include' : 'same-origin',
			headers: this.requestHeader,
		};
		const files = await Promise.all( names.map( async ( name ) => {

			const url = this.manager.resolveURL( LoaderUtils.resolveURL( encodeURIComponent( name ), path ) );
			try {

				const response = await fetch( url, init );
				const data = response.ok ? await response.arrayBuffer() : null;
				// servers can answer 200 with an HTML page for any path
				return data && isImage( data ) ? { name, data } : null;

			} catch {

				return null; // missing textures are reported, not fatal

			}

		} ) );
		return files.filter( Boolean );

	}

	_run( dwg, options, textures, timeout ) {

		return this._pool.run( { type: 'convert', dwg, options, textures }, [ dwg, ...textures.map( ( t ) => t.data ) ], timeout );

	}

}

export { DWGLoader };
