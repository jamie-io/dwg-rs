// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

// The demo page: a three.js viewer for the DWGLoader (dwg2gltf compiled to
// WebAssembly, run in a worker), the sample list (gallery.json, written by
// build.mjs) and a panel with the converter's report. For the samples of the
// test suite it measures the mesh like tests/bricscad/measure.py does and
// compares it with the right answer.

import * as THREE from 'three';
import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
import { RoomEnvironment } from 'three/addons/environments/RoomEnvironment.js';
import { CSS2DRenderer, CSS2DObject } from 'three/addons/renderers/CSS2DRenderer.js';

import { DWGLoader } from './loader/src/DWGLoader.js';

const $ = ( id ) => document.getElementById( id );

const VERSIONS = {
	AC1012: 'R13', AC1014: 'R14', AC1015: '2000', AC1018: '2004',
	AC1021: '2007', AC1024: '2010', AC1027: '2013', AC1032: '2018',
};

// LibreDWG's DWG_ERR_* bits below the critical ones (include/dwg.h)
const DECODE_ERRORS = [
	[ 1, 'WRONGCRC', 'a checksum did not match' ],
	[ 2, 'NOTYETSUPPORTED', 'data LibreDWG cannot decode yet' ],
	[ 4, 'UNHANDLEDCLASS', 'an object class LibreDWG does not handle' ],
	[ 8, 'INVALIDTYPE', 'an object of unknown type' ],
	[ 16, 'INVALIDHANDLE', 'a reference to a missing object' ],
	[ 32, 'INVALIDEED', 'invalid extended entity data' ],
	[ 64, 'VALUEOUTOFBOUNDS', 'a value out of its range' ],
];

// the tolerances of tests/bricscad/measure.py, in millimeters
const BOUNDS_MM = 0.2;
const BOUNDS_OF_DIAGONAL = 1e-4;
const SHARE = 0.01;
const DEVIATION_MM = 0.1;
const FINE_MM = 10 * DEVIATION_MM;
const FACING_SHARE = 0.999;

// edges are left out above this, they take long and hide the shape
const EDGE_LIMIT = 400000;

/** A DOM element: h( 'td', { className: 'n' }, 'text', child, ... ). */
function h( tag, props = {}, ...children ) {

	const el = document.createElement( tag );
	for ( const [ key, value ] of Object.entries( props ) ) {

		if ( key === 'dataset' ) Object.assign( el.dataset, value );
		else if ( key.startsWith( 'aria-' ) || key === 'role' ) el.setAttribute( key, value );
		else el[ key ] = value;

	}

	el.append( ...children.flat().filter( ( c ) => c !== null && c !== undefined && c !== false ) );
	return el;

}

// -- numbers ------------------------------------------------------------

const NNBSP = '\u202f';

function group( digits ) {

	return digits.replace( /\B(?=(\d{3})+(?!\d))/g, NNBSP );

}

/** `value` with `decimals`, digits grouped in threes, a real minus sign. */
function num( value, decimals = 0 ) {

	const [ int, frac ] = Math.abs( value ).toFixed( decimals ).split( '.' );
	return ( value < 0 && Number( Math.abs( value ).toFixed( decimals ) ) !== 0 ? '\u2212' : '' ) + group( int ) + ( frac ? '.' + frac : '' );

}

/** Decimals that show about four significant digits of `mm`, at most 3. */
function decimalsFor( mm ) {

	const a = Math.abs( mm );
	return a >= 1000 ? 0 : a >= 100 ? 1 : a >= 10 ? 2 : 3;

}

/** A length given in millimeters: mm up to 100 m, meters beyond. */
function length( mm, { unit = true, decimals } = {} ) {

	if ( Math.abs( mm ) >= 100000 ) return num( mm / 1000, decimals ?? 1 ) + ( unit ? ' m' : '' );
	return num( mm, decimals ?? Math.min( 1, decimalsFor( mm ) ) ) + ( unit ? ' mm' : '' );

}

/** The same for the dimensions drawn on the model (osifont has no thin spaces). */
function dimText( mm ) {

	const whole = Math.abs( mm ) >= 10 && Math.abs( mm - Math.round( mm ) ) < 0.05;
	return length( mm, whole && Math.abs( mm ) < 100000 ? { decimals: 0 } : {} ).replaceAll( NNBSP, ' ' ).replace( '\u2212', '-' );

}

/** Lengths in millimeters in one unit, joined with `sep`: mm, or m when the largest is 100 m or more. */
function lengths( values, sep = ' × ' ) {

	const largest = Math.max( ...values.map( Math.abs ) );
	const meters = largest >= 100000;
	const decimals = Math.min( 2, decimalsFor( meters ? largest / 1000 : largest ) );
	const text = values.map( ( v ) => num( meters ? v / 1000 : v, decimals ) ).join( sep );
	return `${text} ${meters ? 'm' : 'mm'}`;

}

function percent( share, decimals = 2 ) {

	return ( share > 0 ? '+' : '' ) + num( share * 100, decimals ) + ' %';

}

function bytes( n ) {

	return n >= 1048576 ? `${num( n / 1048576, 1 )} MB` : `${num( Math.max( 1, Math.round( n / 1024 ) ) )} KB`;

}

/** "a", "a and b", "a, b and c". */
function list( items ) {

	return items.length < 2 ? items.join( '' ) : `${items.slice( 0, - 1 ).join( ', ' )} and ${items.at( - 1 )}`;

}

function plural( n, one, many = one + 's' ) {

	return `${num( n )} ${n === 1 ? one : many}`;

}

// -- viewer -------------------------------------------------------------

/**
 * Tone mapping, a soft environment and a key light: drawings are often plain
 * white (color 7), which would render as a flat silhouette otherwise.
 */
function light( r, s ) {

	r.toneMapping = THREE.AgXToneMapping;
	r.toneMappingExposure = 1.0;
	s.environment = new THREE.PMREMGenerator( r ).fromScene( new RoomEnvironment(), 0.04 ).texture;
	s.environmentIntensity = 0.6;
	const key = new THREE.DirectionalLight( 0xffffff, 1.7 );
	key.position.set( - 0.6, 1, 0.7 );
	s.add( key );

}

const css = ( name ) => getComputedStyle( document.documentElement ).getPropertyValue( name ).trim();
const MODEL = new THREE.Color( css( '--model' ) || '#e2e6e9' );
matchMedia( '(prefers-color-scheme: dark)' ).addEventListener( 'change', () => MODEL.set( css( '--model' ) || '#e2e6e9' ) );
const DIM = new THREE.Color( css( '--dim' ) || '#0a7489' );

