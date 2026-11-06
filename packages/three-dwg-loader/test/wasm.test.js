// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

// dwg2gltf.wasm through core.js, against the native command line tool.
//
//   npm test                                  # wasm/dwg2gltf.wasm
//   DWG_WASM=wasm/dwg2gltf.dev.wasm npm test  # dev build (overflow checks)
//
// Needs the module (scripts/build-wasm.sh). The comparisons need the native
// build (cargo build --release; or DWG2GLTF_CLI) and a local folder of
// drawings (samples/, not in git, or DWG2GLTF_SAMPLES); the series fixture
// needs cargo. Tests without them are skipped.

import { after, describe, test } from 'node:test';
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { createRequire } from 'node:module';
import { fileURLToPath } from 'node:url';

import { compile, convert, DWGError } from '../src/core.js';
import { assertSameGlb, bounds, modes, parseGlb } from './glb.js';

const pkg = path.resolve( path.dirname( fileURLToPath( import.meta.url ) ), '..' );
const repo = path.resolve( pkg, '../..' );
const wasmPath = path.resolve( pkg, process.env.DWG_WASM ?? 'wasm/dwg2gltf.wasm' );
const samplesDir = process.env.DWG2GLTF_SAMPLES ?? path.join( repo, 'samples' );
const cli = process.env.DWG2GLTF_CLI ?? path.join( repo, 'target/release/dwg2gltf' );
const validator = createRequire( import.meta.url )( 'gltf-validator' );

/** libm differs between glibc and wasi-libc: an ulp here and there. */
const TOLERANCE = 1e-6;

function dwgFiles( dir ) {

	if ( ! fs.existsSync( dir ) ) return [];
	return fs.readdirSync( dir, { withFileTypes: true } ).sort( ( a, b ) => a.name < b.name ? - 1 : 1 ).flatMap( ( e ) => {

		const p = path.join( dir, e.name );
		if ( e.isDirectory() ) return dwgFiles( p );
		return /\.dwg$/i.test( e.name ) ? [ p ] : [];

	} );

}

const samples = dwgFiles( samplesDir ).filter( ( f ) => ! f.split( path.sep ).includes( 'invalid' ) );
const haveCli = fs.existsSync( cli );
const module = await compile( fs.readFileSync( wasmPath ) );
const tmp = fs.mkdtempSync( path.join( os.tmpdir(), 'three-dwg-loader-' ) );
after( () => fs.rmSync( tmp, { recursive: true, force: true } ) );

/** The series fixture (crates/dwg2gltf/examples/series_fixture.rs), or null. */
const series = ( () => {

	const fixture = path.join( tmp, 'series.dwg' );
	const r = spawnSync( 'cargo', [ 'run', '--release', '--offline', '-q', '--example', 'series_fixture', '--', fixture ], { cwd: repo } );
	return r.status === 0 ? fixture : null;

} )();

/** Rounded to a micrometer, -0 as 0. */
const round = ( v ) => Math.round( v * 1e6 ) / 1e6 + 0;

/** The CLI's summary (its stdout without the "a -> b" line and indent). */
function runCli( args ) {

	const r = spawnSync( cli, args, { encoding: 'utf8' } );
	assert.equal( r.status, 0, r.stderr );
	return r.stdout.split( '\n' ).slice( 1 ).map( ( l ) => l.replace( /^ {2}/, '' ) ).join( '\n' ).trimEnd();

}

async function validate( glb, label ) {

	const result = await validator.validateBytes( new Uint8Array( glb ), { maxIssues: 10 } );
	assert.equal( result.issues.numErrors, 0, `${label}: ${JSON.stringify( result.issues.messages )}` );
	return result.issues;

}

/** A rejection with this DWGError code. */
async function rejectsWith( promise, code ) {

	await assert.rejects( promise, ( e ) => {

		assert.ok( e instanceof DWGError, String( e ) );
		assert.equal( e.code, code, `${e.code}: ${e.message}\n${e.stderr}` );
		return true;

	} );

}

test( 'the module imports only WASI and exports the C ABI', () => {

	const imports = new Set( WebAssembly.Module.imports( module ).map( ( i ) => i.module ) );
	assert.deepEqual( [ ...imports ], [ 'wasi_snapshot_preview1' ] );
	for ( const e of WebAssembly.Module.exports( module ) ) {

		assert.ok( e.name === 'memory' || e.name.startsWith( 'dgl_' ) || e.name === '_initialize', e.name );

	}

} );

