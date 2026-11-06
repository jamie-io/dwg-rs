// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

// The demo page: a three.js viewer, the DWGLoader (dwg2gltf compiled to
// WebAssembly, run in a worker) and the sample gallery (gallery.json).

import * as THREE from 'three';
import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
import { RoomEnvironment } from 'three/addons/environments/RoomEnvironment.js';

import { DWGLoader } from './loader/src/DWGLoader.js';

const $ = ( id ) => document.getElementById( id );

const VERSIONS = {
	AC1012: 'R13', AC1014: 'R14', AC1015: '2000', AC1018: '2004',
	AC1021: '2007', AC1024: '2010', AC1027: '2013', AC1032: '2018',
};

// -- viewer -------------------------------------------------------------

/**
 * Tone mapping, a soft environment and a key light: drawings are often plain
 * white (color 7), which would render as a flat silhouette otherwise.
 */
function light( r, s ) {

	r.toneMapping = THREE.AgXToneMapping;
	r.toneMappingExposure = 1.0;
	s.environment = new THREE.PMREMGenerator( r ).fromScene( new RoomEnvironment(), 0.04 ).texture;
	s.environmentIntensity = 0.55;
	const key = new THREE.DirectionalLight( 0xffffff, 1.8 );
	key.position.set( - 0.6, 1, 0.7 );
	s.add( key );

}

const canvas = $( 'view' );
const renderer = new THREE.WebGLRenderer( { canvas, antialias: true } );
renderer.setPixelRatio( Math.min( window.devicePixelRatio, 2 ) );
const scene = new THREE.Scene();
light( renderer, scene );
const camera = new THREE.PerspectiveCamera( 40, 1, 0.01, 1000 );
camera.position.set( 2, 1.5, 3 );
const controls = new OrbitControls( camera, canvas );
controls.enableDamping = true;
const grid = new THREE.GridHelper( 10, 20, 0x888888, 0xbbbbbb );
grid.material.transparent = true;
grid.material.opacity = 0.3;
scene.add( grid );

const dark = matchMedia( '(prefers-color-scheme: dark)' );
function applyTheme() {

	scene.background = new THREE.Color( getComputedStyle( document.documentElement ).getPropertyValue( '--view' ).trim() );

}

applyTheme();
dark.addEventListener( 'change', applyTheme );

function resize() {

	const { clientWidth: w, clientHeight: h } = canvas.parentElement;
	if ( w === 0 || h === 0 ) return;
	renderer.setSize( w, h, false );
	camera.aspect = w / h;
	camera.updateProjectionMatrix();

}

new ResizeObserver( resize ).observe( canvas.parentElement );
resize();
renderer.setAnimationLoop( () => {

	controls.update();
	renderer.render( scene, camera );

} );

/** Point `cam` (and `ctrl`, if given) at `object` from the usual 3/4 view; the bounding box. */
function frame( object, cam, ctrl ) {

	const box = new THREE.Box3().setFromObject( object );
	if ( box.isEmpty() ) return box;
	const size = box.getSize( new THREE.Vector3() );
	const center = box.getCenter( new THREE.Vector3() );
	const radius = Math.max( size.length() / 2, 1e-6 );
	const fov = THREE.MathUtils.degToRad( cam.fov / 2 );
	const distance = 1.1 * radius / Math.sin( Math.min( fov, Math.atan( Math.tan( fov ) * cam.aspect ) ) );
	cam.position.copy( center ).add( new THREE.Vector3( 0.6, 0.45, 0.8 ).normalize().multiplyScalar( distance ) );
	cam.near = distance / 100;
	cam.far = distance * 100;
	cam.lookAt( center );
	cam.updateProjectionMatrix();
	if ( ctrl ) ctrl.target.copy( center );
	return box;

}

function frameCurrent() {

	if ( ! current?.object ) return;
	const box = frame( current.object, camera, controls );
	if ( box.isEmpty() ) return;
	const radius = box.getSize( new THREE.Vector3() ).length() / 2;
	grid.position.set( ( box.min.x + box.max.x ) / 2, box.min.y, ( box.min.z + box.max.z ) / 2 );
	grid.scale.setScalar( Math.max( radius, 1e-6 ) / 3 );

}

function dispose( object ) {

	object.traverse( ( o ) => {

		o.geometry?.dispose();
		for ( const m of [ o.material ].flat().filter( Boolean ) ) {

			for ( const value of Object.values( m ) ) if ( value?.isTexture ) value.dispose();
			m.dispose();

		}

	} );

}

function setWireframe( object, on ) {

	object.traverse( ( o ) => {

		if ( ! o.isMesh ) return;
		for ( const m of [ o.material ].flat() ) m.wireframe = on;

	} );

}

