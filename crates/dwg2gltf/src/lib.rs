// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Convert AutoCAD DWG drawings to glTF 2.0 (GLB), with support for the
//! OFML extension data in DWG files (UVs, normals, embedded materials), and
//! glTF/GLB, OBJ and STL meshes to DWG.
//! DWG decoding and encoding by LibreDWG (bundled).

pub mod acis;
mod compress;
pub mod convert;
pub mod dwg;
pub mod dwg_writer;
pub mod edges;
pub mod export;
pub mod files;
pub mod geom;
pub mod gltf;
pub mod input;
pub mod limits;
pub mod mapper;
pub mod mesh;
pub mod model;
pub mod ofml;

use std::collections::{HashMap, HashSet};
use std::fmt;
use std::path::{Path, PathBuf};

pub use convert::{Converter, Options, Stats};
pub use dwg::DwgReadError;
pub use dwg_writer::{DwgVersion, WriteError};
pub use export::{WriteOptions, WriteStats};
pub use gltf::GltfWriter;
pub use input::InputError;
pub use limits::Limits;
pub use model::Document;

#[derive(Debug)]
pub enum Error {
    Read(DwgReadError),
    NoModelSpace,
    /// `--seriesdwg` on a drawing without uninserted named blocks.
    NoSeriesBlocks,
    /// A non-finite number reached the glTF document (a bug).
    NonFinite,
    /// Nothing to show (no triangles, no lines) and
    /// [`Options::allow_empty`] not set; nothing was written.
    Empty(Box<Stats>),
    /// A glTF/OBJ/STL file that cannot be read.
    Input(InputError),
    Write(WriteError),
    Io(std::io::Error),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Error::Read(e) => e.fmt(f),
            Error::NoModelSpace => f.write_str("drawing has no *Model_Space block"),
            Error::NoSeriesBlocks => f.write_str(
                "not a series DWG: every named block is inserted somewhere or empty; \
                 convert it without --seriesdwg",
            ),
            Error::NonFinite => f.write_str("non-finite number in the glTF output"),
            Error::Empty(stats) => write!(
                f,
                "nothing to convert (no triangles, no lines), no file written; \
                 --allow-empty writes it anyway\n{stats}"
            ),
            Error::Input(e) => e.fmt(f),
            Error::Write(e) => e.fmt(f),
            Error::Io(e) => e.fmt(f),
        }
    }
}

impl std::error::Error for Error {}

impl From<DwgReadError> for Error {
    fn from(e: DwgReadError) -> Self {
        Error::Read(e)
    }
}

impl From<InputError> for Error {
    fn from(e: InputError) -> Self {
        Error::Input(e)
    }
}

impl From<WriteError> for Error {
    fn from(e: WriteError) -> Self {
        Error::Write(e)
    }
}

impl From<std::io::Error> for Error {
    fn from(e: std::io::Error) -> Self {
        Error::Io(e)
    }
}

/// Convert one DWG file to `dst` (.glb, or .gltf with an embedded buffer).
pub fn convert(src: &Path, dst: &Path, options: &Options) -> Result<Stats, Error> {
    let doc = Document::new(dwg::read(src)?, Some(src));
    write_document(&doc, dst, options)
}

/// [`convert`] for a decoded drawing.
pub fn write_document(doc: &Document, dst: &Path, options: &Options) -> Result<Stats, Error> {
    let (gltf, stats) = convert_document(doc, options)?;
    gltf.write(dst)?;
    Ok(stats)
}

/// [`convert`] in memory: a DWG file image to GLB bytes. `name` names the
/// drawing in error messages. Material images kept in separate files are
/// left out (see [`Document::with_texture_files`] to supply them).
pub fn convert_bytes(
    data: Vec<u8>,
    name: &str,
    options: &Options,
) -> Result<(Vec<u8>, Stats), Error> {
    let doc = Document::with_texture_files(dwg::read_bytes(data, name)?, Box::new(|_| None));
    let (gltf, stats) = convert_document(&doc, options)?;
    Ok((gltf.to_glb()?, stats))
}

/// Convert a decoded drawing's model space (compressed if `options` say so).
pub fn convert_document(doc: &Document, options: &Options) -> Result<(GltfWriter, Stats), Error> {
    let (mut gltf, stats) = Converter::new(doc, options.clone()).run()?;
    if stats.is_empty() && !options.allow_empty {
        return Err(Error::Empty(Box::new(stats)));
    }
    if options.compress {
        gltf.compress();
    }
    Ok((gltf, stats))
}

/// What [`series`] converted.
#[derive(Default, Debug)]
pub struct Series {
    /// The names of the blocks handed over, in order.
    pub written: Vec<String>,
    /// Blocks with nothing to show (only annotation or hidden geometry).
    pub empty: Vec<String>,
    /// Whether model space has content, which a series export leaves out.
    pub model_space: bool,
    /// All blocks together.
    pub stats: Stats,
}

impl fmt::Display for Series {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt_series(
            f,
            self.written.len(),
            &self.empty,
            &self.stats,
            self.model_space,
        )
    }
}

/// What [`write_series`] wrote.
#[derive(Default, Debug)]
pub struct SeriesStats {
    /// Block name and the file written for it.
    pub written: Vec<(String, PathBuf)>,
    /// Blocks with nothing to show (only annotation or hidden geometry).
    pub empty: Vec<String>,
    /// Whether model space has content, which a series export leaves out.
    pub model_space: bool,
    /// All blocks together.
    pub stats: Stats,
}

impl fmt::Display for SeriesStats {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        fmt_series(
            f,
            self.written.len(),
            &self.empty,
            &self.stats,
            self.model_space,
        )
    }
}

