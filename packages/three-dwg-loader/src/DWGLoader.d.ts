// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

// Types for DWGLoader.js.

import { Group, Loader, LoadingManager } from 'three';
import { GLTFLoader } from 'three/addons/loaders/GLTFLoader.js';

import { DWGReport, DWGStats } from './core.js';

export { DWGError, DWGErrorCode, DWGReport, DWGStats } from './core.js';

export type DWGUnits = 'mm' | 'cm' | 'dm' | 'm' | 'km' | 'in' | 'ft' | 'yd' | 'mi';

export interface DWGLoaderOptions {
	/** Wire geometry (lines, arcs, polylines). Default true. */
	lines?: boolean;
	/** OFML 2D plan symbols. Default false. */
	planSymbols?: boolean;
	/** Override the drawing's units. Default null (from the file). 'auto': the
	 * file's, but meters when the drawing would be less than 1 cm across in
	 * millimeters, centimeters, decimeters, inches, feet or yards (OFML
	 * geometry stored in meters). */
	units?: DWGUnits | 'auto' | null;
	/** Keep the DWG's Z-up axes. Default false. */
	zUp?: boolean;
	/** Series DWGs: 'auto' (default), true (one object per article), false (model space). */
	blocks?: 'auto' | boolean;
	/** Series DWGs: convert only these articles (block names). */
	only?: string[] | null;
	/** Material image files: 'auto' (default) fetches them next to the DWG, an object supplies them, false skips them. */
	textures?: 'auto' | false | Record<string, ArrayBuffer | ArrayBufferView>;
	/** Quantized, meshopt-compressed GLBs. Default false. */
	compress?: boolean;
	/** Milliseconds before a conversion is abandoned (0, the default: no limit). */
	timeout?: number;
}

/** `object.userData.dwg` of a loaded drawing. */
export interface DWGInfo {
	mode: 'model' | 'series';
	summary: string;
	stats: DWGStats;
	warnings: string[];
	/** The converted blocks: '*Model_Space', or the articles of a series DWG. */
	blocks: string[];
	empty: string[];
	modelSpaceContent: boolean;
	missingTextures: string[];
	timings: { convertMs: number; parseMs: number };
	memoryBytes: number;
}

export interface DWGConversion {
	/** One GLB file per block. */
	blocks: { name: string; glb: ArrayBuffer }[];
	report: DWGReport;
	stderr: string;
	convertMs: number;
	memoryBytes: number;
}

export class DWGLoader extends Loader<Group> {
	constructor( manager?: LoadingManager );

	wasmPath: string | URL | Response | BufferSource | WebAssembly.Module;
	workerURL: string | URL | null;
	workerLimit: number;
	options: Required<DWGLoaderOptions>;

	/** Where dwg2gltf.wasm is (a URL, or the bytes, or a compiled module). */
	setWasmPath( path: string | URL | Response | BufferSource | WebAssembly.Module ): this;
	/** Where DWGWorker.js is, when a bundler moves it. Must be same-origin. */
	setWorkerURL( url: string | URL ): this;
	setWorkerLimit( limit: number ): this;
	setOptions( options: DWGLoaderOptions ): this;
	setGLTFLoader( gltfLoader: GLTFLoader ): this;

	load(
		url: string,
		onLoad: ( object: Group ) => void,
		onProgress?: ( event: ProgressEvent ) => void,
		onError?: ( error: unknown ) => void,
	): void;
	parse( data: ArrayBuffer | ArrayBufferView, path: string, onLoad: ( object: Group ) => void, onError?: ( error: unknown ) => void ): void;
	/** The object for a DWG file; `options.name` (the file name) names it. */
	parseAsync( data: ArrayBuffer | ArrayBufferView, path?: string, options?: DWGLoaderOptions & { name?: string } ): Promise<Group>;
	/** The GLB files, without building three.js objects. `path`: where textures: 'auto' looks. */
	convert( data: ArrayBuffer | ArrayBufferView, options?: DWGLoaderOptions & { name?: string; path?: string } ): Promise<DWGConversion>;
	/** Compile the module now instead of on the first file. */
	preload(): this;
	/** Stop the workers; pending conversions fail. */
	dispose(): void;
}