const canvas = $( 'view' );
const renderer = new THREE.WebGLRenderer( { canvas, antialias: true } );
renderer.setPixelRatio( Math.min( window.devicePixelRatio, 2 ) );
renderer.autoClear = false;
const scene = new THREE.Scene();
scene.background = MODEL;
light( renderer, scene );
const camera = new THREE.PerspectiveCamera( 35, 1, 0.01, 1000 );
camera.position.set( 1, 1, 1 );
const controls = new OrbitControls( camera, canvas );
controls.enableDamping = true;

const labels = new CSS2DRenderer( { element: h( 'div' ) } );
$( 'labels' ).append( labels.domElement );

let grid = null;
const dims = new THREE.Group();
dims.name = 'dimensions';
scene.add( dims );

// the UCS icon: DWG's axes (X, Y, Z up) in the corner, turning with the view
const triad = { scene: new THREE.Scene(), camera: new THREE.OrthographicCamera( - 1.6, 1.6, 1.6, - 1.6, 0.1, 10 ), size: 84 };

function axisLabel( text, color ) {

	const c = document.createElement( 'canvas' );
	c.width = c.height = 64;
	const g = c.getContext( '2d' );
	g.fillStyle = color;
	g.font = '44px osifont, system-ui, sans-serif';
	g.textAlign = 'center';
	g.textBaseline = 'middle';
	g.fillText( text, 32, 34 );
	const sprite = new THREE.Sprite( new THREE.SpriteMaterial( { map: new THREE.CanvasTexture( c ), depthTest: false } ) );
	sprite.scale.setScalar( 0.75 );
	return sprite;

}

function buildTriad() {

	triad.scene.clear();
	// glTF is Y-up: DWG X is +x, DWG Y is -z, DWG Z is +y
	for ( const [ text, dir, color ] of [
		[ 'X', new THREE.Vector3( 1, 0, 0 ), '#c0362c' ],
		[ 'Y', new THREE.Vector3( 0, 0, - 1 ), '#2b8a3e' ],
		[ 'Z', new THREE.Vector3( 0, 1, 0 ), '#1c62b9' ],
	] ) {

		const line = new THREE.Line(
			new THREE.BufferGeometry().setFromPoints( [ new THREE.Vector3(), dir ] ),
			new THREE.LineBasicMaterial( { color } ),
		);
		triad.scene.add( line );
		const label = axisLabel( text, color );
		label.position.copy( dir ).multiplyScalar( 1.32 );
		triad.scene.add( label );

	}

}

buildTriad();
document.fonts?.load( '44px osifont' ).then( buildTriad, () => {} );

function resize() {

	const { clientWidth: w, clientHeight: h } = canvas;
	if ( w === 0 || h === 0 ) return;
	renderer.setSize( w, h, false );
	labels.setSize( w, h );
	camera.aspect = w / h;
	camera.updateProjectionMatrix();

}

new ResizeObserver( resize ).observe( canvas );
resize();

const viewDirection = new THREE.Vector3();
renderer.setAnimationLoop( () => {

	controls.update();
	const { width, height } = renderer.domElement;
	const ratio = renderer.getPixelRatio();
	renderer.setViewport( 0, 0, width / ratio, height / ratio );
	renderer.clear();
	renderer.render( scene, camera );
	labels.render( scene, camera );
	// the axes, bottom left, on top
	viewDirection.subVectors( camera.position, controls.target ).normalize();
	triad.camera.position.copy( viewDirection ).multiplyScalar( 4 );
	triad.camera.up.copy( camera.up );
	triad.camera.lookAt( 0, 0, 0 );
	renderer.clearDepth();
	renderer.setViewport( 8, 8, triad.size, triad.size );
	renderer.render( triad.scene, triad.camera );

} );

const VIEWS = {
	// AutoCAD's south-east isometric: from +X, -Y, +Z of the drawing
	iso: new THREE.Vector3( 1, 1, 1 ),
	top: new THREE.Vector3( 0, 1, 1e-4 ),
	front: new THREE.Vector3( 0, 0, 1 ),
	right: new THREE.Vector3( 1, 0, 0 ),
};

let view = 'iso';

/** Point `cam` at `object` from `direction`; returns the bounding box. */
function frame( object, cam, ctrl, direction = VIEWS.iso ) {

	const box = new THREE.Box3().setFromObject( object );
	if ( box.isEmpty() ) return box;
	const size = box.getSize( new THREE.Vector3() );
	const center = box.getCenter( new THREE.Vector3() );
	const radius = Math.max( size.length() / 2, 1e-6 );
	const fov = THREE.MathUtils.degToRad( cam.fov / 2 );
	const distance = 1.15 * radius / Math.sin( Math.min( fov, Math.atan( Math.tan( fov ) * cam.aspect ) ) );
	cam.position.copy( center ).add( direction.clone().normalize().multiplyScalar( distance ) );
	cam.near = distance / 100;
	cam.far = distance * 100;
	cam.lookAt( center );
	cam.updateProjectionMatrix();
	if ( ctrl ) ctrl.target.copy( center );
	return box;

}