fn fmt_series(
    f: &mut fmt::Formatter,
    written: usize,
    empty: &[String],
    stats: &Stats,
    model_space: bool,
) -> fmt::Result {
    write!(f, "blocks: {written} written")?;
    if !empty.is_empty() {
        write!(
            f,
            ", {} without geometry ({})",
            empty.len(),
            empty.join(", ")
        )?;
    }
    write!(f, "\n{stats}")?;
    if model_space {
        write!(
            f,
            "\nwarning: model space has content too, which --seriesdwg does not export"
        )?;
    }
    Ok(())
}

/// Convert a series DWG: one file per block that nothing inserts (see
/// [`Document::series_blocks`]), named after the block, into `dir`.
pub fn convert_series(
    src: &Path,
    dir: &Path,
    ext: &str,
    options: &Options,
) -> Result<SeriesStats, Error> {
    let doc = Document::new(dwg::read(src)?, Some(src));
    write_series(&doc, dir, ext, options)
}

/// [`convert_series`] for a decoded drawing.
pub fn write_series(
    doc: &Document,
    dir: &Path,
    ext: &str,
    options: &Options,
) -> Result<SeriesStats, Error> {
    let blocks = doc.series_blocks();
    if blocks.is_empty() {
        return Err(Error::NoSeriesBlocks);
    }
    std::fs::create_dir_all(dir)?;
    let mut taken = HashSet::new();
    let mut paths = vec![];
    let out = series(doc, &blocks, options, |name, gltf| {
        let dst = dir.join(format!("{}.{ext}", files::unique_stem(name, &mut taken)));
        gltf.write(&dst)?;
        paths.push(dst);
        Ok(())
    });
    if matches!(out, Err(Error::Empty(_))) {
        // nothing written: no empty folder either
        let _ = std::fs::remove_dir(dir);
    }
    let out = out?;
    Ok(SeriesStats {
        written: out.written.into_iter().zip(paths).collect(),
        empty: out.empty,
        model_space: out.model_space,
        stats: out.stats,
    })
}

/// Convert `blocks` (block header indices, as [`Document::series_blocks`]
/// returns them) one at a time, each with its base point at the origin, and
/// hand each block's name and glTF to `each`. Blocks with nothing to show
/// go to [`Series::empty`] instead. The blocks share the file's units, so
/// whether the drawing is too small for them ([`convert::tiny_drawing`]) is
/// judged once, by the largest of all its series blocks, also those that
/// `blocks` leaves out.
pub fn series(
    doc: &Document,
    blocks: &[usize],
    options: &Options,
    mut each: impl FnMut(&str, GltfWriter) -> Result<(), Error>,
) -> Result<Series, Error> {
    let mut options = options.clone();
    let mut auto_warning = None;
    if options.auto_units
        && let Some((warning, true)) = tiny_series(doc, &options, &HashMap::new())
    {
        options.units = Some(6);
        auto_warning = Some(warning);
    }
    let mut out = Series {
        model_space: doc.model_space.is_some_and(|ms| doc.block_has_content(ms)),
        ..Series::default()
    };
    let mut measured = HashMap::new();
    for &block in blocks {
        let (gltf, stats) = Converter::new(doc, options.clone()).run_block(block);
        let name = match &doc.objects[block].data {
            dwg::Data::BlockHeader(b) => b.name.clone(),
            _ => panic!("series: object {block} is not a block header"),
        };
        out.stats.merge(stats);
        measured.insert(block, gltf.as_ref().and_then(convert::scene_size));
        let Some(mut gltf) = gltf else {
            out.empty.push(name);
            continue;
        };
        if options.compress {
            gltf.compress();
        }
        each(&name, gltf)?;
        out.written.push(name);
    }
    if let Some(warning) = auto_warning {
        out.stats.warnings.push(warning);
    } else if !options.auto_units
        && let Some((warning, _)) = tiny_series(doc, &options, &measured)
    {
        out.stats.warnings.push(warning);
    }
    if out.written.is_empty() && !options.allow_empty {
        return Err(Error::Empty(Box::new(out.stats)));
    }
    Ok(out)
}

/// [`convert::tiny_drawing`] for a series DWG, by the largest of its series
/// blocks. `measured`: the sizes of blocks already converted; the others are
/// converted to measure them, until one is 1 cm or more across.
fn tiny_series(
    doc: &Document,
    options: &Options,
    measured: &HashMap<usize, Option<f64>>,
) -> Option<(String, bool)> {
    // only for the units it second-guesses
    convert::tiny_drawing(doc, options, Some(0.0))?;
    let unmeasured = doc
        .series_blocks()
        .into_iter()
        .filter(|b| !measured.contains_key(b))
        .map(|b| {
            let (gltf, _) = Converter::new(doc, options.clone()).run_block(b);
            gltf.as_ref().and_then(convert::scene_size)
        });
    let mut largest: Option<f64> = None;
    // empty blocks have no size; a block 1 cm or more across ends it (and
    // the conversions of the blocks after it)
    for size in measured.values().copied().chain(unmeasured).flatten() {
        convert::tiny_drawing(doc, options, Some(size))?;
        largest = Some(largest.map_or(size, |m| m.max(size)));
    }
    convert::tiny_drawing(doc, options, largest)
}

/// Convert one glTF/GLB, OBJ or STL file to the DWG file `dst`.
pub fn convert_to_dwg(src: &Path, dst: &Path, options: &WriteOptions) -> Result<WriteStats, Error> {
    let scene = input::load(src)?;
    let (data, stats) = export::write(&scene, options)?;
    std::fs::write(dst, data)?;
    Ok(stats)
}
