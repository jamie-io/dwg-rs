// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

// DWGLoader's worker pool and texture fetching, with a stand-in Worker
// (Node has no web workers) that clones messages like postMessage does.

import { beforeEach, test } from 'node:test';
import assert from 'node:assert/strict';

import { DWGError, DWGLoader } from '../src/DWGLoader.js';

/** The smallest valid module: the pool only passes it on. */
const MODULE = new WebAssembly.Module( new Uint8Array( [ 0, 97, 115, 109, 1, 0, 0, 0 ] ) );

function report( fields = {} ) {

	return {
		abi: 1, mode: 'model', blocks: [], empty: [], modelSpaceContent: true, summary: '', missingTextures: [],
		stats: { converted: {}, skipped: {}, repaired: {}, warnings: [], instances: 0, triangles: 0 },
		...fields,
	};

}

class FakeWorker {

	static created = 0;
	/** Throw from the constructor (a cross-origin or bad worker URL). */
	static failSpawn = false;
	/** postMessage throws for messages of this type, as for uncloneable data. */
	static failPost = null;
	/** The convert messages received, cloned. */
	static conversions = [];
	/** The answer to a convert message. */
	static respond = () => ( { type: 'result', report: report(), glbs: [] } );

	constructor() {

		if ( FakeWorker.failSpawn ) throw new DOMException( 'worker URL on another origin', 'SecurityError' );
		FakeWorker.created ++;

	}

	postMessage( message, transfer ) {

		if ( message.type === FakeWorker.failPost ) throw new DOMException( 'could not be cloned', 'DataCloneError' );
		const copy = structuredClone( message, { transfer } ); // throws DataCloneError like postMessage
		if ( copy.type !== 'convert' ) return;
		FakeWorker.conversions.push( copy );
		setTimeout( () => this.onmessage?.( { data: { id: copy.id, ...FakeWorker.respond( copy ) } } ) );

	}

	terminate() {}

}

beforeEach( () => {

	globalThis.Worker = FakeWorker;
	FakeWorker.created = 0;
	FakeWorker.failSpawn = false;
	FakeWorker.failPost = null;
	FakeWorker.conversions = [];
	FakeWorker.respond = () => ( { type: 'result', report: report(), glbs: [] } );

} );

const dwg = () => new Uint8Array( 16 );

test( 'options from reactive state (proxies) reach the worker', { timeout: 2000 }, async () => {

	const loader = new DWGLoader().setWasmPath( MODULE ).setWorkerLimit( 1 );
	// Vue and MobX state are proxies, which postMessage cannot clone
	const only = new Proxy( [ 'Chair A' ], {} );
	await loader.convert( dwg(), { only } );
	assert.deepEqual( FakeWorker.conversions[ 0 ].options.only, [ 'Chair A' ] );
	loader.dispose();

} );

test( 'a message the worker cannot take fails that conversion only', { timeout: 2000 }, async () => {

	const loader = new DWGLoader().setWasmPath( MODULE ).setWorkerLimit( 1 );
	// e.g. a browser that cannot hand the compiled module to a worker
	FakeWorker.failPost = 'init';
	await assert.rejects( loader.convert( dwg() ), ( e ) => {

		assert.ok( e instanceof DWGError, String( e ) );
		assert.equal( e.code, 'worker' );
		return true;

	} );
	// the worker slot is free again
	FakeWorker.failPost = null;
	await loader.convert( dwg() );
	assert.equal( FakeWorker.conversions.length, 1 );
	loader.dispose();

} );

test( 'a worker that cannot be created fails the conversion, not the queue', { timeout: 2000 }, async () => {

	const loader = new DWGLoader().setWasmPath( MODULE );
	FakeWorker.failSpawn = true;
	for ( let i = 0; i < 2; i ++ ) {

		await assert.rejects( loader.convert( dwg() ), ( e ) => e instanceof DWGError && e.code === 'worker' );

	}

	FakeWorker.failSpawn = false;
	await loader.convert( dwg() );
	// nothing left over from the failed attempts runs
	assert.equal( FakeWorker.conversions.length, 1 );
	loader.dispose();

} );

test( 'textures: auto fetches images next to a known location only', { timeout: 2000 }, async () => {

	const fetched = [];
	let body = null;
	globalThis.fetch = async ( url ) => {

		fetched.push( String( url ) );
		return new Response( body, { status: 200 } );

	};

	FakeWorker.respond = ( message ) => ( {
		type: 'result',
		report: report( { missingTextures: message.textures.length ? [] : [ 'Wood.PNG' ] } ),
		glbs: [],
	} );
	const loader = new DWGLoader().setWasmPath( MODULE ).setWorkerLimit( 1 );

	// bytes without a location: nothing to look next to
	await loader.convert( dwg(), { path: '' } );
	await loader.convert( dwg() );
	assert.deepEqual( fetched, [] );
	assert.equal( FakeWorker.conversions.length, 2 );

	// a single-page app's fallback answers 200 with HTML: not an image
	body = '<!doctype html><title>app</title>';
	await loader.convert( dwg(), { path: 'https://example.com/models/' } );
	assert.deepEqual( fetched, [ 'https://example.com/models/Wood.PNG' ] );
	assert.equal( FakeWorker.conversions.length, 3 );

	// a PNG is supplied, and the drawing converted again with it
	body = new Uint8Array( [ 0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a, 0, 0, 0, 0 ] );
	await loader.convert( dwg(), { path: 'https://example.com/models/' } );
	assert.equal( FakeWorker.conversions.length, 5 );
	assert.deepEqual( FakeWorker.conversions[ 4 ].textures.map( ( t ) => t.name ), [ 'Wood.PNG' ] );
	loader.dispose();

} );
