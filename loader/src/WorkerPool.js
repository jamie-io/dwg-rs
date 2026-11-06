// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

// The workers of a DWGLoader or DWGExporter: dwg2gltf.wasm compiled once and
// handed to each worker; one task per worker; a worker that fails, cannot
// take a message or runs past its timeout is replaced.

import { compile, DWGError } from './core.js';

export function copyBuffer( data ) {

	if ( data instanceof ArrayBuffer ) return data.slice( 0 );
	if ( ArrayBuffer.isView( data ) ) return new Uint8Array( data.buffer, data.byteOffset, data.byteLength ).slice().buffer;
	throw new TypeError( 'three-dwg-loader: expected an ArrayBuffer or a typed array' );

}

export class WorkerPool {

	/**
	 * @param {{ wasmPath, workerURL, workerLimit }} settings read when needed
	 *   (the loader's or exporter's own fields)
	 * @param {string} owner names the owner in messages
	 */
	constructor( settings, owner ) {

		this.settings = settings;
		this.owner = owner;
		this._module = null; // Promise<WebAssembly.Module>
		this._workers = []; // { worker, task, ready }
		this._queue = [];
		this._nextId = 1;

	}

	/** Run `message` ({ type, ... }; `transfer`: its buffers) on a worker: the worker's result message. */
	run( message, transfer, timeout ) {

		return new Promise( ( resolve, reject ) => {

			this._queue.push( { message, transfer, timeout, resolve, reject } );
			this._pump();

		} );

	}

	/** The wasm path changed: compile again on the next task. */
	forgetModule() {

		this._module = null;

	}

	preload() {

		this._getModule().catch( () => {} );

	}

	/** Stop the workers; pending tasks fail. */
	dispose() {

		const error = () => new DWGError( 'worker', `${this.owner} disposed` );
		for ( const slot of this._workers ) {

			slot.worker.terminate();
			if ( slot.task ) this._settle( slot.task, null, error() );

		}

		for ( const task of this._queue ) task.reject( error() );
		this._workers = [];
		this._queue = [];
		this._module = null;

	}

	// -- internals --------------------------------------------------------

	_getModule() {

		if ( this._module === null ) {

			const path = this.settings.wasmPath;
			this._module = compile( path ).catch( ( e ) => {

				this._module = null; // try again next time
				throw e instanceof DWGError ? e : new DWGError( 'worker', `cannot load ${path}: ${e.message}`, { cause: e } );

			} );

		}

		return this._module;

	}

	_pump() {

		while ( this._queue.length > 0 ) {

			let slot = this._workers.find( ( w ) => w.task === null );
			if ( slot === undefined ) {

				if ( this._workers.length >= Math.max( 1, this.settings.workerLimit ) ) return;
				try {

					slot = this._spawn();

				} catch ( e ) {

					// e.g. a worker URL on another origin
					const message = `cannot start the DWG worker: ${e.message}`;
					this._queue.shift().reject( new DWGError( 'worker', message, { cause: e } ) );
					continue;

				}

			}

			this._start( slot, this._queue.shift() );

		}

	}

	_spawn() {

		const url = this.settings.workerURL;
		const worker = url === null
			? new Worker( new URL( './DWGWorker.js', import.meta.url ), { type: 'module' } )
			: new Worker( url, { type: 'module' } );
		const slot = { worker, task: null, ready: false };
		worker.onmessage = ( event ) => this._onMessage( slot, event.data );
		worker.onerror = ( event ) => {

			event.preventDefault();
			const detail = event.message || 'it could not be started (check setWorkerURL and the server)';
			this._drop( slot, new DWGError( 'worker', `the DWG worker failed: ${detail}` ) );

		};

		this._workers.push( slot );
		return slot;

	}

	_start( slot, task ) {

		slot.task = task;
		task.id = this._nextId ++;
		this._getModule().then( ( module ) => {

			if ( slot.task !== task ) return; // gone meanwhile
			try {

				if ( ! slot.ready ) {

					slot.worker.postMessage( { type: 'init', module } );
					slot.ready = true;

				}

				slot.worker.postMessage( { ...task.message, id: task.id }, task.transfer );

			} catch ( e ) {

				this._drop( slot, new DWGError( 'worker', `cannot pass the file to the DWG worker: ${e.message}`, { cause: e } ) );
				return;

			}

			if ( task.timeout > 0 ) {

				task.timer = setTimeout( () => {

					this._drop( slot, new DWGError( 'timeout', `the conversion took longer than ${task.timeout} ms` ) );

				}, task.timeout );

			}

		}, ( error ) => this._drop( slot, error ) );

	}

	_onMessage( slot, message ) {

		const task = slot.task;
		if ( task === null || message.id !== task.id ) return;
		slot.task = null;
		if ( message.type === 'result' ) {

			this._settle( task, message, null );

		} else {

			const { code, message: text, stderr, report } = message.error;
			this._settle( task, null, new DWGError( code, text, { stderr, report } ) );

		}

		this._pump();

	}

	/** Fail the worker's task and replace the worker (crashed or too slow). */
	_drop( slot, error ) {

		slot.worker.terminate();
		this._workers = this._workers.filter( ( w ) => w !== slot );
		if ( slot.task ) this._settle( slot.task, null, error );
		slot.task = null;
		this._pump();

	}

	_settle( task, result, error ) {

		clearTimeout( task.timer );
		if ( error ) task.reject( error );
		else task.resolve( result );

	}

}
