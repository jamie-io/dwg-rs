// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

// DWGExporter end to end in Node: a stand-in Worker runs DWGWorker.js in
// this process (Node has no web workers), and a stand-in FileReader serves
// GLTFExporter's binary output.

import { before, test } from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { BoxGeometry, Group, Mesh, MeshStandardMaterial } from 'three';

import { compile, convert } from '../src/core.js';
import { DWGError, DWGExporter } from '../src/DWGExporter.js';
import { bounds, boxGlb, parseGlb } from './glb.js';

const pkg = path.resolve( path.dirname( fileURLToPath( import.meta.url ) ), '..' );
const wasm = fs.readFileSync( path.resolve( pkg, process.env.DWG_WASM ?? 'wasm/dwg2gltf.wasm' ) );
const module = await compile( wasm );

/** Runs DWGWorker.js here; one at a time, like the tests use it. */
class InProcessWorker {

	constructor() {

		const scope = { postMessage: ( data ) => setTimeout( () => this.onmessage?.( { data } ) ) };
		this.ready = ( async () => {

			globalThis.self = scope;
			await import( `../src/DWGWorker.js?instance=${InProcessWorker.count ++}` );
			return scope;

		} )();

	}

	static count = 0;

	postMessage( message, transfer ) {

		const copy = structuredClone( message, { transfer } );
		this.ready.then( ( scope ) => scope.onmessage( { data: copy } ) );

	}

	terminate() {}

}

before( () => {

	globalThis.Worker = InProcessWorker;
	globalThis.FileReader = class {

		readAsArrayBuffer( blob ) {

			blob.arrayBuffer().then( ( result ) => {

				this.result = result;
				this.onloadend?.();

			} );

		}

	};

} );

const exporter = () => new DWGExporter().setWasmPath( wasm );
const magic = ( dwg ) => new TextDecoder().decode( new Uint8Array( dwg, 0, 6 ) );
/** The size of the drawing read back (`options`: the reader's, e.g. units). */
const size = async ( dwg, options = {} ) => {

	const b = bounds( parseGlb( ( await convert( module, dwg, options ) ).glbs[ 0 ].glb ) );
	return [ 0, 1, 2 ].map( ( i ) => Math.round( ( b.max[ i ] - b.min[ i ] ) * 1e6 ) / 1e6 );

};

test( 'parseAsync: a three.js object becomes a DWG in millimeters', async () => {

	const group = new Group();
	const box = new Mesh( new BoxGeometry( 0.5, 0.8, 0.4 ), new MeshStandardMaterial( { color: 0x996633 } ) );
	box.position.set( 1, 0.4, 0 );
	group.add( box );
	const dwg = await exporter().parseAsync( group );
	assert.equal( magic( dwg ), 'AC1032' );
	assert.deepEqual( await size( dwg ), [ 0.5, 0.8, 0.4 ] );
	// millimeters by default: taken as meters, the coordinates are 1000 times larger
	assert.deepEqual( await size( dwg, { units: 'm' } ), [ 500, 800, 400 ] );

} );

test( 'convert: a GLB, with the report; options per call', async () => {

	const result = await exporter().convert( boxGlb(), { version: 'r2010' } );
	assert.equal( magic( result.dwg ), 'AC1024' );
	assert.equal( result.report.stats.faces, 12 );
	assert.ok( result.convertMs >= 0 );

} );

test( 'the caller keeps its buffer', async () => {

	const glb = boxGlb();
	await exporter().convert( glb.buffer );
	assert.equal( glb.byteLength > 0, true );

} );

test( 'two exports at once both finish', async () => {

	const e = exporter();
	const [ a, b ] = await Promise.all( [ e.convert( boxGlb() ), e.convert( boxGlb( { size: [ 2, 2, 2 ] } ) ) ] );
	assert.deepEqual( await size( a.dwg ), [ 1, 2, 3 ] );
	assert.deepEqual( await size( b.dwg ), [ 2, 2, 2 ] );

} );

test( 'errors are DWGErrors with a code', async () => {

	await assert.rejects( exporter().convert( new Uint8Array( 8 ) ), ( e ) => e instanceof DWGError && e.code === 'input' );
	assert.throws( () => exporter().setOptions( { lines: true } ), ( e ) => e instanceof DWGError && e.code === 'options' );
	await assert.rejects( exporter().convert( boxGlb(), { colour: 'red' } ), ( e ) => e instanceof DWGError && e.code === 'options' );

} );

test( 'dispose fails what is pending', async () => {

	const e = exporter();
	const pending = e.convert( boxGlb() );
	e.dispose();
	await assert.rejects( pending, ( err ) => err instanceof DWGError && err.code === 'worker' );

} );