/** Triangles and line segments of `object`. */
function count( object ) {

	let triangles = 0, segments = 0;
	object.traverse( ( o ) => {

		const g = o.geometry;
		if ( ! g ) return;
		const n = g.index ? g.index.count : g.attributes.position.count;
		if ( o.isMesh ) triangles += n / 3;
		else if ( o.isLineSegments ) segments += n / 2;
		else if ( o.isLine ) segments += Math.max( n - 1, 0 );

	} );
	return { triangles, segments };

}

// -- converting ---------------------------------------------------------

const loader = new DWGLoader().preload();
const fmt = new Intl.NumberFormat( 'en' );

/** What is shown: { name, data (DWG bytes), textures, version, object, glb }. */
let current = null;
let wireframe = false;
let lines = true;
let busy = 0;

function setBusy( text ) {

	$( 'busy' ).textContent = text;
	$( 'busy' ).classList.toggle( 'hidden', ! text );

}

/** The DWG version from the file header ("AC1032"). */
function versionOf( data ) {

	const magic = new TextDecoder().decode( new Uint8Array( data, 0, Math.min( 6, data.byteLength ) ) );
	return /^AC\d{4}$/.test( magic ) ? magic : null;

}

function showInfo( { title, stats = [], error = null, warnings = [], summary = '' } ) {

	$( 'info' ).classList.remove( 'hidden' );
	$( 'infoTitle' ).textContent = title;
	$( 'stats' ).replaceChildren( ...stats.flatMap( ( [ k, v ] ) => [
		Object.assign( document.createElement( 'dt' ), { textContent: k } ),
		Object.assign( document.createElement( 'dd' ), { textContent: v } ),
	] ) );
	$( 'error' ).textContent = error ?? '';
	$( 'error' ).classList.toggle( 'hidden', ! error );
	$( 'warningsBox' ).classList.toggle( 'hidden', warnings.length === 0 );
	$( 'warningsSummary' ).textContent = `${warnings.length} warning${warnings.length === 1 ? '' : 's'}`;
	$( 'warnings' ).textContent = warnings.join( '\n' );
	$( 'reportBox' ).classList.toggle( 'hidden', ! summary );
	$( 'summary' ).textContent = summary;

}

async function show( entry ) {

	const ticket = ++ busy;
	const { name, data, textures } = entry;
	const version = versionOf( data );
	setBusy( `Converting ${name}…` );
	$( 'empty' ).classList.add( 'hidden' );
	const t0 = performance.now();
	let object;
	let result;
	try {

		// convert() keeps the GLB for the download; parse it with the loader's GLTFLoader
		result = await loader.convert( data, { name, lines, textures: textures ?? false } );
		object = await buildObject( result, name );

	} catch ( e ) {

		if ( ticket !== busy ) return;
		console.warn( e );
		setBusy( null );
		showInfo( {
			title: name,
			stats: version ? [ [ 'DWG version', `${version} (${VERSIONS[ version ] ?? '?'})` ] ] : [],
			error: e.code ? `Conversion failed (${e.code}): ${e.message}` : `Conversion failed: ${e.message ?? e}`,
			summary: e.stderr ?? '',
		} );
		return;

	}

	if ( ticket !== busy ) {

		dispose( object );
		return;

	}

	const totalMs = performance.now() - t0;
	if ( current?.object ) {

		scene.remove( current.object );
		dispose( current.object );

	}

	current = { ...entry, object, result };
	setWireframe( object, wireframe );
	scene.add( object );
	frameCurrent();
	setBusy( null );

	const { triangles, segments } = count( object );
	const report = result.report;
	const warnings = report.stats?.warnings ?? [];
	const stats = [
		[ 'DWG version', version ? `${version} (${VERSIONS[ version ] ?? '?'})` : 'unknown' ],
		[ 'Triangles', fmt.format( triangles ) ],
	];
	if ( segments ) stats.push( [ 'Line segments', fmt.format( segments ) ] );
	stats.push(
		[ 'Conversion', `${fmt.format( Math.round( result.convertMs ) )} ms (WebAssembly)` ],
		[ 'Total', `${fmt.format( Math.round( totalMs ) )} ms, GLB ${( result.blocks.reduce( ( s, b ) => s + b.glb.byteLength, 0 ) / 1024 ).toFixed( 0 )} KB` ],
	);
	if ( report.mode === 'series' ) stats.push( [ 'Blocks', String( result.blocks.length ) ] );
	if ( report.missingTextures?.length ) stats.push( [ 'Missing textures', report.missingTextures.join( ', ' ) ] );
	showInfo( {
		title: name,
		stats,
		warnings: warnings.map( ( w ) => typeof w === 'string' ? w : JSON.stringify( w ) ),
		summary: report.summary ?? '',
	} );
	for ( const id of [ 'fit', 'wire', 'download' ] ) $( id ).disabled = false;

}

