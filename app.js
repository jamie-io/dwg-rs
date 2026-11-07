// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

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

// DWG_ERR_* of libredwg/include/dwg.h, the recoverable ones
const DECODE_ERRORS = [
	[ 1, 'WRONGCRC', 'a checksum did not match' ],
	[ 2, 'NOTYETSUPPORTED', 'data LibreDWG cannot decode yet' ],
	[ 4, 'UNHANDLEDCLASS', 'an object class LibreDWG does not handle' ],
	[ 8, 'INVALIDTYPE', 'an object of unknown type' ],
	[ 16, 'INVALIDHANDLE', 'a reference to a missing object' ],
	[ 32, 'INVALIDEED', 'invalid extended entity data' ],
	[ 64, 'VALUEOUTOFBOUNDS', 'a value out of its range' ],
];

// tests/bricscad/measure.py, in mm
const BOUNDS_MM = 0.2;
const BOUNDS_OF_DIAGONAL = 1e-4;
const SHARE = 0.01;
const DEVIATION_MM = 0.1;
const FINE_MM = 10 * DEVIATION_MM;
const FACING_SHARE = 0.999;

// EdgesGeometry gets slow above this
const EDGE_LIMIT = 400000;

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

function num( value, decimals = 0 ) {

	const [ int, frac ] = Math.abs( value ).toFixed( decimals ).split( '.' );
	return ( value < 0 && Number( Math.abs( value ).toFixed( decimals ) ) !== 0 ? '\u2212' : '' ) + group( int ) + ( frac ? '.' + frac : '' );

}

function decimalsFor( mm ) {

	const a = Math.abs( mm );
	return a >= 1000 ? 0 : a >= 100 ? 1 : a >= 10 ? 2 : 3;

}

function length( mm, { unit = true, decimals } = {} ) {

	if ( Math.abs( mm ) >= 100000 ) return num( mm / 1000, decimals ?? 1 ) + ( unit ? ' m' : '' );
	return num( mm, decimals ?? Math.min( 1, decimalsFor( mm ) ) ) + ( unit ? ' mm' : '' );

}

// osifont has no thin spaces
function dimText( mm ) {

	const whole = Math.abs( mm ) >= 10 && Math.abs( mm - Math.round( mm ) ) < 0.05;
	return length( mm, whole && Math.abs( mm ) < 100000 ? { decimals: 0 } : {} ).replaceAll( NNBSP, ' ' ).replace( '\u2212', '-' );

}

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
	// DWG X, Y, Z are glTF +x, -z, +y
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
	viewDirection.subVectors( camera.position, controls.target ).normalize();
	triad.camera.position.copy( viewDirection ).multiplyScalar( 4 );
	triad.camera.up.copy( camera.up );
	triad.camera.lookAt( 0, 0, 0 );
	renderer.clearDepth();
	renderer.setViewport( 8, 8, triad.size, triad.size );
	renderer.render( triad.scene, triad.camera );

} );

const VIEWS = {
	// AutoCAD's SE isometric
	iso: new THREE.Vector3( 1, 1, 1 ),
	top: new THREE.Vector3( 0, 1, 1e-4 ),
	front: new THREE.Vector3( 0, 0, 1 ),
	right: new THREE.Vector3( 1, 0, 0 ),
};

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

function dispose( object ) {

	object.traverse( ( o ) => {

		o.geometry?.dispose();
		for ( const m of [ o.material ].flat().filter( Boolean ) ) {

			for ( const value of Object.values( m ) ) if ( value?.isTexture ) value.dispose();
			m.dispose();

		}

	} );

}

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

				// 40°: not the steps of a coarsely tessellated blend
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

/** `sizes`: mm, by DWG axis. */
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

/** Like tests/bricscad/glb.py: mirrored instances are rewound. */
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

/** DWG bounds (Z-up, mm) of a world box (Y-up, m). */
function dwgBounds( box ) {

	return [
		[ box.min.x * 1000, - box.max.z * 1000, box.min.y * 1000 ],
		[ box.max.x * 1000, - box.min.z * 1000, box.max.y * 1000 ],
	];

}

function extents( bounds ) {

	return [ 0, 1, 2 ].map( ( k ) => bounds[ 1 ][ k ] - bounds[ 0 ][ k ] );

}

/** The checks of tests/bricscad/measure.py. */
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

function worldAxis( expected ) {

	if ( ! Array.isArray( expected?.facing ) ) return null;
	const [ x, y, z ] = expected.facing;
	return new THREE.Vector3( x, z, - y );

}

// -- state --------------------------------------------------------------

const loader = new DWGLoader().preload();

let current = null;
let site = { facts: {}, groups: [] };
let gallery = [];
const opened = [];
const toggles = { edges: true, wire: false, dims: true, lines: true };
let view = 'iso';
let busy = 0;
let converted = 0;

// -- command line ---------------------------------------------------------

const log = $( 'log' );

function print( text, kind = '' ) {

	log.append( kind ? h( 'span', { className: kind }, text ) : text, '\n' );
	log.scrollTop = log.scrollHeight;

}

function setState( text ) {

	$( 'state' ).textContent = text ?? '';

}

// -- converting -----------------------------------------------------------

