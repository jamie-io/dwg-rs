// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Command line entry point: dwg2gltf INPUT... [-o OUTPUT]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::builder::PossibleValuesParser;
use clap::{Parser, ValueEnum};
use dwg2gltf::Options;
use dwg2gltf::files::{batch, has_ext};
use dwg2gltf::model::UNIT_NAMES;

#[derive(Clone, Copy, ValueEnum)]
enum Format {
    Glb,
    Gltf,
}

/// Convert AutoCAD DWG files to glTF 2.0 (.glb/.gltf).
#[derive(Parser)]
#[command(version)]
struct Args {
    /// DWG files or directories (searched recursively)
    #[arg(required = true)]
    inputs: Vec<PathBuf>,
    /// Output file (single input) or directory
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// Output format when -o is not a file
    #[arg(long, value_enum, default_value = "glb")]
    format: Format,
    /// Drop wire geometry (lines, arcs, polylines)
    #[arg(long)]
    no_lines: bool,
    /// Keep OFML 2D plan symbols (outlines on "..._D2_..." layers),
    /// which are skipped by default
    #[arg(long)]
    plan_symbols: bool,
    /// Override drawing units (INSUNITS). auto: the drawing's units, but
    /// meters when it would be less than 1 cm across in millimeters,
    /// centimeters, decimeters, inches, feet or yards (OFML geometry stored
    /// in meters)
    #[arg(long, value_parser = PossibleValuesParser::new(UNIT_NAMES.map(|(name, _)| name).into_iter().chain(["auto"])))]
    units: Option<String>,
    /// Keep DWG Z-up axes instead of glTF Y-up
    #[arg(long)]
    z_up: bool,
    /// Compress: quantized, meshopt-coded geometry and WebP textures (for
    /// three.js, Babylon.js, model-viewer; not Blender 4.2)
    #[arg(long)]
    compress: bool,
    /// Series DWG (block library): write one file per block that nothing
    /// inserts, into a folder named after the drawing
    #[arg(long, visible_alias = "blocks")]
    seriesdwg: bool,
    /// Write the output even when nothing could be converted (by default
    /// that is an error and exit code 3)
    #[arg(long)]
    allow_empty: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let options = Options {
        lines: !args.no_lines,
        plan_symbols: args.plan_symbols,
        units: args
            .units
            .as_deref()
            .and_then(|u| UNIT_NAMES.iter().find(|(name, _)| *name == u))
            .map(|&(_, v)| v),
        auto_units: args.units.as_deref() == Some("auto"),
        y_up: !args.z_up,
        compress: args.compress,
        allow_empty: args.allow_empty,
        ..Options::default()
    };
    let ext = match args.format {
        Format::Glb => "glb",
        Format::Gltf => "gltf",
    };
    if args.seriesdwg {
        if args
            .output
            .as_deref()
            .is_some_and(|o| has_ext(o, &["glb", "gltf"]))
        {
            eprintln!("error: --seriesdwg writes a folder per drawing; -o must be a directory");
            return ExitCode::from(2);
        }
        return batch(
            &args.inputs,
            &["dwg"],
            args.output.as_deref(),
            &[],
            "",
            |src, dir| dwg2gltf::convert_series(src, dir, ext, &options),
            is_empty,
        );
    }
    batch(
        &args.inputs,
        &["dwg"],
        args.output.as_deref(),
        &["glb", "gltf"],
        ext,
        |src, dst| dwg2gltf::convert(src, dst, &options),
        is_empty,
    )
}

fn is_empty(e: &dwg2gltf::Error) -> bool {
    matches!(e, dwg2gltf::Error::Empty(_))
}