test( 'every sample converts like the command line tool', { skip: ! haveCli || samples.length === 0 }, async () => {

	for ( const file of samples ) {

		const label = path.relative( samplesDir, file );
		const glbPath = path.join( tmp, 'cli.glb' );
		const summary = runCli( [ file, '-o', glbPath ] );
		// the command line converts model space unless told otherwise
		const r = await convert( module, fs.readFileSync( file ), { name: path.basename( file ), mode: 'model' } );
		assert.equal( r.report.summary, summary, label );
		assert.equal( r.report.mode, 'model' );
		assert.equal( r.glbs.length, 1 );
		assertSameGlb( r.glbs[ 0 ].glb, fs.readFileSync( glbPath ), TOLERANCE, label );
		await validate( r.glbs[ 0 ].glb, label );
		assert.equal( r.stderr, '', label );

	}

} );

describe( 'series DWGs', { skip: series === null && 'needs cargo for the fixture' }, () => {

	test( 'one GLB per article, like --seriesdwg', { skip: ! haveCli }, async () => {

		const files = [ series, ...samples ];
		for ( const file of files ) {

			const data = fs.readFileSync( file );
			const r = await convert( module, data, { name: path.basename( file ) } );
			if ( r.report.mode !== 'series' ) continue;
			const label = path.basename( file );
			const dir = path.join( tmp, 'series-' + path.basename( file, path.extname( file ) ) );
			const summary = runCli( [ '--seriesdwg', file, '-o', dir ] );
			assert.equal( r.report.summary, summary, label );
			const written = path.join( dir, path.basename( file, path.extname( file ) ) );
			const cliFiles = fs.readdirSync( written ).sort();
			assert.equal( r.glbs.length, cliFiles.length, label );
			for ( const { name, glb } of r.glbs ) {

				// the command line names files after blocks, made safe for Windows
				const match = cliFiles.find( ( f ) => f === `${name}.glb` ) ?? cliFiles.find( ( f ) => f.startsWith( name.replace( /[<>:"/\\|?*]/g, '_' ) ) );
				assert.ok( match, `${label}: no file for block ${name}` );
				assertSameGlb( glb, fs.readFileSync( path.join( written, match ) ), TOLERANCE, `${label} ${name}` );
				await validate( glb, `${label} ${name}` );

			}

		}

	} );

	test( 'the fixture: blocks, base points and the series options', async () => {

		const data = fs.readFileSync( series );
		const auto = await convert( module, data, {} );
		assert.equal( auto.report.mode, 'series' );
		assert.deepEqual( auto.glbs.map( ( g ) => g.name ), [ 'Chair A', 'Table B' ] );
		assert.equal( auto.report.modelSpaceContent, false );
		assert.deepEqual( auto.report.stats.converted, { INSERT: 4, MESH: 4 } );

		// articles stand at the origin, in meters, Y up
		const chair = bounds( parseGlb( auto.glbs[ 0 ].glb ) );
		assert.deepEqual( chair.min.map( round ), [ 0, 0, - 0.45 ] );
		assert.deepEqual( chair.max.map( round ), [ 0.45, 0.9, 0 ] );
		const table = bounds( parseGlb( auto.glbs[ 1 ].glb ) );
		assert.equal( round( table.max[ 1 ] ), 0.73 );

		const forced = await convert( module, data, { mode: 'series' } );
		assert.deepEqual( forced.report.blocks, auto.report.blocks );

		const one = await convert( module, data, { only: [ 'Table B' ] } );
		assert.deepEqual( one.glbs.map( ( g ) => g.name ), [ 'Table B' ] );
		assert.equal( one.report.stats.converted.MESH, 2 );

		const model = await convert( module, data, { mode: 'model' } );
		assert.equal( model.report.mode, 'model' );
		assert.equal( model.report.stats.triangles, 0 );
		assert.match( model.report.stats.warnings[ 0 ], /series DWG/ );

		await rejectsWith( convert( module, data, { only: [ 'Sofa' ] } ), 'options' );
		await rejectsWith( convert( module, data, { mode: 'model', only: [ 'Table B' ] } ), 'options' );

	} );

	test( 'series mode on a drawing without articles', { skip: samples.length === 0 }, async () => {

		// the first sample that is not a series DWG itself
		let plain = null;
		for ( const f of samples ) {

			if ( ( await convert( module, fs.readFileSync( f ), {} ) ).report.mode === 'model' ) {

				plain = f;
				break;

			}

		}

		assert.ok( plain, 'a sample that is not a series DWG' );
		await rejectsWith( convert( module, fs.readFileSync( plain ), { mode: 'series' } ), 'no-series-blocks' );

	} );

} );

test( 'the same input gives the same bytes', { skip: samples.length === 0 }, async () => {

	// each conversion is a new instance, with new random hash seeds
	const file = samples.reduce( ( a, b ) => fs.statSync( a ).size >= fs.statSync( b ).size ? a : b );
	const data = fs.readFileSync( file );
	const a = await convert( module, data, {} );
	const b = await convert( module, data, {} );
	assert.deepEqual( new Uint8Array( a.glbs[ 0 ].glb ), new Uint8Array( b.glbs[ 0 ].glb ) );

} );

test( 'broken files are rejected cleanly', async () => {

	// a fixed pseudo-random sequence
	let seed = 12345;
	const noise = new Uint8Array( 65536 ).map( () => ( seed = ( seed * 1103515245 + 12345 ) >>> 0 ) >>> 24 );
	const inputs = [ [ 'empty', new Uint8Array( 0 ) ], [ 'noise', noise ], [ 'AC1032 + noise', new Uint8Array( [ ...new TextEncoder().encode( 'AC1032' ), ...noise ] ) ] ];
	const largest = samples.length ? samples.reduce( ( a, b ) => fs.statSync( a ).size >= fs.statSync( b ).size ? a : b ) : null;
	if ( largest ) {

		const data = fs.readFileSync( largest );
		for ( const size of [ 1024, 65536, data.length >> 1 ] ) inputs.push( [ `${path.basename( largest )} cut at ${size}`, data.subarray( 0, size ) ] );

	}

	for ( const [ label, data ] of inputs ) {

		try {

			const r = await convert( module, data, { name: label } );
			for ( const { glb } of r.glbs ) await validate( glb, label );

		} catch ( e ) {

			assert.ok( e instanceof DWGError, `${label}: ${e}` );
			assert.equal( e.code, 'read', `${label}: ${e.code} ${e.message}\n${e.stderr}` );

		}

	}

} );

test( 'options change the output', { skip: series === null && 'needs cargo for the fixture' }, async () => {

	const data = fs.readFileSync( series );
	const height = async ( options, axis ) => {

		const r = await convert( module, data, { only: [ 'Chair A' ], ...options } );
		const b = bounds( parseGlb( r.glbs[ 0 ].glb ) );
		return round( b.max[ axis ] - b.min[ axis ] );

	};

	assert.equal( await height( {}, 1 ), 0.9 ); // mm (the drawing's units) to m
	assert.equal( await height( { units: 'cm' }, 1 ), 9 );
	assert.equal( await height( { units: 'auto' }, 1 ), 0.9 ); // 900 mm: plausible, kept
	assert.equal( await height( { zUp: true }, 2 ), 0.9 );

} );

test( 'lines and plan symbols', { skip: ! samples.some( ( f ) => /_D2_/.test( f ) ) }, async () => {

	// OFML 2D plan symbols: outlines only, skipped unless asked for
	const data = fs.readFileSync( samples.find( ( f ) => /_D2_/.test( f ) ) );
	const plain = await convert( module, data, {} );
	assert.equal( plain.report.stats.triangles, 0 );
	const symbols = await convert( module, data, { planSymbols: true } );
	assert.ok( modes( parseGlb( symbols.glbs[ 0 ].glb ) ).has( 1 ), 'line primitives' );
	const noLines = await convert( module, data, { planSymbols: true, lines: false } );
	assert.ok( ! modes( parseGlb( noLines.glbs[ 0 ].glb ) ).has( 1 ) );

} );

test( 'bad options are refused', async () => {

	const data = new Uint8Array( 0 );
	await rejectsWith( convert( module, data, { units: 'parsec' } ), 'options' );
	await rejectsWith( convert( module, data, { colour: 'red' } ), 'options' );
	await rejectsWith( convert( module, data, { lines: 'yes' } ), 'options' );

} );

test( 'material image files are passed in', { skip: series === null && 'needs cargo for the fixture' }, async () => {

	// the fixture names no image file, so nothing is missing and nothing used
	const r = await convert( module, fs.readFileSync( series ), {}, [ { name: 'C:\\textures\\wood.png', data: new Uint8Array( [ 137, 80, 78, 71 ] ) } ] );
	assert.deepEqual( r.report.missingTextures, [] );
	for ( const { glb } of r.glbs ) assert.equal( parseGlb( glb ).json.images, undefined );

} );
