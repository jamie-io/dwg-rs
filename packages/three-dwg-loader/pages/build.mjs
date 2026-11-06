// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

// Assembles the static demo site into OUT_DIR: the page, the loader, the
// WebAssembly module, three.js (from node_modules) and the sample drawings of
// gallery.json, and renders the gallery thumbnails with the page itself in
// headless Chrome (software WebGL). Run by scripts/build-pages.sh.
//
//   node build.mjs OUT_DIR [--no-thumbs] [--chrome PATH] [--gallery LIST.json]
//
// OUT_DIR is emptied first: it must be empty or an earlier build (a .git
// entry in it is kept, so it can be a worktree of the gh-pages branch).

import { spawn } from 'node:child_process';
import fs from 'node:fs';
import http from 'node:http';
import os from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';

const here = path.dirname( fileURLToPath( import.meta.url ) );
const pkg = path.resolve( here, '..' );
const repo = path.resolve( pkg, '../..' );
const samples = path.join( repo, 'tests/bricscad/dwg/v27' );

const args = process.argv.slice( 2 );
const option = ( name ) => args.includes( name ) ? args[ args.indexOf( name ) + 1 ] : null;
const out = args.find( ( a, i ) => ! a.startsWith( '--' ) && ! [ '--chrome', '--gallery' ].includes( args[ i - 1 ] ) );
if ( ! out ) {

	console.error( 'usage: node build.mjs OUT_DIR [--no-thumbs] [--chrome PATH] [--gallery LIST.json]' );
	process.exit( 2 );

}

const thumbs = ! args.includes( '--no-thumbs' );
const chromeArg = option( '--chrome' );
const galleryList = path.resolve( option( '--gallery' ) ?? path.join( here, 'gallery.json' ) );
const outDir = path.resolve( out );

// -- files --------------------------------------------------------------

function copy( from, to ) {

	fs.mkdirSync( path.dirname( path.join( outDir, to ) ), { recursive: true } );
	fs.copyFileSync( from, path.join( outDir, to ) );

}

fs.mkdirSync( outDir, { recursive: true } );
const existing = fs.readdirSync( outDir ).filter( ( entry ) => entry !== '.git' );
if ( existing.length > 0 && ! existing.includes( '.nojekyll' ) ) {

	// not an earlier build: refuse to empty it
	console.error( `${outDir} is neither empty nor an earlier build of the site` );
	process.exit( 1 );

}

for ( const entry of existing ) {

	if ( entry !== '.git' ) fs.rmSync( path.join( outDir, entry ), { recursive: true, force: true } );

}

copy( path.join( here, 'index.html' ), 'index.html' );
copy( path.join( here, 'app.js' ), 'app.js' );
fs.writeFileSync( path.join( outDir, '.nojekyll' ), '' );

for ( const file of [ 'DWGLoader.js', 'core.js', 'wasi.js', 'WorkerPool.js', 'DWGWorker.js' ] ) {

	copy( path.join( pkg, 'src', file ), `loader/src/${file}` );

}

const wasm = path.join( pkg, 'wasm/dwg2gltf.wasm' );
if ( ! fs.existsSync( wasm ) ) {

	console.error( `missing ${path.relative( repo, wasm )}: run scripts/build-wasm.sh` );
	process.exit( 1 );

}

copy( wasm, 'loader/wasm/dwg2gltf.wasm' );

const three = path.join( pkg, 'node_modules/three' );
if ( ! fs.existsSync( three ) ) {

	console.error( 'missing node_modules/three: run npm ci in packages/three-dwg-loader' );
	process.exit( 1 );

}

for ( const file of [
	'build/three.module.js', 'build/three.core.js',
	'examples/jsm/controls/OrbitControls.js', 'examples/jsm/environments/RoomEnvironment.js',
	'examples/jsm/loaders/GLTFLoader.js', 'examples/jsm/utils/BufferGeometryUtils.js',
	'examples/jsm/utils/SkeletonUtils.js',
] ) {

	copy( path.join( three, file ), `vendor/three/${file}` );

}

// with an extension, so the server sends it as text
copy( path.join( three, 'LICENSE' ), 'vendor/three/LICENSE.txt' );

// -- gallery ------------------------------------------------------------

