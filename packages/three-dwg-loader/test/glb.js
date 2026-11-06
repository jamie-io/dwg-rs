// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

// Reading GLB files in the tests, without three.js.

import assert from 'node:assert/strict';

const SIZES = { SCALAR: 1, VEC2: 2, VEC3: 3, VEC4: 4, MAT4: 16 };

/** {json, bin} of a GLB file. */
export function parseGlb( data ) {

	const bytes = data instanceof Uint8Array ? data : new Uint8Array( data );
	const view = new DataView( bytes.buffer, bytes.byteOffset, bytes.byteLength );
	assert.equal( view.getUint32( 0, true ), 0x46546C67, 'GLB magic' );
	const jsonLength = view.getUint32( 12, true );
	const json = JSON.parse( new TextDecoder().decode( bytes.subarray( 20, 20 + jsonLength ) ) );
	const bin = bytes.subarray( 28 + jsonLength );
	return { json, bin };

}

/** The components of an accessor (no sparse, no normalization). */
export function accessorValues( { json, bin }, index ) {

	const accessor = json.accessors[ index ];
	const view = json.bufferViews[ accessor.bufferView ];
	const n = SIZES[ accessor.type ] * accessor.count;
	const offset = bin.byteOffset + ( view.byteOffset ?? 0 ) + ( accessor.byteOffset ?? 0 );
	const Type = { 5126: Float32Array, 5125: Uint32Array, 5123: Uint16Array, 5121: Uint8Array }[ accessor.componentType ];
	return new Type( bin.buffer.slice( offset, offset + n * Type.BYTES_PER_ELEMENT ) );

}

function multiply( a, b ) {

	const out = new Array( 16 ).fill( 0 );
	for ( let c = 0; c < 4; c ++ ) for ( let r = 0; r < 4; r ++ ) for ( let k = 0; k < 4; k ++ ) {

		out[ c * 4 + r ] += a[ k * 4 + r ] * b[ c * 4 + k ];

	}

	return out;

}

function localMatrix( node ) {

	if ( node.matrix ) return node.matrix;
	const [ x, y, z, w ] = node.rotation ?? [ 0, 0, 0, 1 ];
	const [ sx, sy, sz ] = node.scale ?? [ 1, 1, 1 ];
	const [ tx, ty, tz ] = node.translation ?? [ 0, 0, 0 ];
	return [
		( 1 - 2 * ( y * y + z * z ) ) * sx, 2 * ( x * y + z * w ) * sx, 2 * ( x * z - y * w ) * sx, 0,
		2 * ( x * y - z * w ) * sy, ( 1 - 2 * ( x * x + z * z ) ) * sy, 2 * ( y * z + x * w ) * sy, 0,
		2 * ( x * z + y * w ) * sz, 2 * ( y * z - x * w ) * sz, ( 1 - 2 * ( x * x + y * y ) ) * sz, 0,
		tx, ty, tz, 1,
	];

}

/** The world-space bounds {min, max} of every mesh in the default scene. */
export function bounds( glb ) {

	const { json } = glb;
	const min = [ Infinity, Infinity, Infinity ];
	const max = [ - Infinity, - Infinity, - Infinity ];
	const visit = ( index, parent ) => {

		const node = json.nodes[ index ];
		const world = multiply( parent, localMatrix( node ) );
		for ( const prim of node.mesh === undefined ? [] : json.meshes[ node.mesh ].primitives ) {

			const p = accessorValues( glb, prim.attributes.POSITION );
			for ( let i = 0; i < p.length; i += 3 ) {

				for ( let r = 0; r < 3; r ++ ) {

					const v = world[ r ] * p[ i ] + world[ 4 + r ] * p[ i + 1 ] + world[ 8 + r ] * p[ i + 2 ] + world[ 12 + r ];
					min[ r ] = Math.min( min[ r ], v );
					max[ r ] = Math.max( max[ r ], v );

				}

			}

		}

		for ( const child of node.children ?? [] ) visit( child, world );

	};

	const identity = [ 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1, 0, 0, 0, 0, 1 ];
	for ( const root of json.scenes?.[ json.scene ?? 0 ]?.nodes ?? [] ) visit( root, identity );
	return { min, max };

}

/** Primitive modes used (4 triangles, 1 lines). */
export function modes( { json } ) {

	return new Set( ( json.meshes ?? [] ).flatMap( ( m ) => m.primitives.map( ( p ) => p.mode ?? 4 ) ) );

}

/**
 * Assert two GLBs hold the same document: JSON equal up to `tolerance`
 * (relative) in numbers, integer accessors equal, float accessors within
 * `tolerance` times the largest magnitude in the accessor.
 */
