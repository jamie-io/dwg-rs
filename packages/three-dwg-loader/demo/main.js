// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

import * as THREE from 'three';
import { OrbitControls } from 'three/addons/controls/OrbitControls.js';
import { RoomEnvironment } from 'three/addons/environments/RoomEnvironment.js';

import { DWGLoader } from '../src/DWGLoader.js';

const $ = ( id ) => document.getElementById( id );

// -- viewer -------------------------------------------------------------

const canvas = $( 'view' );
const renderer = new THREE.WebGLRenderer( { canvas, antialias: true } );
renderer.setPixelRatio( window.devicePixelRatio );
const scene = new THREE.Scene();
scene.background = new THREE.Color( getComputedStyle( document.documentElement ).getPropertyValue( '--view' ).trim() );
scene.environment = new THREE.PMREMGenerator( renderer ).fromScene( new RoomEnvironment(), 0.04 ).texture;
const camera = new THREE.PerspectiveCamera( 40, 1, 0.01, 1000 );
camera.position.set( 2, 1.5, 3 );
const controls = new OrbitControls( camera, canvas );
controls.enableDamping = true;
const grid = new THREE.GridHelper( 10, 20, 0x888888, 0xbbbbbb );
grid.material.transparent = true;
grid.material.opacity = 0.35;
scene.add( grid );

function resize() {

	const { clientWidth: w, clientHeight: h } = canvas.parentElement;
	renderer.setSize( w, h, false );
	camera.aspect = w / h;
	camera.updateProjectionMatrix();

}

new ResizeObserver( resize ).observe( canvas.parentElement );
renderer.setAnimationLoop( () => {

	controls.update();
	renderer.render( scene, camera );

} );

function frame( object ) {

	const box = new THREE.Box3().setFromObject( object );
	if ( box.isEmpty() ) return;
	const size = box.getSize( new THREE.Vector3() );
	const center = box.getCenter( new THREE.Vector3() );
	const radius = Math.max( size.length() / 2, 0.01 );
	const distance = radius / Math.sin( THREE.MathUtils.degToRad( camera.fov / 2 ) );
	controls.target.copy( center );
	camera.position.copy( center ).add( new THREE.Vector3( 0.6, 0.45, 0.8 ).normalize().multiplyScalar( distance ) );
	camera.near = distance / 100;
	camera.far = distance * 100;
	camera.updateProjectionMatrix();
	grid.position.y = box.min.y;
	grid.scale.setScalar( Math.max( 1, radius / 2 ) );

}

// -- loading ------------------------------------------------------------

const loader = new DWGLoader().preload();

/** What is shown: the DWG (bytes or URL), supplied textures, the object. */
let current = null;

function options() {

	const blocks = { auto: 'auto', true: true, false: false }[ $( 'blocks' ).value ];
	return {
		lines: $( 'lines' ).checked,
		planSymbols: $( 'planSymbols' ).checked,
		zUp: $( 'zUp' ).checked,
		units: $( 'units' ).value || null,
		blocks,
		timeout: Number( $( 'timeout' ).value ) || 0,
	};

}

