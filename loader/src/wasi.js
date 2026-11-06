// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

// The part of WASI (wasi_snapshot_preview1) dwg2gltf.wasm imports: no files,
// no arguments, no environment; what it writes to stdout and stderr is kept
// as text (panic messages, LibreDWG diagnostics).

const ESUCCESS = 0;
const EBADF = 8;
const ENOSYS = 52;
const ESPIPE = 70;
const FILETYPE_CHARACTER_DEVICE = 2;
/** Output kept per stream; LibreDWG can be chatty on broken files. */
const MAX_TEXT = 64 * 1024;

/** The module called proc_exit. */
export class WasiExit extends Error {
	constructor( code ) {
		super( `exit code ${code}` );
		this.name = 'WasiExit';
		this.code = code;
	}
}

/**
 * Imports for one instance. Call `setMemory(instance.exports.memory)` before
 * the first export; `stdout()` and `stderr()` return what was written.
 * `random(bytes)` fills a Uint8Array (default: crypto.getRandomValues).
 */
export function createWasi( { random = ( bytes ) => crypto.getRandomValues( bytes ) } = {} ) {

	let memory = null;
	const text = [ '', '', '' ];
	const decoders = [ null, new TextDecoder(), new TextDecoder() ];
	// memory.buffer is replaced when memory grows: never keep a view.
	// Pointers arrive as signed i32 (>>> 0 makes them unsigned).
	const view = () => new DataView( memory.buffer );

	const wasi = {

		args_get: () => ESUCCESS,
		args_sizes_get: ( argc, size ) => {

			view().setUint32( argc >>> 0, 0, true );
			view().setUint32( size >>> 0, 0, true );
			return ESUCCESS;

		},
		environ_get: () => ESUCCESS,
		environ_sizes_get: ( count, size ) => {

			view().setUint32( count >>> 0, 0, true );
			view().setUint32( size >>> 0, 0, true );
			return ESUCCESS;

		},
		clock_time_get: ( id, precision, time ) => {

			const ms = id === 0 ? Date.now() : performance.now(); // realtime, else monotonic
			view().setBigUint64( time >>> 0, BigInt( Math.round( ms * 1e6 ) ), true );
			return ESUCCESS;

		},
		random_get: ( ptr, len ) => {

			// getRandomValues fills at most 64 KiB at a time
			for ( let at = 0; at < len; at += 65536 ) {

				random( new Uint8Array( memory.buffer, ( ptr >>> 0 ) + at, Math.min( 65536, len - at ) ) );

			}

			return ESUCCESS;

		},
		fd_write: ( fd, iovs, iovsLen, written ) => {

			if ( fd !== 1 && fd !== 2 ) return EBADF;
			const v = view();
			let total = 0;
			for ( let i = 0; i < iovsLen; i ++ ) {

				const ptr = v.getUint32( ( iovs >>> 0 ) + i * 8, true );
				const len = v.getUint32( ( iovs >>> 0 ) + i * 8 + 4, true );
				if ( text[ fd ].length < MAX_TEXT ) {

					text[ fd ] += decoders[ fd ].decode( new Uint8Array( memory.buffer, ptr, len ), { stream: true } );

				}

				total += len;

			}

			v.setUint32( written >>> 0, total, true );
			return ESUCCESS;

		},
		fd_read: ( fd, iovs, iovsLen, read ) => {

			if ( fd !== 0 ) return EBADF;
			view().setUint32( read >>> 0, 0, true ); // stdin is empty
			return ESUCCESS;

		},
		fd_fdstat_get: ( fd, stat ) => {

			if ( fd > 2 ) return EBADF;
			const v = view();
			stat >>>= 0;
			v.setUint8( stat, FILETYPE_CHARACTER_DEVICE );
			v.setUint16( stat + 2, 0, true );
			v.setBigUint64( stat + 8, 0n, true );
			v.setBigUint64( stat + 16, 0n, true );
			return ESUCCESS;

		},
		fd_seek: ( fd ) => fd <= 2 ? ESPIPE : EBADF,
		fd_close: ( fd ) => fd <= 2 ? ESUCCESS : EBADF,
		fd_prestat_get: () => EBADF, // no preopened directories
		fd_prestat_dir_name: () => EBADF,
		proc_exit: ( code ) => {

			throw new WasiExit( code );

		},
		sched_yield: () => ESUCCESS,

	};

	// anything else (a newer build importing more) fails at the call, not
	// at instantiation
	const imports = new Proxy( wasi, {
		get: ( target, name ) => target[ name ] ?? ( () => ENOSYS ),
	} );

	return {
		imports: { wasi_snapshot_preview1: imports },
		setMemory( m ) {

			memory = m;

		},
		stdout: () => text[ 1 ],
		stderr: () => text[ 2 ],
	};

}
