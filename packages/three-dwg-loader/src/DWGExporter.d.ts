// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

// Types for DWGExporter.js.

import { Object3D } from 'three';

import { DWGExportReport } from './core.js';
import { DWGUnits } from './DWGLoader.js';

export { DWGError, DWGErrorCode, DWGExportReport, DWGExportStats } from './core.js';

export interface DWGExporterOptions {
	/** Drawing units; three.js meters are scaled to them. Default 'mm'. */
	units?: DWGUnits;
	/** DWG version. Default 'r2018'. */
	version?: 'r2010' | 'r2013' | 'r2018';
	/** Milliseconds before an export is abandoned (0, the default: no limit). */
	timeout?: number;
}

export interface DWGExport {
	dwg: ArrayBuffer;
	report: DWGExportReport;
	stderr: string;
	convertMs: number;
	memoryBytes: number;
}

export class DWGExporter {
	constructor();

	wasmPath: string | URL | Response | BufferSource | WebAssembly.Module;
	workerURL: string | URL | null;
	workerLimit: number;
	options: Required<DWGExporterOptions>;

	setWasmPath( path: string | URL | Response | BufferSource | WebAssembly.Module ): this;
	setWorkerURL( url: string | URL ): this;
	setOptions( options: DWGExporterOptions ): this;
	/** The DWG file of `object`. */
	parseAsync( object: Object3D, options?: DWGExporterOptions ): Promise<ArrayBuffer>;
	/** Encode a GLB file. */
	convert( glb: ArrayBuffer | ArrayBufferView, options?: DWGExporterOptions ): Promise<DWGExport>;
	preload(): this;
	dispose(): this;
}
