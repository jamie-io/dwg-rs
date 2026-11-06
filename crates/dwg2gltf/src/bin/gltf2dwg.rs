// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Command line entry point: gltf2dwg INPUT... [-o OUTPUT]

use std::path::PathBuf;
use std::process::ExitCode;

use clap::Parser;
use dwg2gltf::files::batch;
use dwg2gltf::input::EXTENSIONS;
use dwg2gltf::model::UNIT_NAMES;
use dwg2gltf::{DwgVersion, WriteOptions};

/// Convert glTF 2.0 (.glb/.gltf), OBJ and STL meshes to AutoCAD DWG, with
/// OFML material archives for the textures.
#[derive(Parser)]
#[command(version)]
struct Args {
    /// Mesh files or directories (searched recursively)
    #[arg(required = true)]
    inputs: Vec<PathBuf>,
    /// Output file (single input) or directory
    #[arg(short, long)]
    output: Option<PathBuf>,
    /// DWG version to write
    #[arg(long, default_value = "r2018", value_parser = DwgVersion::NAMES.map(|(name, _)| name))]
    dwg_version: String,
    /// Drawing units (INSUNITS); glTF meters are scaled to them
    #[arg(long, default_value = "m", value_parser = UNIT_NAMES.map(|(name, _)| name))]
    units: String,
    /// Units of OBJ and STL coordinates, which carry none (default: m)
    #[arg(long, value_parser = UNIT_NAMES.map(|(name, _)| name))]
    input_units: Option<String>,
    /// The input is Z-up already (glTF and OBJ are taken as Y-up)
    #[arg(long)]
    z_up: bool,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let lookup = |name: &str| UNIT_NAMES.iter().find(|(n, _)| *n == name).map(|&(_, v)| v);
    let options = WriteOptions {
        version: DwgVersion::NAMES
            .iter()
            .find(|(n, _)| *n == args.dwg_version)
            .map(|&(_, v)| v)
            .unwrap_or(DwgVersion::R2018),
        units: lookup(&args.units).unwrap_or(6),
        input_units: args.input_units.as_deref().and_then(lookup),
        z_up: args.z_up,
    };
    batch(
        &args.inputs,
        &EXTENSIONS,
        args.output.as_deref(),
        &["dwg"],
        "dwg",
        |src, dst| dwg2gltf::convert_to_dwg(src, dst, &options),
        |_| false,
    )
}