/** The DWG version from the file header ("AC1032"), or null. */
function versionOf( data ) {

	const magic = new TextDecoder().decode( new Uint8Array( data, 0, Math.min( 6, data.byteLength ) ) );
	return /^AC\d{4}$/.test( magic ) ? magic : null;

}

function notDwg( data ) {

	if ( versionOf( data ) ) return null;
	const head = new TextDecoder( 'latin1' ).decode( new Uint8Array( data, 0, Math.min( 32, data.byteLength ) ) );
	if ( /^AutoCAD Binary DXF/.test( head ) || /^\s*0\s*\r?\n\s*SECTION/.test( head ) ) return 'This is a DXF file. dwg-rs reads DWG only.';
	if ( /^PK/.test( head ) ) return 'This is a zip archive, not a DWG.';
	const shown = head.slice( 0, 6 ).replace( /[^\x20-\x7e]/g, '.' );
	return `Not a DWG file: DWG files start with "AC1", this one with "${shown}".`;

}

function versionName( version ) {

	if ( ! version ) return 'unknown';
	return VERSIONS[ version ] ? `${version} (AutoCAD ${VERSIONS[ version ]})` : version;

}

async function show( entry ) {

	const ticket = ++ busy;
	const { name, data, textures } = entry;
	const version = versionOf( data );
	print( `Opening ${name}: ${versionName( version )}, ${bytes( data.byteLength )}${entry.units ? `, units ${entry.units}` : ''}` );
	if ( entry.unitsNote ) print( entry.unitsNote );
	const why = notDwg( data );
	if ( why ) {

		fail( entry, why );
		return;

	}

	setState( converted === 0 ? `Loading the converter (${site.facts.wasmGzip ?? 'a few MB'}) and converting…` : 'Converting…' );
	$( 'empty' ).classList.add( 'hidden' );
	const t0 = performance.now();
	let object, result;
	try {

		result = await loader.convert( data, { name, lines: toggles.lines, units: entry.units || null, textures: textures ?? false } );
		object = await buildObject( result, name );

	} catch ( e ) {

		if ( ticket !== busy ) return;
		console.warn( e );
		fail( entry, e.code ? `Conversion failed (${e.code}): ${e.message}` : `Conversion failed: ${e.message ?? e}`, e.report?.summary ?? e.stderr ?? '' );
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
	// far from the origin, float32 on the GPU jitters
	const center = measured.box.isEmpty() ? new THREE.Vector3() : measured.box.getCenter( new THREE.Vector3() );
	object.position.sub( center );
	const shown = measured.box.clone().translate( center.clone().negate() );

	current = { ...entry, object, result, report: result.report, measured, expected, center, error: null, totalMs: performance.now() - t0 };
	setWireframe( object, toggles.wire );
	if ( toggles.edges && measured.triangles <= EDGE_LIMIT ) setEdges( object, true );
	scene.add( object );
	placeGrid( shown );
	drawDimensions( shown, extents( dwgBounds( measured.box ) ) );
	dims.visible = toggles.dims;
	frameCurrent();
	$( 'glbButton' ).disabled = false;
	setState( `${name}: ${num( measured.triangles )} triangles` );

	for ( const line of ( result.report.summary ?? '' ).split( '\n' ) ) {

		print( line, line.startsWith( 'warning' ) ? 'warn' : '' );
		const note = line.startsWith( 'warning' ) ? explain( line ) : null;
		if ( note ) print( `  ${note}` );

	}

	if ( result.report.missingTextures?.length ) print( `texture images not found: ${result.report.missingTextures.join( ', ' )}. Open them together with the DWG.`, 'warn' );
	if ( expected ) {

		const check = checkAgainst( expected, measured, result.report );
		print( `Check: ${verdict( check )}`, check.ok ? 'ok' : 'bad' );

	}

	print( `${num( result.convertMs ?? 0 )} ms in WebAssembly, ${num( current.totalMs )} ms until shown.` );
	renderProps();

}

function fail( entry, message, summary = '' ) {

	if ( current?.object ) {

		scene.remove( current.object );
		dispose( current.object );
		drawDimensions( new THREE.Box3(), [] );
		placeGrid( new THREE.Box3() );

	}

	current = { ...entry, object: null, result: null, report: null, measured: null, expected: null, error: message };
	$( 'empty' ).classList.remove( 'hidden' );
	$( 'empty' ).textContent = 'Nothing to show for this file.';
	$( 'glbButton' ).disabled = true;
	print( message, 'bad' );
	if ( summary ) print( summary );
	setState( message );
	renderProps();

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

function explain( warning ) {

	const bits = /recoverable decode errors \(0x([0-9a-f]+)\)/i.exec( warning );
	if ( bits ) {

		const value = parseInt( bits[ 1 ], 16 );
		const names = DECODE_ERRORS.filter( ( [ bit ] ) => value & bit ).map( ( [ , name ] ) => name );
		const facts = site.facts;
		const suite = current?.item?.versions && facts.decodeWarnings
			? ` ${facts.decodeWarnings} of the ${facts.corpusFiles} test files have this; their geometry matches.`
			: ' Check the result for missing parts.';
		return `0x${bits[ 1 ]}: ${names.join( ', ' ) || 'unknown bits'}.${suite}`;

	}

	if ( /no units \(INSUNITS 0\)/.test( warning ) ) return 'If it is 1000 times too large, it was drawn in millimeters: UNITS mm.';
	if ( /less than 1 cm across/.test( warning ) ) return 'On this page: UNITS m, or the Units list.';
	return null;

}

function verdict( check ) {

	if ( check.skipped.length ) return `entities skipped: ${check.skipped.map( ( [ k, n ] ) => `${k} ${n}` ).join( ', ' )}`;
	const failed = check.rows.filter( ( r ) => ! r.pass ).map( ( r ) => r.what.toLowerCase() );
	if ( failed.length ) return `out of tolerance: ${list( failed )}`;
	const offs = check.rows.filter( ( r ) => r.off && ! r.skippedCheck ).map( ( r ) => `${r.what.toLowerCase()} ${r.off.replace( ' at most', '' )}` );
	return `${check.fine ? 'extents within tolerance (thinner than 1 mm: area and volume not checked)' : 'within tolerance'}${offs.length ? ` (${offs.join( ', ' )})` : ''}`;

}

// -- properties -----------------------------------------------------------

function row( name, value, className = '' ) {

	return h( 'tr', {}, h( 'th', { scope: 'row' }, name ), h( 'td', { className }, value ) );

}

function category( name, rows ) {

	return h( 'tbody', {}, h( 'tr', { className: 'cat' }, h( 'th', { colSpan: 2, scope: 'rowgroup' }, name ) ), rows );

}

function renderProps() {

	const panel = $( 'props' );
	if ( ! current ) {

		panel.replaceChildren( h( 'p', { className: 'none' }, 'Nothing selected.' ) );
		return;

	}

	const { item, report, measured, result } = current;
	const general = [ row( 'Name', current.name ) ];
	if ( item ) general.push( row( 'Sample', item.title ), row( 'Description', item.about ) );
	general.push( row( 'Source', item?.source ? 'gltf2dwg (round trip)' : item ? 'BricsCAD V27, tests/bricscad' : 'your file, not uploaded' ) );
	general.push( row( 'Format', versionName( versionOf( current.data ) ) ) );
	general.push( row( 'File size', bytes( current.data.byteLength ) ) );
	general.push( row( 'Units', current.units ? `${current.units} (set)` : 'from drawing' ) );
	const sections = [ category( 'General', general ) ];

	if ( current.error ) {

		sections.push( category( 'Error', [ row( 'Message', current.error, 'fail' ) ] ) );
		panel.replaceChildren( h( 'table', { className: 'props' }, sections ) );
		return;

	}

	const stats = report.stats ?? {};
	const bounds = dwgBounds( measured.box );
	const empty = measured.box.isEmpty();
	sections.push( category( 'Geometry', [
		row( 'Size X, Y, Z', empty ? '—' : lengths( extents( bounds ), ', ' ) ),
		row( 'Extents min', empty ? '—' : lengths( bounds[ 0 ], ', ' ) ),
		row( 'Extents max', empty ? '—' : lengths( bounds[ 1 ], ', ' ) ),
		row( 'Triangles', num( measured.triangles ) ),
		row( 'Line segments', num( measured.segments ) ),
		row( 'Block instances', num( stats.instances ?? 0 ) ),
	] ) );

	const conversion = [];
	for ( const [ type, n ] of Object.entries( stats.converted ?? {} ) ) conversion.push( row( `Converted ${type}`, num( n ) ) );
	for ( const [ type, n ] of Object.entries( stats.skipped ?? {} ) ) conversion.push( row( `Skipped ${type}`, num( n ), 'fail' ) );
	for ( const [ what, n ] of Object.entries( stats.repaired ?? {} ) ) conversion.push( row( `Repaired: ${what}`, num( n ) ) );
	if ( conversion.length === 0 ) conversion.push( row( 'Converted', 'nothing in model space' ) );
	conversion.push(
		row( 'GLB size', bytes( result.blocks.reduce( ( s, b ) => s + b.glb.byteLength, 0 ) ) ),
		row( 'Time', `${num( result.convertMs ?? 0 )} ms in WebAssembly` ),
	);
	sections.push( category( 'Conversion', conversion ) );

	if ( current.expected ) sections.push( checkSection() );
	if ( item?.source ) sections.push( roundTripSection() );

	const warnings = ( stats.warnings ?? [] ).map( ( w ) => typeof w === 'string' ? w : JSON.stringify( w ) );
	if ( warnings.length ) {

		sections.push( category( 'Warnings', warnings.flatMap( ( w, i ) => {

			const note = explain( w );
			return [ row( String( i + 1 ), w ), note ? row( '', note ) : null ];

		} ).filter( Boolean ) ) );

	}

	panel.replaceChildren( h( 'table', { className: 'props' }, sections ) );

}

function checkSection() {

	const check = checkAgainst( current.expected, current.measured, current.report );
	const rows = [];
	for ( const r of check.rows ) {

		const className = r.skippedCheck ? '' : r.pass ? 'pass' : 'fail';
		if ( r.what === 'Faces' ) {

			rows.push( row( 'Faces', `${r.measured}${r.allowed ? `, limit ${r.allowed}` : ''}`, className ) );
			continue;

		}

		rows.push( row( `${r.what} expected`, r.expected ), row( `${r.what} measured`, r.measured ) );
		rows.push( row( `${r.what} off by`, `${r.off.replace( ' at most', '' )}, ${r.skippedCheck ? 'not checked' : `limit ${r.allowed}`}`, className ) );

	}

	rows.push( row( 'Result', verdict( check ), check.ok ? 'pass' : 'fail' ) );
	return category( 'Check (tests/bricscad/measure.py)', rows );

}

function roundTripSection() {

	const source = current.item.source;
	const m = current.measured;
	const [ slo, shi ] = source.bounds;
	const off = Math.max( ...[ 0, 1, 2 ].flatMap( ( k ) => [ Math.abs( m.box.min.getComponent( k ) - slo[ k ] ), Math.abs( m.box.max.getComponent( k ) - shi[ k ] ) ] ) ) * 1000;
	const kept = new Set();
	let factors = '';
	current.object.traverse( ( o ) => {

		for ( const mat of [ o.material ].flat().filter( Boolean ) ) {

			if ( mat.map ) kept.add( 'base color' );
			if ( mat.normalMap ) kept.add( 'normal' );
			if ( mat.roughnessMap || mat.metalnessMap ) kept.add( 'metallic-roughness' );
			if ( mat.aoMap ) kept.add( 'occlusion' );
			if ( mat.emissiveMap ) kept.add( 'emissive' );
			if ( ! factors && o.isMesh ) factors = `metalness ${num( mat.metalness, 2 )}, roughness ${num( mat.roughness, 2 )}`;

		}

	} );
	const had = [ ...new Set( source.materials.flatMap( ( mat ) => mat.maps ) ) ];
	const lost = had.filter( ( x ) => ! kept.has( x ) );
	return category( 'Round trip', [
		row( 'Source', h( 'a', { href: source.source, target: '_blank', rel: 'noopener' }, `Khronos ${source.title}` ) ),
		row( 'License', `${source.license}, ${source.author}` ),
		row( 'Images', source.images ),
		row( 'Written by', source.writer ),
		row( 'Triangles', `${num( m.triangles )}, source ${num( source.triangles )}`, m.triangles === source.triangles ? 'pass' : 'fail' ),
		row( 'Area', `${num( m.area, 3 )} m², source ${num( source.area, 3 )}; ${percent( ( m.area - source.area ) / source.area, 4 )}` ),
		row( 'Extents', `${length( off, { decimals: 3 } )} off` ),
		row( 'Maps kept', kept.size ? list( [ ...kept ] ) : 'none' ),
		row( 'Maps lost', lost.length ? list( lost ) : 'none', lost.length ? 'fail' : '' ),
		row( 'Factors', `${factors} (gltf2dwg writes one texture per material, the base color)` ),
	] );

}

// -- files and samples ----------------------------------------------------

async function openFiles( files ) {

	const dwg = files.find( ( f ) => /\.dwg$/i.test( f.name ) ) ?? ( files.length === 1 ? files[ 0 ] : null );
	if ( ! dwg ) {

		print( 'Choose a .dwg file; texture images may come along with it.', 'bad' );
		return;

	}

	const images = files.filter( ( f ) => f !== dwg && /\.(png|jpe?g)$/i.test( f.name ) );
	const textures = images.length ? {} : null;
	for ( const f of images ) textures[ f.name ] = await f.arrayBuffer();
	if ( images.length ) print( `with ${plural( images.length, 'texture image' )}: ${images.map( ( f ) => f.name ).join( ', ' )}` );
	if ( location.hash ) history.replaceState( null, '', location.pathname + location.search );
	const entry = { name: dwg.name, data: await dwg.arrayBuffer(), textures, units: $( 'units' ).value };
	opened.unshift( entry );
	opened.splice( 5 );
	renderTree();
	markCurrent( null, null, entry );
	show( entry );

}

async function fetchBytes( url ) {

	const response = await fetch( url );
	if ( ! response.ok ) throw new Error( `${url}: HTTP ${response.status}` );
	return response.arrayBuffer();

}

let autoUnits = false;

async function openSample( item, version = item.default, { keepHash = false } = {} ) {

	const v = item.versions.find( ( x ) => x.version === version ) ?? item.versions.at( - 1 );
	markCurrent( item.id, v.version );
	if ( ! keepHash ) history.replaceState( null, '', `#${item.id}${v.version !== item.default ? '/' + v.version : ''}` );
	// R14 has no INSUNITS; measure.py converts it as mm too
	let units = $( 'units' ).value;
	let unitsNote = null;
	if ( v.version === 'AC1014' && item.versions.length > 1 && ( units === '' || autoUnits ) ) {

		units = 'mm';
		autoUnits = true;
		unitsNote = 'R14 has no units field; converted as mm, like the test suite.';

	} else if ( autoUnits ) {

		units = '';
		autoUnits = false;

	}

	$( 'units' ).value = units;
	let data;
	try {

		data = await fetchBytes( v.file );

	} catch ( e ) {

		print( `Cannot load the sample: ${e.message}`, 'bad' );
		return;

	}

	await show( { name: v.file.split( '/' ).pop(), data, textures: null, item, version: v.version, units, unitsNote } );

}

function findSample( words ) {

	const q = words.toLowerCase();
	return gallery.find( ( i ) => i.id.toLowerCase() === q ) ?? gallery.find( ( i ) => i.title.toLowerCase().startsWith( q ) ) ?? gallery.find( ( i ) => i.id.toLowerCase().includes( q ) || i.title.toLowerCase().includes( q ) );

}

function parseVersion( text ) {

	const t = text.toUpperCase();
	if ( VERSIONS[ t ] ) return t;
	return Object.keys( VERSIONS ).find( ( k ) => VERSIONS[ k ] === t ) ?? null;

}

// -- the sample tree --------------------------------------------------------

function treeNode( label, { icon = null, className = '', expanded = null, onClick, dataset = {} } ) {

	const button = h( 'button', { type: 'button', className, dataset },
		h( 'span', { className: 'twisty', 'aria-hidden': 'true' }, expanded === null ? '' : expanded ? '▼' : '▶' ),
		icon, h( 'span', {}, label ) );
	button.addEventListener( 'click', onClick );
	const li = h( 'li', { role: 'treeitem' }, button );
	if ( expanded !== null ) li.setAttribute( 'aria-expanded', String( expanded ) );
	return li;

}

function setExpanded( li, on ) {

	li.setAttribute( 'aria-expanded', String( on ) );
	li.querySelector( ':scope > button > .twisty' ).textContent = on ? '▼' : '▶';
	li.querySelector( ':scope > ul' )?.classList.toggle( 'hidden', ! on );

}

function renderTree() {

	const tree = $( 'tree' );
	tree.replaceChildren();
	const groups = [];
	if ( opened.length ) groups.push( { title: 'Opened files', files: opened } );
	groups.push( ...site.groups );
	for ( const group of groups ) {

		const li = treeNode( group.title, { className: 'group', expanded: true, onClick: () => setExpanded( li, li.getAttribute( 'aria-expanded' ) !== 'true' ) } );
		li.classList.add( 'group' );
		if ( group.note ) li.title = group.note;
		const ul = h( 'ul', { role: 'group' } );
		for ( const entry of group.files ?? [] ) {

			ul.append( treeNode( entry.name, { icon: h( 'span', { className: 'icon' } ), dataset: { file: entry.name }, onClick: () => {

				run( `OPEN ${entry.name}`, { echo: true, quiet: true } );
				markCurrent( null, null, entry );
				show( entry );

			} } ) );

		}

		for ( const item of group.items ?? [] ) {

			const icon = item.thumb ? h( 'img', { src: item.thumb, alt: '', loading: 'lazy', width: 18, height: 18 } ) : h( 'span', { className: 'icon' } );
			const versions = item.versions.length > 1;
			const node = treeNode( item.title, { icon, expanded: versions ? false : null, dataset: { id: item.id }, onClick: ( e ) => {

				if ( versions && e.target.classList.contains( 'twisty' ) ) {

					setExpanded( node, node.getAttribute( 'aria-expanded' ) !== 'true' );
					return;

				}

				run( `SAMPLE ${item.id}` );

			} } );
			node.title = item.about;
			if ( versions ) {

				const sub = h( 'ul', { role: 'group', className: 'hidden' } );
				for ( const v of item.versions ) {

					sub.append( treeNode( `${VERSIONS[ v.version ] ?? v.version} (${v.version})`, { className: 'version', dataset: { id: item.id, version: v.version }, onClick: () => run( `SAMPLE ${item.id} ${VERSIONS[ v.version ] ?? v.version}` ) } ) );

				}

				node.append( sub );

			}

			ul.append( node );

		}

		li.append( ul );
		tree.append( li );

	}

	if ( current?.item ) markCurrent( current.item.id, current.version );

}

function markCurrent( id, version, file = null ) {

	for ( const b of document.querySelectorAll( '.tree button' ) ) {

		let on = false;
		if ( file ) on = b.dataset.file === file.name;
		else if ( b.dataset.version ) on = b.dataset.id === id && b.dataset.version === version;
		else if ( b.dataset.id ) on = b.dataset.id === id && ! b.parentElement.querySelector( '[data-version]' );
		b.setAttribute( 'aria-current', String( on ) );

	}

	if ( id ) {

		const node = document.querySelector( `.tree li[aria-expanded] > button[data-id="${CSS.escape( id )}"]` )?.parentElement;
		if ( node ) setExpanded( node, true );

	}

}

async function loadGallery() {

	try {

		site = await ( await fetch( 'gallery.json' ) ).json();

	} catch ( e ) {

		print( `The sample list is missing (${e.message}).`, 'bad' );
		return;

	}

	gallery = site.groups.flatMap( ( g ) => g.items );
	renderTree();
	print( `dwg-rs ${site.facts.commit ?? ''} (WebAssembly, ${site.facts.wasmGzip ?? ''}), LibreDWG ${site.facts.libredwg ?? ''}. Files are converted in this tab; nothing is uploaded.` );
	print( 'Drop a DWG on the drawing area or type OPEN. HELP lists the commands.' );
	const [ id ] = decodeURIComponent( location.hash.slice( 1 ) ).split( '/' );
	if ( HELP_TOPICS.includes( id ) ) openHelp( id );
	// #build: the thumbnail pass
	if ( ! openFromHash() && location.hash !== '#build' ) {

		const start = gallery.find( ( i ) => i.id === site.start ) ?? gallery[ 0 ];
		run( `SAMPLE ${start.id}`, { keepHash: Boolean( location.hash ) } );

	}

}

function openFromHash() {

	const [ id, version ] = decodeURIComponent( location.hash.slice( 1 ) ).split( '/' );
	const item = gallery.find( ( i ) => i.id === id );
	if ( ! item ) return false;
	run( `SAMPLE ${item.id}${version ? ' ' + ( VERSIONS[ version ] ?? version ) : ''}`, { keepHash: true } );
	return true;

}

// -- commands -------------------------------------------------------------

const HELP_TOPICS = [ 'entities', 'limits', 'tests', 'usage', 'license', 'credits' ];

const COMMANDS = {
	OPEN: 'open a DWG file (with its texture images)',
	SAMPLE: 'SAMPLE name [version]: open a sample, e.g. SAMPLE spring 2004',
	VERSION: 'VERSION R14|2000|2004|2007|2010|2013|2018: the same sample, saved as another version',
	UNITS: 'UNITS mm|cm|m|in|ft|drawing: convert again in these units',
	ZOOM: 'zoom to the extents (F)',
	'ISO, TOP, FRONT, RIGHT': 'view direction',
	'EDGES, WIRE, DIMS, LINES': 'switch edges (E), triangles (W), dimensions (D), lines and curves [ON|OFF]',
	CHECK: 'compare the shown sample with its expected values',
	GLB: 'save the converted glTF binary',
	COPY: 'copy the converter summary',
	REPORT: 'open a GitHub issue form with the summary',
	'HELP [topic]': `this list, or a help topic: ${HELP_TOPICS.join( ', ' )}`,
	CLEAR: 'clear this window',
};

function syncToggles() {

	for ( const el of document.querySelectorAll( '[data-toggle]' ) ) {

		const on = String( toggles[ el.dataset.toggle ] );
		el.setAttribute( el.getAttribute( 'role' ) === 'menuitem' ? 'aria-checked' : 'aria-pressed', on );

	}

	for ( const el of document.querySelectorAll( '[data-view]' ) ) el.setAttribute( 'aria-pressed', String( el.dataset.view === view ) );

}

function setToggle( name, value ) {

	toggles[ name ] = value;
	syncToggles();
	if ( name === 'wire' && current?.object ) setWireframe( current.object, value );
	if ( name === 'edges' && current?.object && ( ! value || current.measured.triangles <= EDGE_LIMIT ) ) setEdges( current.object, value );
	if ( name === 'dims' ) dims.visible = value;
	if ( name === 'lines' && current?.data && ! current.error ) show( current );

}

function frameCurrent() {

	syncToggles();
	if ( current?.object ) frame( current.object, camera, controls, VIEWS[ view ] );

}

function run( line, { echo = true, keepHash = false, quiet = false } = {} ) {

	const words = line.trim().split( /\s+/ ).filter( Boolean );
	if ( words.length === 0 ) return;
	const verb = words[ 0 ].toUpperCase();
	const rest = words.slice( 1 );
	if ( echo ) print( `Command: ${line.trim()}`, 'in' );
	if ( quiet ) return;
	const onOff = ( name ) => {

		const arg = ( rest[ 0 ] ?? '' ).toUpperCase();
		setToggle( name, arg === 'ON' ? true : arg === 'OFF' ? false : ! toggles[ name ] );

	};

	switch ( verb ) {

		case 'OPEN': $( 'files' ).click(); break;
		case 'Z': case 'ZOOM': case 'FIT': frameCurrent(); break;
		case 'ISO': case 'TOP': case 'FRONT': case 'RIGHT':
			view = verb.toLowerCase();
			frameCurrent();
			break;
		case 'VIEW': {

			const name = ( rest[ 0 ] ?? '' ).toLowerCase().replace( /^se/, 'iso' );
			if ( VIEWS[ name ] ) {

				view = name;
				frameCurrent();

			} else print( 'VIEW iso|top|front|right' );
			break;

		}

		case 'EDGES': onOff( 'edges' ); break;
		case 'WIRE': case 'TRIANGLES': onOff( 'wire' ); break;
		case 'DIMS': case 'DIMENSIONS': onOff( 'dims' ); break;
		case 'LINES': onOff( 'lines' ); break;
		case 'UNITS': {

			const u = ( rest[ 0 ] ?? '' ).toLowerCase();
			const value = u === 'drawing' || u === 'auto' ? '' : u;
			if ( ! [ ...$( 'units' ).options ].some( ( o ) => o.value === value ) || rest.length === 0 ) {

				print( `Units: ${$( 'units' ).value || 'from drawing'}. UNITS mm|cm|m|in|ft|drawing` );
				break;

			}

			$( 'units' ).value = value;
			autoUnits = false;
			if ( current?.data ) show( { ...current, units: value, unitsNote: null } );
			break;

		}

		case 'SAMPLE': {

			const versionWord = rest.length > 1 ? parseVersion( rest.at( - 1 ) ) : null;
			const item = findSample( ( versionWord ? rest.slice( 0, - 1 ) : rest ).join( ' ' ) );
			if ( ! item ) {

				print( rest.length ? `No sample "${rest.join( ' ' )}".` : 'SAMPLE name [version]' );
				break;

			}

			if ( versionWord && ! item.versions.some( ( v ) => v.version === versionWord ) ) print( `${item.title} has no ${VERSIONS[ versionWord ] ?? versionWord} version.` );
			openSample( item, versionWord ?? item.default, { keepHash } );
			break;

		}

		case 'VERSION': {

			const v = parseVersion( rest[ 0 ] ?? '' );
			if ( ! current?.item || current.item.versions.length < 2 ) print( 'Only the test samples come in several versions.' );
			else if ( ! v || ! current.item.versions.some( ( x ) => x.version === v ) ) print( `VERSION ${current.item.versions.map( ( x ) => VERSIONS[ x.version ] ).join( '|' )}` );
			else openSample( current.item, v );
			break;

		}

		case 'CHECK': {

			if ( ! current?.expected ) {

				print( 'No expected values: only the test samples have them.' );
				break;

			}

			const check = checkAgainst( current.expected, current.measured, current.report );
			for ( const r of check.rows ) print( `  ${r.what.padEnd( 8 )} ${r.measured}${r.expected ? `  expected ${r.expected}` : ''}${r.off ? `  ${r.off}` : ''}${r.allowed ? `  limit ${r.allowed}` : ''}`, r.skippedCheck ? '' : r.pass ? 'ok' : 'bad' );
			print( `Check: ${verdict( check )}`, check.ok ? 'ok' : 'bad' );
			break;

		}

		case 'GLB': case 'SAVEAS': case 'EXPORT':
			if ( current?.result ) downloadGlb();
			else print( 'Nothing to save.' );
			break;
		case 'COPY':
			if ( ! current?.report ) {

				print( 'Nothing converted yet.' );
				break;

			}

			navigator.clipboard.writeText( summaryText( current ) ).then( () => print( 'Copied.' ), () => print( 'Copying failed.', 'bad' ) );
			break;
		case 'REPORT':
			window.open( issueUrl( current ), '_blank', 'noopener' );
			print( 'Opened a GitHub issue form with the summary. Nothing is sent until you submit it; the DWG is not attached.' );
			break;
		case 'HELP': case '?': {

			const topic = ( rest[ 0 ] ?? '' ).toLowerCase();
			if ( HELP_TOPICS.includes( topic ) ) {

				openHelp( topic );
				break;

			}

			for ( const [ name, text ] of Object.entries( COMMANDS ) ) print( `  ${name.padEnd( 24 )} ${text}` );
			break;

		}

		case 'CLEAR': case 'CLS': log.replaceChildren(); break;
		default: print( `Unknown command "${words[ 0 ]}". HELP lists the commands.` );

	}

}

function summaryText( entry ) {

	const facts = site.facts;
	return [
		`dwg-rs ${facts.commit ?? '?'} (WebAssembly, ${location.origin}${location.pathname})`,
		`File: ${entry.name}, ${versionName( versionOf( entry.data ) )}, ${bytes( entry.data.byteLength )}`,
		`Options: units ${entry.units || 'from drawing'}, lines ${toggles.lines ? 'on' : 'off'}`,
		'',
		entry.report?.summary ?? entry.error ?? '',
	].join( '\n' );

}

function issueUrl( entry ) {

	const text = entry ? summaryText( entry ) : `dwg-rs ${site.facts.commit ?? '?'} (WebAssembly)`;
	const body = `${text.slice( 0, 4000 )}\n\nWhat looks wrong:\n\n`;
	const title = entry ? `Conversion of ${entry.name}` : 'Demo page';
	return `https://github.com/jamie-io/dwg-rs/issues/new?title=${encodeURIComponent( title )}&body=${encodeURIComponent( body )}`;

}

function downloadGlb() {

	const stem = current.name.replace( /\.dwg$/i, '' );
	for ( const { name, glb } of current.result.blocks ) {

		const a = document.createElement( 'a' );
		a.href = URL.createObjectURL( new Blob( [ glb ], { type: 'model/gltf-binary' } ) );
		a.download = ( current.result.blocks.length > 1 ? `${stem}-${name}` : stem ) + '.glb';
		a.click();
		setTimeout( () => URL.revokeObjectURL( a.href ), 1000 );
		print( `Saved ${a.download}.` );

	}

}

// -- help -------------------------------------------------------------------

const help = $( 'help' );

function openHelp( topic ) {

	if ( ! help.open ) help.showModal();
	$( topic )?.scrollIntoView( { block: 'start' } );

}

$( 'helpClose' ).addEventListener( 'click', () => help.close() );
help.addEventListener( 'close', () => {

	const [ id ] = decodeURIComponent( location.hash.slice( 1 ) ).split( '/' );
	if ( HELP_TOPICS.includes( id ) ) history.replaceState( null, '', location.pathname + location.search );

} );

// -- menus, toolbar, keys -------------------------------------------------

function closeMenus( except = null ) {

	for ( const menu of document.querySelectorAll( '.menu' ) ) {

		if ( menu === except ) continue;
		menu.querySelector( '[role="menu"]' ).classList.add( 'hidden' );
		menu.querySelector( ':scope > button' ).setAttribute( 'aria-expanded', 'false' );

	}

}

for ( const menu of document.querySelectorAll( '.menu' ) ) {

	const button = menu.querySelector( ':scope > button' );
	const list = menu.querySelector( '[role="menu"]' );
	const openMenu = () => {

		closeMenus( menu );
		list.classList.remove( 'hidden' );
		button.setAttribute( 'aria-expanded', 'true' );

	};

	button.addEventListener( 'click', () => list.classList.contains( 'hidden' ) ? openMenu() : closeMenus() );
	button.addEventListener( 'mouseenter', () => {

		if ( document.querySelector( '.menu > button[aria-expanded="true"]' ) ) openMenu();

	} );
	button.addEventListener( 'keydown', ( e ) => {

		if ( e.key === 'ArrowDown' ) {

			e.preventDefault();
			openMenu();
			list.querySelector( '[role="menuitem"]' )?.focus();

		}

	} );
	list.addEventListener( 'keydown', ( e ) => {

		const items = [ ...list.querySelectorAll( '[role="menuitem"]' ) ];
		const i = items.indexOf( document.activeElement );
		if ( e.key === 'ArrowDown' || e.key === 'ArrowUp' ) {

			e.preventDefault();
			items[ ( i + ( e.key === 'ArrowDown' ? 1 : items.length - 1 ) ) % items.length ]?.focus();

		} else if ( e.key === 'Escape' ) {

			closeMenus();
			button.focus();

		}

	} );

}

document.addEventListener( 'click', ( e ) => {

	if ( ! e.target.closest( '.menu' ) ) closeMenus();
	const item = e.target.closest( '[data-command], [data-help]' );
	if ( ! item ) return;
	closeMenus();
	if ( item.dataset.help ) openHelp( item.dataset.help );
	else run( item.dataset.command );

} );

$( 'prompt' ).addEventListener( 'submit', ( e ) => {

	e.preventDefault();
	const line = $( 'cmd' ).value;
	$( 'cmd' ).value = '';
	run( line );

} );

$( 'files' ).addEventListener( 'change', ( e ) => {

	openFiles( [ ...e.target.files ] );
	e.target.value = '';

} );

$( 'units' ).addEventListener( 'change', () => run( `UNITS ${$( 'units' ).value || 'drawing'}` ) );

document.addEventListener( 'keydown', ( e ) => {

	if ( e.key === 'o' && ( e.ctrlKey || e.metaKey ) && ! e.altKey ) {

		e.preventDefault();
		run( 'OPEN' );
		return;

	}

	if ( e.target.closest?.( 'input, textarea, select, dialog' ) || e.ctrlKey || e.metaKey || e.altKey ) return;
	const key = { f: 'ZOOM', w: 'WIRE', e: 'EDGES', d: 'DIMS' }[ e.key.toLowerCase() ];
	if ( key ) run( key );
	else if ( e.key === 'Escape' ) closeMenus();

} );

window.addEventListener( 'hashchange', () => {

	const [ id ] = decodeURIComponent( location.hash.slice( 1 ) ).split( '/' );
	if ( HELP_TOPICS.includes( id ) ) openHelp( id );
	else openFromHash();

} );

for ( const tab of document.querySelectorAll( '[data-tab]' ) ) {

	tab.addEventListener( 'click', () => {

		for ( const t of document.querySelectorAll( '[data-tab]' ) ) t.setAttribute( 'aria-selected', String( t === tab ) );
		for ( const id of [ 'samples', 'properties', 'command' ] ) $( id ).classList.toggle( 'active', id === tab.dataset.tab );

	} );

}

// -- dropping files ---------------------------------------------------------

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
	if ( e.dataTransfer?.files.length ) {

		print( 'Command: OPEN (dropped)', 'in' );
		openFiles( [ ...e.dataTransfer.files ] );

	}

} );