function status( text ) {

	$( 'status' ).textContent = text;

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

async function show() {

	const { name, data, url, textures } = current;
	const opts = options();
	status( `Converting ${name}…` );
	$( 'errorSection' ).classList.add( 'hidden' );
	const t0 = performance.now();
	let object;
	try {

		if ( url ) {

			loader.setOptions( opts );
			object = await loader.loadAsync( url );

		} else {

			object = await loader.parseAsync( data, '', { ...opts, name, textures: textures ?? 'auto' } );

		}

	} catch ( e ) {

		console.warn( e );
		$( 'error' ).textContent = e.code ? `${e.code}: ${e.message}` : String( e );
		$( 'stderr' ).textContent = e.stderr ?? '';
		$( 'stderrBox' ).classList.toggle( 'hidden', ! e.stderr );
		$( 'errorSection' ).classList.remove( 'hidden' );
		status( `${name}: failed` );
		return;

	}

	if ( current.object ) {

		scene.remove( current.object );
		dispose( current.object );

	}

	current.object = object;
	scene.add( object );
	const info = object.userData.dwg;
	$( 'summary' ).textContent = info.summary + ( info.missingTextures.length ? `\nmissing texture files: ${info.missingTextures.join( ', ' )}` : '' );
	$( 'timings' ).textContent = `${Math.round( info.timings.convertMs )} ms + ${Math.round( info.timings.parseMs )} ms, ${Math.round( info.memoryBytes / 1048576 )} MB`;
	$( 'resultSection' ).classList.remove( 'hidden' );
	listBlocks( object );
	status( `${name} in ${Math.round( performance.now() - t0 )} ms` );

}

function listBlocks( object ) {

	const list = $( 'blockList' );
	list.replaceChildren();
	const series = object.userData.dwg.mode === 'series';
	$( 'blockSection' ).classList.toggle( 'hidden', ! series );
	if ( ! series ) {

		frame( object );
		return;

	}

	const select = ( child ) => {

		for ( const c of object.children ) c.visible = c === child;
		frame( child );

	};

	object.children.forEach( ( child, i ) => {

		const label = document.createElement( 'label' );
		const radio = Object.assign( document.createElement( 'input' ), { type: 'radio', name: 'block', checked: i === 0 } );
		radio.addEventListener( 'change', () => select( child ) );
		label.append( radio, child.name );
		list.append( label );

	} );
	select( object.children[ 0 ] );

}

async function openFiles( files ) {

	const dwg = files.find( ( f ) => /\.dwg$/i.test( f.name ) );
	if ( ! dwg ) {

		status( 'No .dwg file among the dropped files' );
		return;

	}

	const images = files.filter( ( f ) => f !== dwg );
	const textures = images.length ? {} : null;
	for ( const f of images ) textures[ f.name ] = await f.arrayBuffer();
	current = { name: dwg.name, data: await dwg.arrayBuffer(), textures, object: current?.object };
	show();

}

async function download() {

	const { name, url, textures } = current;
	const data = current.data ?? await ( await fetch( url ) ).arrayBuffer();
	const opts = options();
	const block = current.object.children.find( ( c ) => c.visible && c.userData.dwgBlock )?.userData.dwgBlock;
	const result = await loader.convert( data, { ...opts, name, textures: textures ?? false, only: block ? [ block ] : null } );
	const { name: blockName, glb } = result.blocks[ 0 ];
	const a = document.createElement( 'a' );
	a.href = URL.createObjectURL( new Blob( [ glb ], { type: 'model/gltf-binary' } ) );
	a.download = ( block ? blockName : name.replace( /\.dwg$/i, '' ) ) + '.glb';
	a.click();
	setTimeout( () => URL.revokeObjectURL( a.href ), 1000 );

}

// -- UI -----------------------------------------------------------------

const drop = $( 'drop' );
drop.addEventListener( 'dragover', ( e ) => {

	e.preventDefault();
	drop.classList.add( 'over' );

} );
drop.addEventListener( 'dragleave', () => drop.classList.remove( 'over' ) );
document.addEventListener( 'dragover', ( e ) => e.preventDefault() );
document.addEventListener( 'drop', ( e ) => {

	e.preventDefault();
	drop.classList.remove( 'over' );
	openFiles( [ ...e.dataTransfer.files ] );

} );
$( 'open' ).addEventListener( 'click', () => $( 'files' ).click() );
$( 'files' ).addEventListener( 'change', ( e ) => openFiles( [ ...e.target.files ] ) );
for ( const id of [ 'lines', 'planSymbols', 'zUp', 'units', 'blocks' ] ) {

	$( id ).addEventListener( 'change', () => current && show() );

}

$( 'download' ).addEventListener( 'click', () => download().catch( ( e ) => status( `Download failed: ${e.message}` ) ) );

const params = new URLSearchParams( location.search );
if ( params.has( 'timeout' ) ) $( 'timeout' ).value = params.get( 'timeout' );
if ( params.has( 'url' ) ) {

	const url = params.get( 'url' );
	current = { name: decodeURIComponent( url.split( '/' ).pop() ), url, object: null };
	show();

}

// for automated checks
window.demo = { loader, scene, get current() {

	return current;

} };