export function assertSameGlb( actual, expected, tolerance, label ) {

	const a = parseGlb( actual );
	const e = parseGlb( expected );
	const close = ( x, y ) => Math.abs( x - y ) <= tolerance * Math.max( 1, Math.abs( x ), Math.abs( y ) );
	const walk = ( x, y, path ) => {

		if ( typeof x === 'number' && typeof y === 'number' ) {

			assert.ok( close( x, y ), `${label}: ${path} ${x} != ${y}` );

		} else if ( x && typeof x === 'object' ) {

			assert.deepEqual( Object.keys( x ), Object.keys( y ?? {} ), `${label}: keys of ${path}` );
			for ( const k of Object.keys( x ) ) walk( x[ k ], y[ k ], `${path}.${k}` );

		} else {

			assert.equal( x, y, `${label}: ${path}` );

		}

	};

	walk( a.json, e.json, 'json' );
	assert.equal( a.bin.length, e.bin.length, `${label}: binary chunk length` );
	for ( let i = 0; i < ( a.json.accessors ?? [] ).length; i ++ ) {

		const x = accessorValues( a, i );
		const y = accessorValues( e, i );
		if ( x instanceof Float32Array ) {

			const scale = y.reduce( ( m, v ) => Math.max( m, Math.abs( v ) ), 1 );
			for ( let k = 0; k < x.length; k ++ ) {

				if ( Math.abs( x[ k ] - y[ k ] ) > tolerance * scale ) {

					assert.fail( `${label}: accessor ${i}[${k}] ${x[ k ]} != ${y[ k ]}` );

				}

			}

		} else {

			assert.deepEqual( x, y, `${label}: accessor ${i}` );

		}

	}

}

/** A 1x1 PNG. */
const PNG = Uint8Array.from( atob( 'iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=' ), ( c ) => c.charCodeAt( 0 ) );

/** A GLB file of a box (12 triangles, `size` in meters, centred), optionally with a texture. */
export function boxGlb( { size = [ 1, 2, 3 ], texture = false } = {} ) {

	const [ x, y, z ] = size.map( ( s ) => s / 2 );
	const positions = new Float32Array( [
		- x, - y, - z, x, - y, - z, x, y, - z, - x, y, - z,
		- x, - y, z, x, - y, z, x, y, z, - x, y, z,
	] );
	const uvs = new Float32Array( [ 0, 0, 1, 0, 1, 1, 0, 1, 0, 0, 1, 0, 1, 1, 0, 1 ] );
	const indices = new Uint16Array( [
		0, 2, 1, 0, 3, 2, 4, 5, 6, 4, 6, 7, 0, 1, 5, 0, 5, 4,
		3, 6, 2, 3, 7, 6, 0, 4, 7, 0, 7, 3, 1, 2, 6, 1, 6, 5,
	] );
	const parts = [ positions, uvs, indices, ...( texture ? [ PNG ] : [] ) ];
	const views = [];
	let offset = 0;
	for ( const part of parts ) {

		views.push( { buffer: 0, byteOffset: offset, byteLength: part.byteLength } );
		offset += Math.ceil( part.byteLength / 4 ) * 4;

	}

	const bin = new Uint8Array( offset );
	parts.forEach( ( part, i ) => bin.set( new Uint8Array( part.buffer, part.byteOffset, part.byteLength ), views[ i ].byteOffset ) );
	const material = texture
		? { pbrMetallicRoughness: { baseColorTexture: { index: 0 } } }
		: { pbrMetallicRoughness: { baseColorFactor: [ 0.8, 0.2, 0.2, 1 ] } };
	const json = {
		asset: { version: '2.0' },
		scene: 0,
		scenes: [ { nodes: [ 0 ] } ],
		nodes: [ { mesh: 0, name: 'box' } ],
		meshes: [ { primitives: [ { attributes: { POSITION: 0, TEXCOORD_0: 1 }, indices: 2, material: 0 } ] } ],
		materials: [ { name: 'red', ...material } ],
		accessors: [
			{ bufferView: 0, componentType: 5126, count: 8, type: 'VEC3', min: [ - x, - y, - z ], max: [ x, y, z ] },
			{ bufferView: 1, componentType: 5126, count: 8, type: 'VEC2' },
			{ bufferView: 2, componentType: 5123, count: 36, type: 'SCALAR' },
		],
		bufferViews: views,
		buffers: [ { byteLength: offset } ],
		...( texture ? { images: [ { bufferView: 3, mimeType: 'image/png' } ], textures: [ { source: 0 } ] } : {} ),
	};
	let text = JSON.stringify( json );
	while ( text.length % 4 ) text += ' ';
	const jsonBytes = new TextEncoder().encode( text );
	const glb = new Uint8Array( 12 + 8 + jsonBytes.length + 8 + bin.length );
	const view = new DataView( glb.buffer );
	view.setUint32( 0, 0x46546C67, true );
	view.setUint32( 4, 2, true );
	view.setUint32( 8, glb.length, true );
	view.setUint32( 12, jsonBytes.length, true );
	view.setUint32( 16, 0x4E4F534A, true );
	glb.set( jsonBytes, 20 );
	view.setUint32( 20 + jsonBytes.length, bin.length, true );
	view.setUint32( 24 + jsonBytes.length, 0x004E4942, true );
	glb.set( bin, 28 + jsonBytes.length );
	return glb;

}
