// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

// The DWGLoader's and DWGExporter's module worker: one task at a time, each
// in a fresh instance of the compiled module the pool sends first.
//
//   -> { type: 'init', module }
//   -> { type: 'convert', id, dwg, options, textures }
//   <- { type: 'result', id, report, glbs, stderr, convertMs, memoryBytes }
//   -> { type: 'export', id, glb, options }
//   <- { type: 'result', id, report, dwg, stderr, convertMs, memoryBytes }
//   <- { type: 'error', id, error: { code, message, stderr, report } }

import { convert, DWGError, exportDWG } from './core.js';

const scope = self;
let module = null;

/** Answer task `id` with what `work` resolves to: [result fields, buffers to transfer]. */
async function answer( id, work ) {

	const t0 = performance.now();
	try {

		const [ result, transfer ] = await work();
		scope.postMessage( { type: 'result', id, ...result, convertMs: performance.now() - t0 }, transfer );

	} catch ( e ) {

		const error = e instanceof DWGError
			? { code: e.code, message: e.message, stderr: e.stderr, report: e.report }
			: { code: 'worker', message: String( e?.message ?? e ), stderr: '', report: null };
		scope.postMessage( { type: 'error', id, error } );

	}

}

scope.onmessage = ( event ) => {

	const message = event.data;
	if ( message.type === 'init' ) {

		module = message.module;

	} else if ( message.type === 'convert' ) {

		const { id, dwg, options, textures } = message;
		return answer( id, async () => {

			const result = await convert( module, dwg, options, textures );
			return [ result, result.glbs.map( ( g ) => g.glb ) ];

		} );

	} else if ( message.type === 'export' ) {

		const { id, glb, options } = message;
		return answer( id, async () => {

			const result = await exportDWG( module, glb, options );
			return [ result, [ result.dwg ] ];

		} );

	}

};
