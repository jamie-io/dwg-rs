// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

// Types for core.js.

/** The C ABI version this file speaks (dgl_abi_version). */
export const ABI: number;

export type DWGErrorCode =
	// reported by the converter
	| 'options' | 'read' | 'no-model-space' | 'no-series-blocks' | 'non-finite' | 'other'
	// reported by the export
	| 'input' | 'write'
	// the module trapped or exited
	| 'panic' | 'out-of-memory' | 'crash' | 'exit' | 'abi'
	// from the DWGLoader or DWGExporter
	| 'timeout' | 'worker' | 'gltf';

/** Entity counts by type, as in the command line tool's summary. */
export interface DWGStats {
	converted: Record<string, number>;
	skipped: Record<string, number>;
	repaired: Record<string, number>;
	warnings: string[];
	instances: number;
	triangles: number;
}

/** What the converter reports about one file. */
export interface DWGReport {
	abi: number;
	/** dwg2gltf's version. */
	version: string;
	mode: 'model' | 'series';
	/** The GLB files: byte ranges of the converter's output. */
	blocks: { name: string; offset: number; length: number }[];
	/** Series DWGs: articles without geometry, not converted. */
	empty: string[];
	modelSpaceContent: boolean;
	stats: DWGStats;
	/** The command line tool's text. */
	summary: string;
	/** Material image files that were not supplied. */
	missingTextures: string[];
}

/** A failed conversion or export. */
export class DWGError extends Error {
	constructor( code: DWGErrorCode, message: string, extra?: { stderr?: string; report?: DWGReport | null; cause?: unknown } );
	code: DWGErrorCode;
	/** The converter's output. */
	stderr: string;
	report: DWGReport | null;
}

/** Compile dwg2gltf.wasm from a URL, a fetch Response, bytes or a module. */
export function compile( source: string | URL | Response | BufferSource | WebAssembly.Module ): Promise<WebAssembly.Module>;

export interface CoreOptions {
	name?: string;
	lines?: boolean;
	planSymbols?: boolean;
	units?: string | null;
	zUp?: boolean;
	compress?: boolean;
	mode?: 'auto' | 'model' | 'series';
	only?: string[] | null;
}

/** Convert one DWG file in a fresh instance of `module`. */
export function convert(
	module: WebAssembly.Module,
	dwg: BufferSource,
	options?: CoreOptions,
	textures?: { name: string; data: BufferSource }[],
): Promise<{ report: DWGReport; glbs: { name: string; glb: ArrayBuffer }[]; stderr: string; memoryBytes: number }>;

/** What an export wrote: entity counts, as in gltf2dwg's summary. */
export interface DWGExportStats {
	meshes: number;
	faces: number;
	lines: number;
	blocks: number;
	inserts: number;
	materials: number;
	/** Materials with an image (OFML applications show it; AutoCAD shows the color). */
	textures: number;
	skipped: Record<string, number>;
	warnings: string[];
}

/** What the export reports about one file. */
export interface DWGExportReport {
	abi: number;
	/** dwg2gltf's version. */
	version: string;
	stats: DWGExportStats;
	/** gltf2dwg's text. */
	summary: string;
}

export interface ExportOptions {
	units?: string;
	version?: 'r2010' | 'r2013' | 'r2018';
}

/** Encode one GLB file as DWG in a fresh instance of `module`. */
export function exportDWG(
	module: WebAssembly.Module,
	glb: BufferSource,
	options?: ExportOptions,
): Promise<{ report: DWGExportReport; dwg: ArrayBuffer; stderr: string; memoryBytes: number }>;