// -- cursor coordinates -----------------------------------------------------

const raycaster = new THREE.Raycaster();
const pointer = new THREE.Vector2();
const ground = new THREE.Plane( new THREE.Vector3( 0, 1, 0 ), 0 );
const hitPoint = new THREE.Vector3();
let pointerQueued = false;

canvas.addEventListener( 'pointermove', ( e ) => {

	pointer.set( e.offsetX / canvas.clientWidth * 2 - 1, - ( e.offsetY / canvas.clientHeight ) * 2 + 1 );
	if ( pointerQueued ) return;
	pointerQueued = true;
	requestAnimationFrame( () => {

		pointerQueued = false;
		$( 'coords' ).textContent = coordinates() ?? ' ';

	} );

} );
canvas.addEventListener( 'pointerleave', () => $( 'coords' ).textContent = ' ' );

function coordinates() {

	if ( ! current?.object ) return null;
	raycaster.setFromCamera( pointer, camera );
	let point = null;
	let where = '';
	if ( current.measured.triangles <= 300000 ) {

		const hit = raycaster.intersectObject( current.object, true ).find( ( i ) => i.object.isMesh );
		if ( hit ) point = hitPoint.copy( hit.point );

	}

	if ( ! point && grid ) {

		ground.constant = - grid.position.y;
		point = raycaster.ray.intersectPlane( ground, hitPoint );
		where = ' (grid)';

	}

	if ( ! point ) return null;
	const p = point.clone().add( current.center );
	return `X ${lengths( [ p.x * 1000, - p.z * 1000, p.y * 1000 ], '  Y ' ).replace( /^(.*)  Y (.*)  Y /, '$1  Y $2  Z ' )}${where}`;

}

syncToggles();
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

/** For automated runs: `await demo.verifyAll()`. */
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
window.demo = { loader, scene, thumbnail, measure, checkAgainst, verifyAll, run, get current() {

	return current;

} };