let gltfLoader = null;

/** The three.js object of a convert() result, built like DWGLoader.parseAsync does. */
async function buildObject( result, name ) {

	if ( gltfLoader === null ) {

		const { GLTFLoader } = await import( 'three/addons/loaders/GLTFLoader.js' );
		gltfLoader = new GLTFLoader();

	}

	const group = new THREE.Group();
	group.name = name.replace( /\.dwg$/i, '' );
	for ( const { name: block, glb } of result.blocks ) {

		const scene = ( await gltfLoader.parseAsync( glb.slice( 0 ), '' ) ).scene;
		scene.name = block;
		group.add( scene );

	}

	if ( result.report.mode === 'series' ) {

		// lay the blocks of a block library out in a row
		let x = 0;
		for ( const child of group.children ) {

			const box = new THREE.Box3().setFromObject( child );
			if ( box.isEmpty() ) continue;
			child.position.x += x - box.min.x;
			x += ( box.max.x - box.min.x ) * 1.15;

		}

	}

	return group;

}

function download() {

	if ( ! current?.result ) return;
	const stem = current.name.replace( /\.dwg$/i, '' );
	for ( const { name, glb } of current.result.blocks ) {

		const a = document.createElement( 'a' );
		a.href = URL.createObjectURL( new Blob( [ glb ], { type: 'model/gltf-binary' } ) );
		a.download = ( current.result.blocks.length > 1 ? `${stem}-${name}` : stem ) + '.glb';
		a.click();
		setTimeout( () => URL.revokeObjectURL( a.href ), 1000 );

	}

}

// -- files and gallery --------------------------------------------------

async function openFiles( files ) {

	const dwg = files.find( ( f ) => /\.dwg$/i.test( f.name ) );
	if ( ! dwg ) {

		showInfo( { title: 'No DWG file', error: 'Choose a file ending in .dwg (texture images may come along).' } );
		return;

	}

	const images = files.filter( ( f ) => f !== dwg && /\.(png|jpe?g)$/i.test( f.name ) );
	const textures = images.length ? {} : null;
	for ( const f of images ) textures[ f.name ] = await f.arrayBuffer();
	markCurrent( null );
	if ( location.hash ) history.replaceState( null, '', location.pathname + location.search );
	show( { name: dwg.name, data: await dwg.arrayBuffer(), textures } );

}

let gallery = [];

async function fetchBytes( url ) {

	const response = await fetch( url );
	if ( ! response.ok ) throw new Error( `${url}: HTTP ${response.status}` );
	return response.arrayBuffer();

}

async function openSample( item ) {

	markCurrent( item.id );
	setBusy( `Loading ${item.file.split( '/' ).pop()}…` );
	let data;
	try {

		data = await fetchBytes( item.file );

	} catch ( e ) {

		setBusy( null );
		showInfo( { title: item.title, error: `Cannot load the sample: ${e.message}` } );
		return;

	}

	await show( { name: item.file.split( '/' ).pop(), data, textures: null } );

}

function markCurrent( id ) {

	for ( const card of document.querySelectorAll( '.card' ) ) card.setAttribute( 'aria-current', String( card.dataset.id === id ) );

}

async function loadGallery() {

	let list;
	try {

		list = await ( await fetch( 'gallery.json' ) ).json();

	} catch ( e ) {

		$( 'gallery' ).textContent = `The sample list is missing (${e.message}).`;
		return;

	}

	const root = $( 'gallery' );
	root.replaceChildren();
	for ( const group of list.groups ) {

		const section = document.createElement( 'div' );
		section.className = 'group';
		const h = Object.assign( document.createElement( 'h3' ), { textContent: group.title } );
		const grid = document.createElement( 'div' );
		grid.className = 'grid';
		for ( const item of group.items ) {

			gallery.push( item );
			const card = Object.assign( document.createElement( 'button' ), { type: 'button', className: 'card' } );
			card.dataset.id = item.id;
			card.title = `${item.title}: ${item.file.split( '/' ).pop()}, DWG ${VERSIONS[ item.version ] ?? item.version}`;
			if ( item.thumb ) {

				card.append( Object.assign( document.createElement( 'img' ), { src: item.thumb, alt: '', loading: 'lazy', width: 160, height: 160 } ) );

			} else {

				card.append( Object.assign( document.createElement( 'span' ), { className: 'thumb-none', textContent: 'DWG' } ) );

			}

			card.append(
				Object.assign( document.createElement( 'span' ), { className: 'name', textContent: item.title } ),
				Object.assign( document.createElement( 'span' ), { className: 'ver', textContent: `${item.version} · ${VERSIONS[ item.version ] ?? ''}` } ),
			);
			card.addEventListener( 'click', () => {

				history.replaceState( null, '', `#${item.id}` );
				openSample( item );

			} );
			grid.append( card );

		}

		section.append( h, grid );
		root.append( section );

	}

	const wanted = decodeURIComponent( location.hash.slice( 1 ) );
	const first = gallery.find( ( i ) => i.id === wanted ) ?? ( wanted ? null : gallery.find( ( i ) => i.id === list.start ) );
	if ( first ) openSample( first );

}