const source = JSON.parse( fs.readFileSync( galleryList, 'utf8' ) );
const gallery = { start: source.start, groups: [] };
for ( const group of source.groups ) {

	const items = [];
	for ( const { file, title } of group.items ) {

		const name = path.basename( file );
		const id = name.replace( /\.dwg$/i, '' );
		const version = /_(AC\d{4})\.dwg$/i.exec( name )?.[ 1 ] ?? null;
		copy( path.join( samples, file ), `gallery/${name}` );
		items.push( { id, title, version, file: `gallery/${name}` } );

	}

	gallery.groups.push( { title: group.title, items } );

}

const writeGallery = () => fs.writeFileSync( path.join( outDir, 'gallery.json' ), JSON.stringify( gallery, null, '\t' ) + '\n' );
writeGallery();

if ( thumbs ) await renderThumbnails();
report();

// -- thumbnails ---------------------------------------------------------

async function renderThumbnails() {

	const server = serve( outDir );
	await new Promise( ( resolve ) => server.listen( 0, '127.0.0.1', resolve ) );
	const base = `http://127.0.0.1:${server.address().port}/dwg-rs/`;
	const profile = fs.mkdtempSync( path.join( os.tmpdir(), 'dwg-rs-pages-' ) );
	const chrome = spawn( findChrome(), [
		'--headless=new', `--user-data-dir=${profile}`, '--remote-debugging-port=0',
		'--use-angle=swiftshader', '--enable-unsafe-swiftshader', '--ignore-gpu-blocklist',
		'--no-first-run', '--no-default-browser-check', '--disable-extensions',
		'--window-size=1200,800', 'about:blank',
	], { stdio: [ 'ignore', 'ignore', 'pipe' ] } );
	let chromeLog = '';
	chrome.stderr.on( 'data', ( d ) => chromeLog = ( chromeLog + d ).slice( -4000 ) );
	let running = true;
	const exited = new Promise( ( resolve ) => chrome.once( 'close', () => {

		running = false;
		resolve();

	} ) );

	try {

		const page = await connect( profile, () => running || `Chrome exited:\n${chromeLog}` );
		const errors = [];
		page.on( 'Runtime.exceptionThrown', ( p ) => errors.push( p.exceptionDetails.exception?.description ?? p.exceptionDetails.text ) );
		page.on( 'Runtime.consoleAPICalled', ( p ) => {

			if ( p.type === 'error' ) errors.push( p.args.map( ( a ) => a.value ?? a.description ).join( ' ' ) );

		} );
		await page.send( 'Runtime.enable' );
		await page.send( 'Page.enable' );
		// the hash keeps the page from opening the start drawing
		await page.send( 'Page.navigate', { url: `${base}index.html#build` } );
		await waitFor( page, 'typeof window.demo?.thumbnail === "function"' );

		for ( const group of gallery.groups ) {

			for ( const item of group.items ) {

				const { result, exceptionDetails } = await page.send( 'Runtime.evaluate', {
					expression: `demo.thumbnail(${JSON.stringify( item.file )}, 320)`,
					awaitPromise: true, returnByValue: true,
				} );
				if ( exceptionDetails ) {

					console.error( `  ${item.id}: ${exceptionDetails.exception?.description ?? exceptionDetails.text}` );
					continue;

				}

				const { image, triangles, convertMs, warnings } = result.value;
				const thumb = `gallery/${item.id}.webp`;
				fs.writeFileSync( path.join( outDir, thumb ), Buffer.from( image.split( ',' )[ 1 ], 'base64' ) );
				item.thumb = thumb;
				console.log( `  ${item.id.padEnd( 32 )} ${String( triangles ).padStart( 7 )} triangles  ${String( Math.round( convertMs ) ).padStart( 5 )} ms  ${warnings} warnings` );

			}

		}

		if ( errors.length ) console.error( `page errors:\n  ${errors.join( '\n  ' )}` );
		writeGallery();

	} finally {

		chrome.kill();
		server.close();
		await exited;
		fs.rmSync( profile, { recursive: true, force: true } );

	}

}