function frameCurrent() {

	if ( ! current?.object ) return;
	frame( current.object, camera, controls, VIEWS[ view ] );
	for ( const b of document.querySelectorAll( '[data-view]' ) ) b.setAttribute( 'aria-pressed', String( b.dataset.view === view ) );

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

/** A nice grid step (1, 2 or 5 times a power of ten) near `size / 6`. */
function gridStep( size ) {

	const raw = size / 6;
	const power = 10 ** Math.floor( Math.log10( raw ) );
	return [ 1, 2, 5, 10 ].map( ( k ) => k * power ).find( ( s ) => s >= raw * 0.7 ) ?? 10 * power;

}

function placeGrid( box ) {

	if ( grid ) {

		scene.remove( grid );
		grid.geometry.dispose();
		grid.material.dispose();
		grid = null;

	}

	if ( box.isEmpty() ) {

		$( 'gridLabel' ).textContent = '';
		return;

	}

	const size = box.getSize( new THREE.Vector3() );
	const step = gridStep( Math.max( size.x, size.z, size.y * 0.5, 1e-9 ) );
	const cells = 2 * Math.ceil( Math.max( size.x, size.z ) / step / 2 + 3 );
	grid = new THREE.GridHelper( cells * step, cells, 0x9aa5ae, 0xc4cbd0 );
	grid.position.set( ( box.min.x + box.max.x ) / 2, box.min.y, ( box.min.z + box.max.z ) / 2 );
	grid.renderOrder = - 1;
	scene.add( grid );
	const stepMm = step * 1000;
	$( 'gridLabel' ).textContent = `Grid ${length( stepMm, { decimals: stepMm >= 1 ? 0 : 3 } )}`;

}

// -- edges, triangles, dimensions --------------------------------------

const edgeCache = new WeakMap();
const darkEdges = new THREE.LineBasicMaterial( { color: 0x1d2226, transparent: true, opacity: 0.55 } );
const lightEdges = new THREE.LineBasicMaterial( { color: 0xb7c0c7, transparent: true, opacity: 0.8 } );

/** Edges that show on `material`: light ones on dark, untextured surfaces. */
function edgeMaterialFor( material ) {

	const m = [ material ].flat()[ 0 ];
	if ( ! m?.color || m.map ) return darkEdges;
	const { r, g, b } = m.color;
	return 0.2126 * r + 0.7152 * g + 0.0722 * b < 0.12 ? lightEdges : darkEdges;

}

function setEdges( object, on ) {

	object.traverse( ( o ) => {

		if ( ! o.isMesh ) return;
		let edges = o.children.find( ( c ) => c.userData.isEdges );
		if ( on && ! edges ) {

			let geometry = edgeCache.get( o.geometry );
			if ( ! geometry ) {

				// 40°: box and hole edges, not the steps of a coarsely tessellated blend
				geometry = new THREE.EdgesGeometry( o.geometry, 40 );
				edgeCache.set( o.geometry, geometry );

			}

			edges = new THREE.LineSegments( geometry, edgeMaterialFor( o.material ) );
			edges.userData.isEdges = true;
			edges.raycast = () => {};
			o.add( edges );

		}

		if ( edges ) edges.visible = on;
		for ( const m of [ o.material ].flat() ) {

			m.polygonOffset = on;
			m.polygonOffsetFactor = 1;
			m.polygonOffsetUnits = 1;

		}

	} );

}

function setWireframe( object, on ) {

	object.traverse( ( o ) => {

		if ( ! o.isMesh ) return;
		for ( const m of [ o.material ].flat() ) m.wireframe = on;

	} );

}

const dimMaterial = new THREE.LineBasicMaterial( { color: DIM, depthTest: false, transparent: true } );

/**
 * The overall dimensions of `box` (world, Y-up, meters) drawn like a CAD
 * dimension: extension lines, a dimension line with oblique ticks, the text.
 * X along the front, the drawing's Y along the right side, Z up the front left
 * edge, offset outwards. `sizes` are the lengths in millimeters, by DWG axis.
 */
function drawDimensions( box, sizes ) {

	for ( const child of [ ...dims.children ] ) {

		dims.remove( child );
		child.geometry?.dispose();
		if ( child.isCSS2DObject ) child.element.remove();

	}

	if ( box.isEmpty() ) return;
	const size = box.getSize( new THREE.Vector3() );
	const big = Math.max( size.x, size.y, size.z );
	const gap = big * 0.04;
	const off = big * 0.14;
	const tick = big * 0.035;
	const points = [];
	const V = ( x, y, z ) => new THREE.Vector3( x, y, z );
	const segment = ( a, b ) => points.push( a, b );

	/** One dimension from `a` to `b` (on the box), pushed out along `out`; ticks lean towards `lean`. */
	const draw = ( a, b, out, lean, text ) => {

		const a2 = a.clone().addScaledVector( out, off );
		const b2 = b.clone().addScaledVector( out, off );
		segment( a.clone().addScaledVector( out, gap ), a2.clone().addScaledVector( out, gap * 0.8 ) );
		segment( b.clone().addScaledVector( out, gap ), b2.clone().addScaledVector( out, gap * 0.8 ) );
		segment( a2, b2 );
		const along = b2.clone().sub( a2 ).normalize();
		const slant = along.clone().add( lean ).normalize().multiplyScalar( tick / 2 );
		for ( const p of [ a2, b2 ] ) segment( p.clone().sub( slant ), p.clone().add( slant ) );
		const label = new CSS2DObject( h( 'div', { className: 'dimtext' }, text ) );
		label.position.copy( a2 ).add( b2 ).multiplyScalar( 0.5 ).addScaledVector( out, gap * 0.9 );
		label.center.set( 0.5, 0.5 );
		dims.add( label );

	};

	const { min: lo, max: hi } = box;
	const flat = 1e-9 * Math.max( big, 1e-9 );
	if ( size.x > flat ) draw( V( lo.x, lo.y, hi.z ), V( hi.x, lo.y, hi.z ), V( 0, 0, 1 ), V( 0, 0, 1 ), dimText( sizes[ 0 ] ) );
	if ( size.z > flat ) draw( V( hi.x, lo.y, hi.z ), V( hi.x, lo.y, lo.z ), V( 1, 0, 0 ), V( 1, 0, 0 ), dimText( sizes[ 1 ] ) );
	if ( size.y > flat ) draw( V( lo.x, lo.y, hi.z ), V( lo.x, hi.y, hi.z ), V( - 1, 0, 0 ), V( - 1, 0, 0 ), dimText( sizes[ 2 ] ) );
	const lines = new THREE.LineSegments( new THREE.BufferGeometry().setFromPoints( points ), dimMaterial );
	lines.renderOrder = 10;
	dims.add( lines );

}

// -- measuring ------------------------------------------------------------

/**
 * Area, signed volume, bounds and facing of the triangles of `object`, in
 * world space (meters, Y-up), like tests/bricscad/glb.py: mirrored
 * instances are rewound. `axis` (world) collects the area facing it.
 */
function measure( object, axis = null ) {

	object.updateWorldMatrix( true, true );
	const a = new THREE.Vector3(), b = new THREE.Vector3(), c = new THREE.Vector3();
	const ab = new THREE.Vector3(), ac = new THREE.Vector3(), n = new THREE.Vector3(), bc = new THREE.Vector3();
	const box = new THREE.Box3();
	let area = 0, volume = 0, facing = 0, triangles = 0, segments = 0;
	object.traverse( ( o ) => {

		const g = o.geometry;
		if ( ! g || o.userData.isEdges ) return;
		const position = g.attributes.position;
		const index = g.index;
		const count = index ? index.count : position.count;
		if ( o.isLineSegments ) segments += count / 2;
		else if ( o.isLine ) segments += Math.max( count - 1, 0 );
		if ( ! o.isMesh ) return;
		const m = o.matrixWorld;
		const mirrored = m.determinant() < 0;
		for ( let i = 0; i + 2 < count; i += 3 ) {

			const i0 = index ? index.getX( i ) : i;
			let i1 = index ? index.getX( i + 1 ) : i + 1;
			let i2 = index ? index.getX( i + 2 ) : i + 2;
			if ( mirrored ) [ i1, i2 ] = [ i2, i1 ];
			a.fromBufferAttribute( position, i0 ).applyMatrix4( m );
			b.fromBufferAttribute( position, i1 ).applyMatrix4( m );
			c.fromBufferAttribute( position, i2 ).applyMatrix4( m );
			box.expandByPoint( a ).expandByPoint( b ).expandByPoint( c );
			n.crossVectors( ab.subVectors( b, a ), ac.subVectors( c, a ) );
			const twice = n.length();
			area += twice / 2;
			volume += a.dot( bc.crossVectors( b, c ) ) / 6;
			if ( axis && n.dot( axis ) > 0 ) facing += twice / 2;
			triangles ++;

		}

	} );
	return { area, volume, box, facing, triangles, segments };

}

/** World (Y-up, meters) bounds as DWG bounds (Z-up) in millimeters. */
function dwgBounds( box ) {

	return [
		[ box.min.x * 1000, - box.max.z * 1000, box.min.y * 1000 ],
		[ box.max.x * 1000, - box.min.z * 1000, box.max.y * 1000 ],
	];

}

function extents( bounds ) {

	return [ 0, 1, 2 ].map( ( k ) => bounds[ 1 ][ k ] - bounds[ 0 ][ k ] );

}

/**
 * The checks of tests/bricscad/measure.py for one conversion: rows for the
 * panel and whether everything is within its tolerance.
 */
function checkAgainst( expected, m, report ) {

	const bounds = dwgBounds( m.box );
	const areaMm = m.area * 1e6;
	const volumeMm = m.volume * 1e9;
	const sizes = extents( bounds );
	const thinnest = Math.min( ...sizes );
	const fine = thinnest > 0 && thinnest < FINE_MM;
	const rows = [];
	let ok = true;
	const skipped = Object.entries( report.stats?.skipped ?? {} );
	if ( skipped.length ) ok = false;
	if ( expected.bounds ) {

		const e = expected.bounds;
		const diagonal = Math.hypot( ...extents( e ) );
		const allowed = BOUNDS_MM + BOUNDS_OF_DIAGONAL * diagonal;
		const off = Math.max( ...[ 0, 1 ].flatMap( ( i ) => [ 0, 1, 2 ].map( ( k ) => Math.abs( bounds[ i ][ k ] - e[ i ][ k ] ) ) ) );
		const pass = off <= allowed;
		ok &&= pass;
		rows.push( {
			what: 'Extents', expected: lengths( extents( e ) ), measured: lengths( sizes ),
			off: `${length( off, { decimals: off < 1 ? 3 : 2 } )} at most`, allowed: length( allowed, { decimals: 2 } ), pass,
		} );

	}

	if ( expected.area ) {

		const share = ( areaMm - expected.area ) / expected.area;
		const pass = fine || Math.abs( share ) <= SHARE;
		ok &&= pass;
		rows.push( {
			what: 'Area', expected: `${num( expected.area, decimalsFor( expected.area ) )} mm²`,
			measured: `${num( areaMm, decimalsFor( areaMm ) )} mm²`, off: percent( share ), allowed: fine ? 'not checked' : '1 %', pass, skippedCheck: fine,
		} );

	}

	if ( expected.volume && expected.closed !== false ) {

		const surface = expected.area || areaMm;
		const allowance = SHARE * Math.abs( expected.volume ) + DEVIATION_MM * surface;
		const share = ( volumeMm - expected.volume ) / expected.volume;
		const pass = fine || Math.abs( volumeMm - expected.volume ) <= allowance;
		ok &&= pass;
		rows.push( {
			what: 'Volume', expected: `${num( expected.volume, decimalsFor( expected.volume ) )} mm³`,
			measured: `${num( volumeMm, decimalsFor( volumeMm ) )} mm³`, off: percent( share ),
			allowed: fine ? 'not checked' : percent( allowance / Math.abs( expected.volume ) ).replace( '+', '' ), pass, skippedCheck: fine,
		} );

	}

	if ( expected.facing === 'outward' ) {

		const pass = m.volume > 0;
		ok &&= pass;
		rows.push( { what: 'Faces', expected: 'outwards', measured: pass ? 'outwards' : 'inwards', off: '', allowed: '', pass } );

	} else if ( Array.isArray( expected.facing ) ) {

		const share = m.area > 0 ? m.facing / m.area : 0;
		const pass = share >= FACING_SHARE;
		ok &&= pass;
		const [ x, y, z ] = expected.facing;
		const name = z > 0 ? '+Z' : z < 0 ? '−Z' : y > 0 ? '+Y' : y < 0 ? '−Y' : x > 0 ? '+X' : '−X';
		rows.push( { what: 'Faces', expected: `all towards ${name}`, measured: `${num( share * 100, 1 )} % of the area`, off: '', allowed: '99.9 %', pass } );

	}

	return { rows, ok, fine, skipped };

}

/** The facing axis of `expected` in world space (DWG Z-up to glTF Y-up). */
function worldAxis( expected ) {

	if ( ! Array.isArray( expected?.facing ) ) return null;
	const [ x, y, z ] = expected.facing;
	return new THREE.Vector3( x, z, - y );

}

// -- converting ---------------------------------------------------------

const loader = new DWGLoader().preload();

/** What is shown: { name, data, textures, item, version, units, object, result, report, measured } */
let current = null;
let site = { facts: {}, groups: [] };
let gallery = [];
let wireframe = false;
let edges = true;
let showDims = true;
let lines = true;
let busy = 0;
let converted = 0;

function setStatus( text, kind = 'working' ) {

	const el = $( 'busy' );
	el.textContent = text ?? '';
	el.className = `status ${kind}`;
	el.classList.toggle( 'hidden', ! text );

}

/** The DWG version from the file header ("AC1032"), or null. */
function versionOf( data ) {

	const magic = new TextDecoder().decode( new Uint8Array( data, 0, Math.min( 6, data.byteLength ) ) );
	return /^AC\d{4}$/.test( magic ) ? magic : null;

}

/** Why `data` is not a DWG, in a sentence; null when it looks like one. */
function notDwg( data ) {

	if ( versionOf( data ) ) return null;
	const head = new TextDecoder( 'latin1' ).decode( new Uint8Array( data, 0, Math.min( 32, data.byteLength ) ) );
	if ( /^AutoCAD Binary DXF/.test( head ) || /^\s*0\s*\r?\n\s*SECTION/.test( head ) ) return 'This is a DXF file. dwg-rs reads DWG only: save the drawing as DWG.';
	if ( /^PK/.test( head ) ) return 'This is a zip archive, not a DWG. Unpack it and open the DWG inside.';
	const shown = head.slice( 0, 6 ).replace( /[^\x20-\x7e]/g, '.' );
	return `This is not a DWG file: DWG files start with “AC1…”, this one with “${shown}”.`;

}

function versionName( version ) {

	if ( ! version ) return 'unknown';
	return VERSIONS[ version ] ? `${version}, AutoCAD ${VERSIONS[ version ]} format` : version;

}

async function show( entry ) {

	const ticket = ++ busy;
	const { name, data, textures } = entry;
	const why = notDwg( data );
	if ( why ) {

		setStatus( why, 'error' );
		renderError( entry, why, '', false );
		return;

	}

	setStatus( converted === 0 ? `Loading the converter (${site.facts.wasmGzip ?? 'a few MB'}, once) and converting ${name}…` : `Converting ${name}…` );
	$( 'empty' ).classList.add( 'hidden' );
	for ( const id of [ 'download', 'copy' ] ) if ( $( id ) ) $( id ).disabled = true;
	const t0 = performance.now();
	let object, result;
	try {

		result = await loader.convert( data, { name, lines, units: entry.units || null, textures: textures ?? false } );
		object = await buildObject( result, name );

	} catch ( e ) {

		if ( ticket !== busy ) return;
		console.warn( e );
		const message = e.code ? `Conversion failed (${e.code}): ${e.message}` : `Conversion failed: ${e.message ?? e}`;
		setStatus( message, 'error' );
		renderError( entry, message, e.report?.summary ?? e.stderr ?? '' );
		return;

	}

	if ( ticket !== busy ) {

		dispose( object );
		return;

	}

	converted ++;
	if ( current?.object ) {

		scene.remove( current.object );
		dispose( current.object );

	}

	const expected = entry.item?.versions?.find( ( v ) => v.version === entry.version )?.expected ?? null;
	const measured = measure( object, worldAxis( expected ) );
	// center the drawing: coordinates far from the origin lose precision on the GPU
	const center = measured.box.getCenter( new THREE.Vector3() );
	object.position.sub( center );
	const shown = measured.box.clone().translate( center.clone().negate() );

	current = { ...entry, object, result, report: result.report, measured, expected, totalMs: performance.now() - t0 };
	setWireframe( object, wireframe );
	if ( edges && measured.triangles <= EDGE_LIMIT ) setEdges( object, true );
	scene.add( object );
	placeGrid( shown );
	drawDimensions( shown, extents( dwgBounds( measured.box ) ) );
	dims.visible = showDims;
	frameCurrent();
	setStatus( null );
	renderReport();

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

		const s = ( await gltfLoader.parseAsync( glb.slice( 0 ), '' ) ).scene;
		s.name = block;
		group.add( s );

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

// -- the panel ------------------------------------------------------------

function field( label, value, wide = false ) {

	return h( 'div', { className: wide ? 'wide' : '' }, h( 'dt', {}, label ), h( 'dd', {}, value ) );

}

function titleBlock( entry, extra = [] ) {

	const version = versionOf( entry.data );
	const fields = [
		h( 'div', { className: 'wide' },
			h( 'dt', {}, entry.item ? entry.item.title : 'Your drawing' ),
			h( 'dd', { className: 'drawing' }, entry.name ),
		),
		field( 'DWG format', versionName( version ), true ),
	];
	if ( entry.item?.versions?.length > 1 ) {

		const row = h( 'div', { className: 'versions', role: 'group', 'aria-label': 'Saved by BricsCAD as' } );
		for ( const v of entry.item.versions ) {

			const b = h( 'button', { type: 'button', textContent: VERSIONS[ v.version ] ?? v.version, title: `${v.version}: AutoCAD ${VERSIONS[ v.version ] ?? '?'} format` } );
			b.setAttribute( 'aria-pressed', String( v.version === entry.version ) );
			b.addEventListener( 'click', () => openSample( entry.item, v.version ) );
			row.append( b );

		}

		fields.push( h( 'div', { className: 'wide' }, h( 'dt', {}, 'The same shape, saved by BricsCAD as' ), row ) );

	}

	fields.push( field( 'File size', bytes( entry.data.byteLength ) ) );
	fields.push( field( 'Units', entry.units ? `${entry.units} (set here)` : 'the drawing’s own' ) );
	return h( 'dl', { className: 'titleblock' }, fields, extra );

}

function renderError( entry, message, summary = '', reportable = true ) {

	if ( current?.object ) {

		scene.remove( current.object );
		dispose( current.object );
		drawDimensions( new THREE.Box3(), [] );
		placeGrid( new THREE.Box3() );

	}

	current = { ...entry, object: null, result: null };
	$( 'empty' ).classList.remove( 'hidden' );
	$( 'empty' ).textContent = 'Nothing to show for this file.';
	const panel = $( 'inspector' );
	panel.replaceChildren( ...[
		titleBlock( entry ),
		h( 'p', { className: 'verdict bad' }, message ),
		summary ? h( 'h3', {}, 'Converter output' ) : null,
		summary ? h( 'pre', { className: 'summary' }, summary ) : null,
		reportable ? h( 'p', { className: 'muted' }, 'If your CAD program opens this file, please ',
			h( 'a', { href: issueUrl( entry, message + ( summary ? '\n' + summary : '' ) ), target: '_blank', rel: 'noopener' }, 'report it' ),
			'. The form opens on GitHub with this message filled in; nothing is sent until you submit it.' ) : null,
	].filter( Boolean ) );

}

function renderReport() {

	const { report, result, measured } = current;
	const stats = report.stats ?? {};
	const bounds = dwgBounds( measured.box );
	const sizes = extents( bounds );
	const glbBytes = result.blocks.reduce( ( s, b ) => s + b.glb.byteLength, 0 );

	const block = titleBlock( current, [
		field( 'Extents X × Y × Z', measured.box.isEmpty() ? '—' : lengths( sizes ), true ),
		field( 'Lower corner', measured.box.isEmpty() ? '—' : lengths( bounds[ 0 ], ', ' ), true ),
		field( 'Triangles', num( measured.triangles ) ),
		field( 'Line segments', num( measured.segments ) ),
		field( 'Block instances', num( stats.instances ?? 0 ) ),
		field( 'GLB size', bytes( glbBytes ) ),
		field( 'Converted in', `${num( result.convertMs ?? 0 )} ms` ),
		field( 'Shown after', `${num( current.totalMs )} ms` ),
	] );

	const panel = $( 'inspector' );
	panel.replaceChildren( block );
	if ( current.unitsNote ) panel.append( h( 'p', { className: 'verdict note' }, current.unitsNote ) );

	// what the converter did, entity by entity
	const rows = [];
	for ( const [ type, n ] of Object.entries( stats.converted ?? {} ) ) rows.push( [ type, 'converted', n ] );
	for ( const [ type, n ] of Object.entries( stats.skipped ?? {} ) ) rows.push( [ type, 'skipped', n ] );
	for ( const [ what, n ] of Object.entries( stats.repaired ?? {} ) ) rows.push( [ what, 'repaired', n ] );
	panel.append( h( 'h3', {}, 'Entities' ) );
	if ( rows.length === 0 ) {

		panel.append( h( 'p', { className: 'muted' }, 'Nothing in model space was converted.' ) );

	} else {

		panel.append( h( 'table', {},
			h( 'thead', {}, h( 'tr', {}, h( 'th', {}, 'Entity' ), h( 'th', {}, '' ), h( 'th', { className: 'n' }, 'Count' ) ) ),
			h( 'tbody', {}, rows.map( ( [ type, what, n ] ) => h( 'tr', {},
				h( 'td', {}, what === 'repaired' ? type : h( 'code', {}, type ) ),
				h( 'td', { className: what === 'skipped' ? 'fail' : what === 'repaired' ? '' : 'pass' }, what ),
				h( 'td', { className: 'n' }, num( n ) ),
			) ) ),
		) );

	}

	const warnings = ( stats.warnings ?? [] ).map( ( w ) => typeof w === 'string' ? w : JSON.stringify( w ) );
	if ( report.missingTextures?.length ) warnings.push( `texture images not found: ${report.missingTextures.join( ', ' )}. Open them together with the DWG.` );
	if ( warnings.length ) {

		panel.append( h( 'h3', {}, plural( warnings.length, 'warning' ) ) );
		panel.append( h( 'ul', { className: 'warnings' }, warnings.map( ( w ) => h( 'li', {}, w, explain( w ) ) ) ) );

	}

	if ( current.expected ) panel.append( renderCheck() );
	if ( current.item?.source ) panel.append( renderRoundTrip() );

	panel.append( h( 'h3', {}, 'Converter summary' ), h( 'pre', { className: 'summary' }, report.summary ?? '' ) );
	const download = h( 'button', { type: 'button', id: 'download', textContent: result.blocks.length > 1 ? `Download ${result.blocks.length} GLB files` : 'Download GLB' } );
	download.addEventListener( 'click', downloadGlb );
	const copy = h( 'button', { type: 'button', id: 'copy', textContent: 'Copy summary' } );
	copy.addEventListener( 'click', async () => {

		try {

			await navigator.clipboard.writeText( summaryText( current ) );
			copy.textContent = 'Copied';

		} catch {

			copy.textContent = 'Copy failed';

		}

		setTimeout( () => copy.textContent = 'Copy summary', 1600 );

	} );
	panel.append(
		h( 'div', { className: 'buttons' }, download, copy,
			h( 'a', { href: issueUrl( current, summaryText( current ) ), target: '_blank', rel: 'noopener', textContent: 'Report a problem' } ) ),
		h( 'p', { className: 'muted' }, 'Report a problem opens a GitHub issue form with the summary filled in. Nothing is sent until you submit it, and the DWG is not attached.' ),
	);

}

/** A line of explanation under a warning, or null. */
function explain( warning ) {

	const bits = /recoverable decode errors \(0x([0-9a-f]+)\)/i.exec( warning );
	if ( bits ) {

		const value = parseInt( bits[ 1 ], 16 );
		const names = DECODE_ERRORS.filter( ( [ bit ] ) => value & bit ).map( ( [ , name, text ] ) => `${name} (${text})` );
		const facts = site.facts;
		const suite = current?.item?.versions && facts.decodeWarnings
			? ` The test suite has this on ${facts.decodeWarnings} of its ${facts.corpusFiles} files, and their geometry still matches.`
			: ' Check the result for missing parts.';
		return h( 'small', {}, `LibreDWG skipped what it could not read: ${names.join( ', ' ) || 'unknown bits'}.${suite}` );

	}

	if ( /no units \(INSUNITS 0\)/.test( warning ) ) return h( 'small', {}, 'If it comes out 1000 times too large, it was drawn in millimeters: pick mm under Units.' );
	if ( /less than 1 cm across/.test( warning ) ) return h( 'small', {}, 'On this page, the Units menu does what --units does on the command line.' );
	if ( /paper space/.test( warning ) ) return h( 'small', {}, 'Layouts are not converted, only model space.' );
	return null;

}

function renderCheck() {

	const { rows, ok, fine, skipped } = checkAgainst( current.expected, current.measured, current.report );
	const section = h( 'div', {} );
	section.append( h( 'h3', {}, 'Check against the right answer' ) );
	section.append( h( 'p', { className: 'muted' }, 'Measured here, on the triangles you see, with the tolerances of the test suite. The right answers come from a formula where one exists, otherwise from BricsCAD.' ) );
	section.append( h( 'table', {},
		h( 'thead', {}, h( 'tr', {}, h( 'th', {}, '' ), h( 'th', {}, 'Right answer' ), h( 'th', {}, 'Measured' ), h( 'th', { className: 'n' }, 'Off by' ) ) ),
		h( 'tbody', {}, rows.map( ( r ) => h( 'tr', {},
			h( 'td', {}, r.what ),
			h( 'td', {}, r.expected ),
			h( 'td', { className: r.skippedCheck ? '' : r.pass ? 'pass' : 'fail' }, r.measured ),
			h( 'td', { className: 'n', title: r.allowed ? `allowed: ${r.allowed}` : '' }, r.off, r.allowed && r.off ? h( 'br' ) : null, r.allowed && r.off ? h( 'small', { className: 'muted' }, r.allowed === 'not checked' ? 'not checked' : `${r.allowed} allowed` ) : null ),
		) ) ),
	) );
	let text;
	if ( skipped.length ) text = `Entities were skipped: ${skipped.map( ( [ k, n ] ) => `${k} ${n}` ).join( ', ' )}.`;
	else if ( ok && fine ) text = 'Thinner than 1 mm: only the extents count, and they match. At 0.1 mm chord tolerance, area and volume of so thin a part mean little.';
	else if ( ok ) text = 'Matches the right answer within the test suite’s tolerances.';
	else text = 'Does not match the right answer. This is a bug; the report of the test suite should list it too.';
	section.append( h( 'p', { className: ok ? 'verdict' : 'verdict bad' }, text ) );
	return section;

}

function renderRoundTrip() {

	const source = current.item.source;
	const m = current.measured;
	const section = h( 'div', {} );
	const [ slo, shi ] = source.bounds;
	// both are glTF (Y-up); shown in the drawing's X, Y, Z like the title block
	const srcSize = [ 0, 2, 1 ].map( ( k ) => ( shi[ k ] - slo[ k ] ) * 1000 );
	const size = [ 'x', 'z', 'y' ].map( ( k ) => ( m.box.max[ k ] - m.box.min[ k ] ) * 1000 );
	const off = Math.max( ...[ 0, 1, 2 ].flatMap( ( k ) => [ Math.abs( m.box.min.getComponent( k ) - slo[ k ] ), Math.abs( m.box.max.getComponent( k ) - shi[ k ] ) ] ) ) * 1000;
	const share = ( m.area - source.area ) / source.area;
	const kept = new Set();
	current.object.traverse( ( o ) => {

		for ( const mat of [ o.material ].flat().filter( Boolean ) ) {

			if ( mat.map ) kept.add( 'base color' );
			if ( mat.normalMap ) kept.add( 'normal' );
			if ( mat.roughnessMap || mat.metalnessMap ) kept.add( 'metallic-roughness' );
			if ( mat.aoMap ) kept.add( 'occlusion' );
			if ( mat.emissiveMap ) kept.add( 'emissive' );

		}

	} );
	const had = [ ...new Set( source.materials.flatMap( ( mat ) => mat.maps ) ) ];
	const lost = had.filter( ( x ) => ! kept.has( x ) );
	const factors = [];
	current.object.traverse( ( o ) => {

		if ( o.isMesh && o.material && ! factors.length ) factors.push( `metalness ${num( o.material.metalness, 2 )}, roughness ${num( o.material.roughness, 2 )}` );

	} );

	section.append(
		h( 'h3', {}, 'Compared with the source glTF' ),
		h( 'p', { className: 'muted' }, h( 'a', { href: source.source }, `Khronos ${source.title}` ), ` (${source.license}, ${source.author}), images ${source.images}. Written by ${source.writer}.` ),
		h( 'table', {},
			h( 'thead', {}, h( 'tr', {}, h( 'th', {}, '' ), h( 'th', {}, 'Source' ), h( 'th', {}, 'Read back' ), h( 'th', { className: 'n' }, 'Off by' ) ) ),
			h( 'tbody', {},
				h( 'tr', {}, h( 'td', {}, 'Triangles' ), h( 'td', {}, num( source.triangles ) ), h( 'td', {}, num( m.triangles ) ), h( 'td', { className: 'n' }, num( m.triangles - source.triangles ) ) ),
				h( 'tr', {}, h( 'td', {}, 'Area' ), h( 'td', {}, `${num( source.area, 3 )} m²` ), h( 'td', {}, `${num( m.area, 3 )} m²` ), h( 'td', { className: 'n' }, percent( share, 4 ) ) ),
				h( 'tr', {}, h( 'td', {}, 'Extents' ), h( 'td', {}, lengths( srcSize ) ), h( 'td', {}, lengths( size ) ), h( 'td', { className: 'n' }, length( off, { decimals: 3 } ) ) ),
			),
		),
		h( 'p', { className: lost.length ? 'verdict note' : 'verdict' },
			lost.length
				? `Lost on the way: the ${list( lost )} ${lost.length === 1 ? 'map' : 'maps'}. gltf2dwg writes one texture per material, the base color, and the base color, metallic and roughness factors; here ${factors[ 0 ] ?? 'unknown'}.`
				: 'All texture maps of the source came back.' ),
	);
	return section;

}

function summaryText( entry ) {

	const facts = site.facts;
	return [
		`dwg-rs ${facts.commit ?? '?'} (WebAssembly, ${location.origin}${location.pathname})`,
		`File: ${entry.name}, ${versionName( versionOf( entry.data ) )}, ${bytes( entry.data.byteLength )}`,
		`Options: units ${entry.units || 'as drawn'}, lines ${lines ? 'on' : 'off'}`,
		'',
		entry.report?.summary ?? '',
	].join( '\n' );

}

function issueUrl( entry, text ) {

	const body = `${text.slice( 0, 4000 )}\n\nWhat looks wrong:\n\n`;
	return `https://github.com/jamie-io/dwg-rs/issues/new?title=${encodeURIComponent( `Conversion of ${entry.name}` )}&body=${encodeURIComponent( body )}`;

}

function downloadGlb() {

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

// -- files and samples --------------------------------------------------

async function openFiles( files ) {

	const dwg = files.find( ( f ) => /\.dwg$/i.test( f.name ) ) ?? ( files.length === 1 ? files[ 0 ] : null );
	if ( ! dwg ) {

		setStatus( 'Choose a file ending in .dwg; texture images may come along with it.', 'error' );
		return;

	}

	const images = files.filter( ( f ) => f !== dwg && /\.(png|jpe?g)$/i.test( f.name ) );
	const textures = images.length ? {} : null;
	for ( const f of images ) textures[ f.name ] = await f.arrayBuffer();
	markCurrent( null );
	if ( location.hash ) history.replaceState( null, '', location.pathname + location.search );
	show( { name: dwg.name, data: await dwg.arrayBuffer(), textures, units: $( 'units' ).value } );

}

async function fetchBytes( url ) {

	const response = await fetch( url );
	if ( ! response.ok ) throw new Error( `${url}: HTTP ${response.status}` );
	return response.arrayBuffer();

}

let autoUnits = false;

async function openSample( item, version = item.default, { keepHash = false } = {} ) {

	markCurrent( item.id );
	const v = item.versions.find( ( x ) => x.version === version ) ?? item.versions.at( - 1 );
	if ( ! keepHash ) history.replaceState( null, '', `#${item.id}${v.version !== item.default ? '/' + v.version : ''}` );
	// R14 cannot store units: the test suite converts it as millimeters
	let units = $( 'units' ).value;
	let unitsNote = null;
	if ( v.version === 'AC1014' && item.versions.length > 1 && ( units === '' || autoUnits ) ) {

		units = 'mm';
		autoUnits = true;
		unitsNote = 'R14 drawings cannot store their units, so this one would come out 1000 times too large. It is converted as millimeters, as the test suite does.';

	} else if ( autoUnits ) {

		units = '';
		autoUnits = false;

	}

	$( 'units' ).value = units;
	const name = v.file.split( '/' ).pop();
	setStatus( `Loading ${name}…` );
	let data;
	try {

		data = await fetchBytes( v.file );

	} catch ( e ) {

		setStatus( `Cannot load the sample: ${e.message}`, 'error' );
		return;

	}

	await show( { name, data, textures: null, item, version: v.version, units, unitsNote } );

}

function markCurrent( id ) {

	for ( const row of document.querySelectorAll( '.sample' ) ) row.setAttribute( 'aria-current', String( row.dataset.id === id ) );

}

async function loadGallery() {

	try {

		site = await ( await fetch( 'gallery.json' ) ).json();

	} catch ( e ) {

		$( 'gallery' ).textContent = `The sample list is missing (${e.message}).`;
		return;

	}

	const root = $( 'gallery' );
	root.replaceChildren();
	for ( const group of site.groups ) {

		const section = h( 'section', { className: 'group' }, h( 'h3', {}, group.title ), group.note ? h( 'p', {}, group.note ) : null );
		for ( const item of group.items ) {

			gallery.push( item );
			const row = h( 'button', { type: 'button', className: 'sample', dataset: { id: item.id }, title: item.title },
				item.thumb ? h( 'img', { src: item.thumb, alt: '', loading: 'lazy', width: 44, height: 44 } ) : h( 'i', { className: 'nothumb' } ),
				h( 'div', {}, h( 'b', {}, item.title ), h( 'span', {}, item.about ) ),
			);
			row.addEventListener( 'click', () => openSample( item ) );
			section.append( row );

		}

		root.append( section );

	}

	// #limits and the like keep their place; #build (the thumbnail pass) wants nothing loaded
	if ( ! openFromHash() && location.hash !== '#build' ) {

		openSample( gallery.find( ( i ) => i.id === site.start ) ?? gallery[ 0 ], undefined, { keepHash: Boolean( location.hash ) } );

	}

}

function openFromHash() {

	const [ id, version ] = decodeURIComponent( location.hash.slice( 1 ) ).split( '/' );
	const item = gallery.find( ( i ) => i.id === id );
	if ( ! item ) return false;
	openSample( item, version || item.default );
	return true;

}

// -- controls -----------------------------------------------------------

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

for ( const b of document.querySelectorAll( '[data-view]' ) ) {

	b.addEventListener( 'click', () => {

		view = b.dataset.view;
		frameCurrent();

	} );

}

function toggle( id, value ) {

	$( id ).setAttribute( 'aria-pressed', String( value ) );
	return value;

}

$( 'fit' ).addEventListener( 'click', frameCurrent );
$( 'wire' ).addEventListener( 'click', () => {

	wireframe = toggle( 'wire', ! wireframe );
	if ( current?.object ) setWireframe( current.object, wireframe );

} );
$( 'edges' ).addEventListener( 'click', () => {

	edges = toggle( 'edges', ! edges );
	if ( current?.object && ( ! edges || current.measured.triangles <= EDGE_LIMIT ) ) setEdges( current.object, edges );

} );
$( 'dims' ).addEventListener( 'click', () => {

	showDims = toggle( 'dims', ! showDims );
	dims.visible = showDims;

} );
$( 'lines' ).addEventListener( 'click', () => {

	lines = toggle( 'lines', ! lines );
	if ( current?.data ) show( current );

} );
$( 'units' ).addEventListener( 'change', () => {

	autoUnits = false;
	if ( current?.data ) show( { ...current, units: $( 'units' ).value, unitsNote: null } );

} );
document.addEventListener( 'keydown', ( e ) => {

	if ( e.target.closest?.( 'input, textarea, select' ) || e.ctrlKey || e.metaKey || e.altKey ) return;
	const key = e.key.toLowerCase();
	if ( key === 'f' ) frameCurrent();
	else if ( key === 'w' ) $( 'wire' ).click();
	else if ( key === 'e' ) $( 'edges' ).click();
	else if ( key === 'd' ) $( 'dims' ).click();

} );
window.addEventListener( 'hashchange', openFromHash );

toggle( 'edges', edges );
toggle( 'dims', showDims );
for ( const b of document.querySelectorAll( '[data-view]' ) ) b.setAttribute( 'aria-pressed', String( b.dataset.view === view ) );

loadGallery();

// -- thumbnails (used when the site is built) ----------------------------

let thumbRenderer = null;

/**
 * Convert the DWG at `url` and render it into a square image with a
 * transparent background: a data: URL (WebP). Used by the site build in
 * headless Chrome; harmless on the page.
 */
async function thumbnail( url, size = 160, units = null ) {

	const data = await fetchBytes( url );
	const t0 = performance.now();
	const result = await loader.convert( data, { name: url.split( '/' ).pop(), lines: true, units, textures: false } );
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
	const { triangles } = measure( object );
	s.remove( object );
	dispose( object );
	return { image, triangles, convertMs, warnings: result.report.stats?.warnings?.length ?? 0 };

}

/**
 * Every test drawing of the gallery, in every version, through the panel's
 * check, as the page converts it. For automated runs: `await demo.verifyAll()`.
 */
async function verifyAll() {

	const failures = [];
	let checked = 0;
	for ( const item of gallery ) {

		for ( const v of item.versions ) {

			if ( ! v.expected ) continue;
			const data = await fetchBytes( v.file );
			const units = v.version === 'AC1014' ? 'mm' : null;
			const result = await loader.convert( data, { name: v.file.split( '/' ).pop(), lines: true, units, textures: false } );
			const object = await buildObject( result, v.file.split( '/' ).pop() );
			const check = checkAgainst( v.expected, measure( object, worldAxis( v.expected ) ), result.report );
			dispose( object );
			checked ++;
			if ( ! check.ok ) failures.push( { file: v.file, failed: check.rows.filter( ( r ) => ! r.pass ), skipped: check.skipped } );

		}

	}

	return { checked, failures };

}

// for the site build and automated checks
window.demo = { loader, scene, thumbnail, measure, checkAgainst, verifyAll, get current() {

	return current;

} };
