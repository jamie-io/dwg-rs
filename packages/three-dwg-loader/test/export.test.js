// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

// GLB -> DWG with dwg2gltf.wasm (gltf2dwg's code path), checked by
// converting the DWG back.

import { test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

import { compile, convert, DWGError, exportDWG } from '../src/core.js';
import { bounds, boxGlb, parseGlb } from './glb.js';

const pkg = path.resolve( path.dirname( fileURLToPath( import.meta.url ) ), '..' );
const module = await compile( fs.readFileSync( path.resolve( pkg, process.env.DWG_WASM ?? 'wasm/dwg2gltf.wasm' ) ) );

const round = ( v ) => Math.round( v * 1e6 ) / 1e6 + 0;
const magic = ( dwg ) => new TextDecoder().decode( new Uint8Array( dwg, 0, 6 ) );

async function rejectsWith( promise, code ) {

	await assert.rejects( promise, ( e ) => {

		assert.ok( e instanceof DWGError, String( e ) );
		assert.equal( e.code, code, `${e.code}: ${e.message}\n${e.stderr}` );
		return true;

	} );

}

test( 'a GLB becomes a DWG that converts back to the same box', async () => {

	const { dwg, report } = await exportDWG( module, boxGlb(), {} );
	assert.equal( magic( dwg ), 'AC1032' );
	assert.equal( report.stats.meshes, 1 );
	assert.equal( report.stats.faces, 12 );
	assert.match( report.summary, /^written: MESH=1/ );

	const back = await convert( module, dwg, {} );
	assert.equal( back.report.stats.triangles, 12 );
	const b = bounds( parseGlb( back.glbs[ 0 ].glb ) );
	assert.deepEqual( [ 0, 1, 2 ].map( ( i ) => round( b.max[ i ] - b.min[ i ] ) ), [ 1, 2, 3 ] );

} );

test( 'units and version', async () => {

	const height = async ( dwg, options ) => {

		const b = bounds( parseGlb( ( await convert( module, dwg, options ) ).glbs[ 0 ].glb ) );
		return round( b.max[ 1 ] - b.min[ 1 ] );

	};

	const mm = await exportDWG( module, boxGlb(), { units: 'mm', version: 'r2013' } );
	assert.equal( magic( mm.dwg ), 'AC1027' );
	// the drawing is in millimeters: read with its units, the size is the same
	assert.equal( await height( mm.dwg, {} ), 2 );
	// taken as meters, its coordinates are millimeters
	assert.equal( await height( mm.dwg, { units: 'm' } ), 2000 );
	const m = await exportDWG( module, boxGlb(), {} );
	assert.equal( await height( m.dwg, { units: 'm' } ), 2 );

} );

test( 'an embedded texture is kept', async () => {

	const { report } = await exportDWG( module, boxGlb( { texture: true } ), {} );
	assert.equal( report.stats.textures, 1 );

} );

test( 'a GLB without meshes gives an empty drawing or a clean error', async () => {

	const empty = new TextEncoder().encode( JSON.stringify( { asset: { version: '2.0' }, scenes: [ { nodes: [] } ], scene: 0 } ) );
	try {

		const { dwg } = await exportDWG( module, empty, {} );
		assert.equal( magic( dwg ), 'AC1032' );

	} catch ( e ) {

		assert.ok( e instanceof DWGError && [ 'input', 'write' ].includes( e.code ), String( e ) );

	}

} );

test( 'what is not a GLB is refused, never a crash', async () => {

	let seed = 7;
	const noise = new Uint8Array( 4096 ).map( () => ( seed = ( seed * 1103515245 + 12345 ) >>> 0 ) >>> 24 );
	const glb = boxGlb();
	for ( const data of [ new Uint8Array( 0 ), noise, glb.subarray( 0, 40 ), glb.subarray( 0, glb.length - 100 ) ] ) {

		await rejectsWith( exportDWG( module, data, {} ), 'input' );

	}

} );

test( 'bad export options are refused', async () => {

	await rejectsWith( exportDWG( module, boxGlb(), { units: 'parsec' } ), 'options' );
	await rejectsWith( exportDWG( module, boxGlb(), { version: 'r14' } ), 'options' );
	await rejectsWith( exportDWG( module, boxGlb(), { lines: true } ), 'options' );

} );

test( 'the worker answers an export message', async () => {

	const answers = [];
	globalThis.self = { postMessage: ( message ) => answers.push( message ) };
	await import( '../src/DWGWorker.js' );
	const scope = globalThis.self;
	await scope.onmessage( { data: { type: 'init', module } } );
	await scope.onmessage( { data: { type: 'export', id: 7, glb: boxGlb(), options: { units: 'mm' } } } );
	assert.equal( answers.length, 1 );
	assert.equal( answers[ 0 ].type, 'result', JSON.stringify( answers[ 0 ].error ) );
	assert.equal( answers[ 0 ].id, 7 );
	assert.equal( magic( answers[ 0 ].dwg ), 'AC1032' );

	await scope.onmessage( { data: { type: 'export', id: 8, glb: new Uint8Array( 3 ), options: {} } } );
	assert.equal( answers[ 1 ].type, 'error' );
	assert.equal( answers[ 1 ].error.code, 'input' );
	delete globalThis.self;

} );
