// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

// Runs dwg2gltf.wasm on one file: DWG bytes in, GLB files out (convert), or
// GLB in, DWG out (exportDWG). Used by the worker of the DWGLoader and the
// DWGExporter; works in Node too (no DOM, no three.js).

import { createWasi, WasiExit } from './wasi.js';

/** The C ABI version this file speaks (dgl_abi_version). */
export const ABI = 2;

const STATUS_CODES = [ 'ok', 'options', 'read', 'no-model-space', 'no-series-blocks', 'non-finite', 'other', 'input', 'write' ];

/**
 * A failed conversion or export. `code` is one of: options, read,
 * no-model-space, no-series-blocks, non-finite, other (reported by the
 * converter); input, write (reported by the export); panic, out-of-memory,
 * crash (the module trapped); exit, abi; and, from the DWGLoader or
 * DWGExporter, timeout, worker, gltf.
 */
export class DWGError extends Error {

	constructor( code, message, { stderr = '', report = null, cause } = {} ) {

		super( message, cause === undefined ? undefined : { cause } );
		this.name = 'DWGError';
		this.code = code;
		this.stderr = stderr;
		this.report = report;

	}

}

/**
 * Compile dwg2gltf.wasm. `source`: a URL (string or URL), a fetch Response,
 * bytes (ArrayBuffer or typed array) or an already compiled module.
 */
export async function compile( source ) {

	if ( source instanceof WebAssembly.Module ) return source;
	if ( source instanceof ArrayBuffer || ArrayBuffer.isView( source ) ) return WebAssembly.compile( source );
	const response = source instanceof Response ? source : await fetch( source );
	if ( ! response.ok ) {

		throw new DWGError( 'worker', `cannot load ${response.url || source}: HTTP ${response.status}` );

	}

	try {

		return await WebAssembly.compileStreaming( response.clone() );

	} catch ( e ) {

		// servers that do not send application/wasm
		if ( e instanceof TypeError ) return WebAssembly.compile( await response.arrayBuffer() );
		throw e;

	}

}

const encoder = new TextEncoder();
const decoder = new TextDecoder();

function bytesOf( data ) {

	if ( data instanceof Uint8Array ) return data;
	if ( ArrayBuffer.isView( data ) ) return new Uint8Array( data.buffer, data.byteOffset, data.byteLength );
	return new Uint8Array( data );

}

/**
 * Run `work` against a fresh instance of `module` (so nothing is left
 * behind: LibreDWG's state, memory, the effects of a crash). `work({ exports,
 * put, read, check })` returns the result's fields; `stderr` and
 * `memoryBytes` are added. Traps and exits become DWGErrors.
 */
async function run( module, work ) {

	const wasi = createWasi();
	const instance = await WebAssembly.instantiate( module, wasi.imports );
	const exports = instance.exports;
	wasi.setMemory( exports.memory );
	const fail = ( code, message, extra = {} ) => new DWGError( code, message, { stderr: wasi.stderr(), ...extra } );

	try {

		exports._initialize?.();
		const abi = exports.dgl_abi_version();
		if ( abi !== ABI ) throw fail( 'abi', `dwg2gltf.wasm speaks ABI ${abi}, this loader ${ABI}` );

		// copies into a buffer the module then owns
		const put = ( data ) => {

			const bytes = bytesOf( data );
			const ptr = exports.dgl_alloc( bytes.length ) >>> 0;
			new Uint8Array( exports.memory.buffer, ptr, bytes.length ).set( bytes );
			return [ ptr, bytes.length ];

		};

		const read = ( ptr, len ) => new Uint8Array( exports.memory.buffer, ptr >>> 0, len >>> 0 );

		// the report of the call that returned `status`; throws on failure
		const check = ( status, what ) => {

			const report = JSON.parse( decoder.decode( read( exports.dgl_report_ptr(), exports.dgl_report_len() ) ) );
			if ( status !== 0 ) {

				const code = report.error?.code ?? STATUS_CODES[ status ] ?? 'other';
				throw fail( code, report.error?.message ?? `${what} failed (status ${status})`, { report } );

			}

			return report;

		};

		const result = await work( { exports, put, read, check } );
		return { ...result, stderr: wasi.stderr(), memoryBytes: exports.memory.buffer.byteLength };

	} catch ( e ) {

		if ( e instanceof DWGError ) throw e;
		if ( e instanceof WasiExit ) throw fail( 'exit', `the converter exited (code ${e.code})`, { cause: e } );
		if ( e instanceof WebAssembly.RuntimeError ) {

			const stderr = wasi.stderr();
			const panic = /panicked at [^\n]*\n([^\n]*)/.exec( stderr );
			if ( /memory allocation of \d+ bytes failed/.test( stderr ) ) {

				throw fail( 'out-of-memory', 'the converter ran out of memory', { cause: e } );

			}

			if ( panic ) throw fail( 'panic', `the converter failed: ${panic[ 1 ] || e.message}`, { cause: e } );
			throw fail( 'crash', `the converter crashed (${e.message})`, { cause: e } );

		}

		throw e;

	}

}

/**
 * Convert one DWG file in a fresh instance of `module`.
 *
 * @param {WebAssembly.Module} module from compile()
 * @param {ArrayBuffer|ArrayBufferView} dwg the file
 * @param {object} options name, lines, planSymbols, units (a unit name, 'auto'
 *   or null), zUp, compress,
 *   mode ('auto' | 'model' | 'series'), only (block names)
 * @param {{name: string, data: ArrayBuffer|ArrayBufferView}[]} textures
 *   material image files, matched by file name
 * @returns {Promise<{report: object, glbs: {name: string, glb: ArrayBuffer}[],
 *   stderr: string, memoryBytes: number}>}
 */
export async function convert( module, dwg, options = {}, textures = [] ) {

	return run( module, ( { exports, put, read, check } ) => {

		for ( const texture of textures ) {

			exports.dgl_add_texture( ...put( encoder.encode( texture.name ) ), ...put( texture.data ) );

		}

		const report = check( exports.dgl_convert( ...put( dwg ), ...put( encoder.encode( JSON.stringify( options ) ) ) ), 'conversion' );
		const output = exports.dgl_output_ptr() >>> 0;
		const glbs = report.blocks.map( ( block ) => ( {
			name: block.name,
			glb: read( output + block.offset, block.length ).slice().buffer,
		} ) );
		return { report, glbs };

	} );

}

/**
 * Encode one GLB file as DWG (what gltf2dwg does) in a fresh instance of
 * `module`.
 *
 * @param {WebAssembly.Module} module from compile()
 * @param {ArrayBuffer|ArrayBufferView} glb the file
 * @param {object} options units ('m' default, 'mm', ...), version ('r2018'
 *   default, 'r2013', 'r2010')
 * @returns {Promise<{report: object, dwg: ArrayBuffer, stderr: string, memoryBytes: number}>}
 */
export async function exportDWG( module, glb, options = {} ) {

	return run( module, ( { exports, put, read, check } ) => {

		const report = check( exports.dgl_export( ...put( glb ), ...put( encoder.encode( JSON.stringify( options ) ) ) ), 'export' );
		const dwg = read( exports.dgl_output_ptr(), exports.dgl_output_len() ).slice().buffer;
		return { report, dwg };

	} );

}