// -- UI -----------------------------------------------------------------

$( 'open' ).addEventListener( 'click', () => $( 'files' ).click() );
$( 'files' ).addEventListener( 'change', ( e ) => {

	openFiles( [ ...e.target.files ] );
	e.target.value = '';

} );

let dragDepth = 0;
document.addEventListener( 'dragenter', ( e ) => {

	if ( ! e.dataTransfer?.types.includes( 'Files' ) ) return;
	dragDepth ++;
	$( 'dropOverlay' ).classList.remove( 'hidden' );

} );
document.addEventListener( 'dragleave', () => {

	if ( -- dragDepth <= 0 ) {

		dragDepth = 0;
		$( 'dropOverlay' ).classList.add( 'hidden' );

	}

} );
document.addEventListener( 'dragover', ( e ) => e.preventDefault() );
document.addEventListener( 'drop', ( e ) => {

	e.preventDefault();
	dragDepth = 0;
	$( 'dropOverlay' ).classList.add( 'hidden' );
	if ( e.dataTransfer?.files.length ) openFiles( [ ...e.dataTransfer.files ] );

} );

$( 'fit' ).addEventListener( 'click', frameCurrent );
$( 'wire' ).addEventListener( 'click', () => {

	wireframe = ! wireframe;
	$( 'wire' ).setAttribute( 'aria-pressed', String( wireframe ) );
	if ( current?.object ) setWireframe( current.object, wireframe );

} );
$( 'lines' ).addEventListener( 'click', () => {

	lines = ! lines;
	$( 'lines' ).setAttribute( 'aria-pressed', String( lines ) );
	if ( current ) show( current );

} );
$( 'download' ).addEventListener( 'click', download );
document.addEventListener( 'keydown', ( e ) => {

	if ( e.target.closest?.( 'input, textarea, select' ) || e.ctrlKey || e.metaKey || e.altKey ) return;
	if ( e.key === 'f' ) frameCurrent();
	if ( e.key === 'w' ) $( 'wire' ).click();

} );
window.addEventListener( 'hashchange', () => {

	const item = gallery.find( ( i ) => i.id === decodeURIComponent( location.hash.slice( 1 ) ) );
	if ( item ) openSample( item );

} );

loadGallery();

// -- thumbnails (used when the site is built) ----------------------------

let thumbRenderer = null;

/**
 * Convert the DWG at `url` and render it into a square image with a
 * transparent background: a data: URL (WebP). Used by the site build in
 * headless Chrome; harmless on the page.
 */
async function thumbnail( url, size = 320 ) {

	const data = await fetchBytes( url );
	const t0 = performance.now();
	const result = await loader.convert( data, { name: url.split( '/' ).pop(), lines: true, textures: false } );
	const convertMs = performance.now() - t0;
	const object = await buildObject( result, url.split( '/' ).pop() );
	if ( thumbRenderer === null ) {

		// one offscreen renderer for all thumbnails (browsers cap the WebGL contexts)
		const r = new THREE.WebGLRenderer( { antialias: true, alpha: true, preserveDrawingBuffer: true } );
		r.setPixelRatio( 1 );
		r.setClearColor( 0x000000, 0 );
		const s = new THREE.Scene();
		light( r, s );
		thumbRenderer = { renderer: r, scene: s };

	}

	const { renderer: r, scene: s } = thumbRenderer;
	r.setSize( size, size, false );
	s.add( object );
	const cam = new THREE.PerspectiveCamera( 30, 1, 0.01, 1000 );
	frame( object, cam, null );
	// tighter than the viewer's framing
	cam.position.lerp( new THREE.Box3().setFromObject( object ).getCenter( new THREE.Vector3() ), 0.2 );
	cam.updateProjectionMatrix();
	r.render( s, cam );
	const image = r.domElement.toDataURL( 'image/webp', 0.86 );
	const { triangles } = count( object );
	s.remove( object );
	dispose( object );
	return { image, triangles, convertMs, warnings: result.report.stats?.warnings?.length ?? 0 };

}

// for the site build and automated checks
window.demo = { loader, scene, thumbnail, get current() {

	return current;

} };