function findChrome() {

	if ( chromeArg ) return chromeArg;
	for ( const name of [ 'google-chrome-stable', 'google-chrome', 'chromium', 'chromium-browser' ] ) {

		for ( const dir of ( process.env.PATH ?? '' ).split( path.delimiter ) ) {

			if ( fs.existsSync( path.join( dir, name ) ) ) return path.join( dir, name );

		}

	}

	throw new Error( 'no Chrome or Chromium on PATH (pass --chrome, or --no-thumbs)' );

}

/** Static files of `root` under /dwg-rs/, as GitHub Pages serves a project site. */
function serve( root ) {

	const types = {
		'.html': 'text/html', '.js': 'text/javascript', '.json': 'application/json',
		'.wasm': 'application/wasm', '.webp': 'image/webp', '.dwg': 'application/octet-stream',
	};
	return http.createServer( ( req, res ) => {

		const url = new URL( req.url, 'http://localhost' );
		const rel = decodeURIComponent( url.pathname ).replace( /^\/dwg-rs\//, '' );
		const file = path.join( root, rel || 'index.html' );
		if ( ! file.startsWith( root ) || ! fs.existsSync( file ) || fs.statSync( file ).isDirectory() ) {

			res.writeHead( 404 ).end();
			return;

		}

		res.writeHead( 200, { 'content-type': types[ path.extname( file ) ] ?? 'application/octet-stream' } );
		fs.createReadStream( file ).pipe( res );

	} );

}

/** A DevTools Protocol session with the first page of the Chrome using `profile`; `alive()` is true while it runs. */
async function connect( profile, alive ) {

	const portFile = path.join( profile, 'DevToolsActivePort' );
	for ( let i = 0; i < 600 && ! fs.existsSync( portFile ); i ++ ) {

		if ( alive() !== true ) throw new Error( alive() );
		await sleep( 100 );

	}

	if ( ! fs.existsSync( portFile ) ) throw new Error( 'Chrome did not open its DevTools port' );
	const port = fs.readFileSync( portFile, 'utf8' ).split( '\n' )[ 0 ];
	let target;
	for ( let i = 0; i < 100 && ! target; i ++ ) {

		const list = await ( await fetch( `http://127.0.0.1:${port}/json/list` ) ).json();
		target = list.find( ( t ) => t.type === 'page' );
		if ( ! target ) await sleep( 100 );

	}

	const ws = new WebSocket( target.webSocketDebuggerUrl );
	await new Promise( ( resolve, reject ) => {

		ws.onopen = resolve;
		ws.onerror = reject;

	} );
	let nextId = 1;
	const pending = new Map();
	const listeners = new Map();
	ws.onmessage = ( event ) => {

		const message = JSON.parse( event.data );
		if ( message.id ) {

			const { resolve, reject } = pending.get( message.id );
			pending.delete( message.id );
			if ( message.error ) reject( new Error( message.error.message ) );
			else resolve( message.result );

		} else {

			for ( const fn of listeners.get( message.method ) ?? [] ) fn( message.params );

		}

	};

	return {
		send( method, params = {} ) {

			const id = nextId ++;
			ws.send( JSON.stringify( { id, method, params } ) );
			return new Promise( ( resolve, reject ) => pending.set( id, { resolve, reject } ) );

		},
		on( method, fn ) {

			listeners.set( method, [ ...( listeners.get( method ) ?? [] ), fn ] );

		},
	};

}

async function waitFor( page, expression, ms = 30000 ) {

	for ( const end = Date.now() + ms; Date.now() < end; await sleep( 100 ) ) {

		const { result } = await page.send( 'Runtime.evaluate', { expression, returnByValue: true } );
		if ( result.value === true ) return;

	}

	throw new Error( `timed out waiting for ${expression}` );

}

function sleep( ms ) {

	return new Promise( ( resolve ) => setTimeout( resolve, ms ) );

}

function report() {

	let total = 0;
	const walk = ( dir ) => {

		for ( const entry of fs.readdirSync( dir, { withFileTypes: true } ) ) {

			if ( entry.name === '.git' ) continue;
			const p = path.join( dir, entry.name );
			if ( entry.isDirectory() ) walk( p );
			else total += fs.statSync( p ).size;

		}

	};

	walk( outDir );
	const items = gallery.groups.reduce( ( n, g ) => n + g.items.length, 0 );
	console.log( `site: ${( total / 1048576 ).toFixed( 1 )} MB, ${items} sample drawings` );

}
