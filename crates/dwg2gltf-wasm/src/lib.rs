// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! dwg2gltf as a WebAssembly module (wasm32-wasip1) for the three.js
//! DWGLoader and DWGExporter: `packages/three-dwg-loader/src/core.js` drives
//! this C ABI. [`dgl_convert`] turns DWG into GLB, [`dgl_export`] GLB into
//! DWG (what `gltf2dwg` does).
//!
//! Buffers: the caller gets one from [`dgl_alloc`], fills it and hands it
//! to [`dgl_add_texture`], [`dgl_convert`] or [`dgl_export`], which take it
//! over ([`dgl_free`] is for one never handed over). The report and output
//! stay in the module until the next conversion or [`dgl_reset`]. A panic or a
//! LibreDWG crash traps; the caller then drops the instance, and LibreDWG's
//! static state with it.

use std::cell::RefCell;
use std::collections::{BTreeSet, HashMap};
use std::rc::Rc;

use dwg2gltf::model::{UNIT_NAMES, basename};
use dwg2gltf::{Document, DwgVersion, Error, Options, Stats, WriteOptions, WriteStats, dwg};
use serde_json::{Map, Value, json};

/// Changes when an export or the report changes incompatibly.
const ABI: u32 = 2;

/// Status codes of [`dgl_convert`] and [`dgl_export`], and their names in
/// the report.
const OK: u32 = 0;
const OPTIONS: u32 = 1;
const READ: u32 = 2;
const NO_MODEL_SPACE: u32 = 3;
const NO_SERIES_BLOCKS: u32 = 4;
const NON_FINITE: u32 = 5;
const OTHER: u32 = 6;
/// [`dgl_export`]: the GLB was refused, or LibreDWG could not encode it.
const INPUT: u32 = 7;
const WRITE: u32 = 8;
const CODES: [&str; 9] = [
    "ok",
    "options",
    "read",
    "no-model-space",
    "no-series-blocks",
    "non-finite",
    "other",
    "input",
    "write",
];

#[derive(Default)]
struct State {
    /// Material image files for the next conversion, by lowercase name.
    textures: HashMap<String, Vec<u8>>,
    /// The GLB files of the last conversion, one after the other, or the
    /// DWG file of the last export.
    output: Vec<u8>,
    /// The last conversion's report (JSON).
    report: Vec<u8>,
}

thread_local! {
    static STATE: RefCell<State> = RefCell::default();
}

#[unsafe(no_mangle)]
pub extern "C" fn dgl_abi_version() -> u32 {
    ABI
}

/// A buffer for `len` bytes (plus the 0 byte LibreDWG appends to a file).
#[unsafe(no_mangle)]
pub extern "C" fn dgl_alloc(len: usize) -> *mut u8 {
    std::mem::ManuallyDrop::new(Vec::<u8>::with_capacity(len + 1)).as_mut_ptr()
}

/// Release a buffer from [`dgl_alloc`] that was not handed over.
///
/// # Safety
/// `ptr` comes from `dgl_alloc(len)`.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dgl_free(ptr: *mut u8, len: usize) {
    drop(unsafe { Vec::from_raw_parts(ptr, 0, len + 1) });
}

/// A buffer from `dgl_alloc(len)`, filled.
unsafe fn take(ptr: *mut u8, len: usize) -> Vec<u8> {
    unsafe { Vec::from_raw_parts(ptr, len, len + 1) }
}

/// Supply a material image file (`name` UTF-8, matched by file name without
/// case) for the next [`dgl_convert`]. Takes both buffers.
///
/// # Safety
/// Both buffers come from [`dgl_alloc`] with these lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dgl_add_texture(
    name_ptr: *mut u8,
    name_len: usize,
    data_ptr: *mut u8,
    data_len: usize,
) {
    let name = unsafe { take(name_ptr, name_len) };
    let data = unsafe { take(data_ptr, data_len) };
    let key = basename(&String::from_utf8_lossy(&name)).to_lowercase();
    STATE.with_borrow_mut(|s| s.textures.insert(key, data));
}

/// Convert a DWG file image with the options in `opts` (UTF-8 JSON, see
/// [`Request::parse`]). Takes both buffers and the supplied textures.
/// Returns a status code; the report (`dgl_report_*`) tells more, and the
/// output (`dgl_output_*`) holds the GLB files the report lists.
///
/// # Safety
/// Both buffers come from [`dgl_alloc`] with these lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dgl_convert(
    dwg_ptr: *mut u8,
    dwg_len: usize,
    opts_ptr: *mut u8,
    opts_len: usize,
) -> u32 {
    let data = unsafe { take(dwg_ptr, dwg_len) };
    let opts = unsafe { take(opts_ptr, opts_len) };
    let textures = STATE.with_borrow_mut(|s| {
        s.output = Vec::new();
        s.report = Vec::new();
        std::mem::take(&mut s.textures)
    });
    finish(Request::parse(&opts).and_then(|req| convert(data, &req, textures)))
}

/// Encode a GLB file image as DWG (what `gltf2dwg` does) with the options in
/// `opts` (UTF-8 JSON, see [`parse_write_options`]). Takes both buffers;
/// supplied textures are dropped (a GLB embeds its images). Returns a status
/// code; the output holds the DWG file.
///
/// # Safety
/// Both buffers come from [`dgl_alloc`] with these lengths.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn dgl_export(
    glb_ptr: *mut u8,
    glb_len: usize,
    opts_ptr: *mut u8,
    opts_len: usize,
) -> u32 {
    let data = unsafe { take(glb_ptr, glb_len) };
    let opts = unsafe { take(opts_ptr, opts_len) };
    STATE.with_borrow_mut(|s| *s = State::default());
    finish(parse_write_options(&opts).and_then(|options| export(&data, &options)))
}

/// Keep a conversion's report and output for `dgl_report_*`/`dgl_output_*`.
fn finish(result: Result<(Value, Vec<u8>), Failure>) -> u32 {
    let (status, report, output) = match result {
        Ok((report, output)) => (OK, report, output),
        Err((status, message)) => {
            let code = CODES[status as usize];
            let report = json!({"abi": ABI, "error": {"code": code, "message": message}});
            (status, report, Vec::new())
        }
    };
    STATE.with_borrow_mut(|s| {
        s.report = serde_json::to_vec(&report).expect("JSON values serialize");
        s.output = output;
    });
    status
}

#[unsafe(no_mangle)]
pub extern "C" fn dgl_report_ptr() -> *const u8 {
    STATE.with_borrow(|s| s.report.as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn dgl_report_len() -> usize {
    STATE.with_borrow(|s| s.report.len())
}

#[unsafe(no_mangle)]
pub extern "C" fn dgl_output_ptr() -> *const u8 {
    STATE.with_borrow(|s| s.output.as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn dgl_output_len() -> usize {
    STATE.with_borrow(|s| s.output.len())
}

/// Drop the output, the report and supplied textures.
#[unsafe(no_mangle)]
pub extern "C" fn dgl_reset() {
    STATE.with_borrow_mut(|s| *s = State::default());
}

enum Mode {
    Model,
    Series,
    Auto,
}

struct Request {
    name: String,
    options: Options,
    mode: Mode,
    only: Option<Vec<String>>,
}

type Failure = (u32, String);

impl Request {
    /// `{"name": "chair.dwg", "lines": true, "planSymbols": false,
    /// "units": null | "mm" | ... | "auto", "zUp": false, "compress": false,
    /// "mode": "auto" | "model" | "series", "only": null | [block names]}`,
    /// every key optional (these are the defaults; `name` is for messages).
    fn parse(json: &[u8]) -> Result<Request, Failure> {
        let bad = |message: String| (OPTIONS, message);
        let map = json_object(json)?;
        let mut req = Request {
            name: "drawing.dwg".into(),
            // a drawing with nothing to show loads as an empty scene
            options: Options {
                allow_empty: true,
                ..Options::default()
            },
            mode: Mode::Auto,
            only: None,
        };
        for (key, v) in &map {
            let boolean = || {
                v.as_bool()
                    .ok_or_else(|| bad(format!("option {key} must be true or false")))
            };
            match key.as_str() {
                "name" => {
                    req.name = v
                        .as_str()
                        .ok_or_else(|| bad("option name must be a string".into()))?
                        .into()
                }
                "lines" => req.options.lines = boolean()?,
                "planSymbols" => req.options.plan_symbols = boolean()?,
                "zUp" => req.options.y_up = !boolean()?,
                "compress" => req.options.compress = boolean()?,
                "units" => {
                    (req.options.units, req.options.auto_units) = match v {
                        Value::Null => (None, false),
                        Value::String(u) if u == "auto" => (None, true),
                        Value::String(u) => (Some(unit_code(u)?), false),
                        _ => return Err(bad("option units must be a unit name or null".into())),
                    }
                }
                "mode" => {
                    req.mode = match v.as_str() {
                        Some("model") => Mode::Model,
                        Some("series") => Mode::Series,
                        Some("auto") => Mode::Auto,
                        _ => {
                            return Err(bad(
                                "option mode must be \"model\", \"series\" or \"auto\"".into(),
                            ));
                        }
                    }
                }
                "only" => {
                    req.only = match v {
                        Value::Null => None,
                        Value::Array(names) => Some(
                            names
                                .iter()
                                .map(|n| n.as_str().map(String::from))
                                .collect::<Option<_>>()
                                .ok_or_else(|| {
                                    bad("option only must be a list of block names".into())
                                })?,
                        ),
                        _ => return Err(bad("option only must be a list of block names".into())),
                    }
                }
                _ => return Err(bad(format!("unknown option {key:?}"))),
            }
        }
        Ok(req)
    }
}

fn json_object(json: &[u8]) -> Result<Map<String, Value>, Failure> {
    let value: Value = serde_json::from_slice(json)
        .map_err(|e| (OPTIONS, format!("options are not valid JSON: {e}")))?;
    match value {
        Value::Object(map) => Ok(map),
        _ => Err((OPTIONS, "options must be a JSON object".into())),
    }
}

fn unit_code(name: &str) -> Result<i32, Failure> {
    UNIT_NAMES
        .iter()
        .find(|(n, _)| *n == name)
        .map(|&(_, code)| code)
        .ok_or_else(|| (OPTIONS, format!("unknown units {name:?}")))
}

/// `{"units": "m" | "mm" | ..., "version": "r2018" | "r2013" | "r2010"}`,
/// both optional (these are the defaults, as in `gltf2dwg`).
fn parse_write_options(json: &[u8]) -> Result<WriteOptions, Failure> {
    let mut options = WriteOptions::default();
    for (key, v) in &json_object(json)? {
        match key.as_str() {
            "units" => {
                let name = v
                    .as_str()
                    .ok_or_else(|| (OPTIONS, "option units must be a unit name".to_string()))?;
                options.units = unit_code(name)?;
            }
            "version" => {
                options.version = DwgVersion::NAMES
                    .iter()
                    .find(|(n, _)| Some(*n) == v.as_str())
                    .map(|&(_, version)| version)
                    .ok_or_else(|| {
                        (
                            OPTIONS,
                            "option version must be \"r2010\", \"r2013\" or \"r2018\"".to_string(),
                        )
                    })?;
            }
            _ => return Err((OPTIONS, format!("unknown option {key:?}"))),
        }
    }
    Ok(options)
}

fn failure(e: Error) -> Failure {
    let status = match e {
        Error::Read(_) => READ,
        Error::NoModelSpace => NO_MODEL_SPACE,
        Error::NoSeriesBlocks => NO_SERIES_BLOCKS,
        Error::NonFinite => NON_FINITE,
        _ => OTHER,
    };
    (status, e.to_string())
}

fn stats_json(s: &Stats) -> Value {
    json!({
        "converted": s.converted,
        "skipped": s.skipped,
        "repaired": s.repaired,
        "warnings": s.warnings,
        "instances": s.instances,
        "triangles": s.triangles,
    })
}

fn write_stats_json(s: &WriteStats) -> Value {
    json!({
        "meshes": s.meshes,
        "faces": s.faces,
        "lines": s.lines,
        "blocks": s.blocks,
        "inserts": s.inserts,
        "materials": s.materials,
        "textures": s.textures,
        "skipped": s.skipped,
        "warnings": s.warnings,
    })
}

fn block_name(doc: &Document, block: usize) -> String {
    match &doc.objects[block].data {
        dwg::Data::BlockHeader(b) => b.name.clone(),
        _ => String::new(),
    }
}

/// The report and the GLB files of one conversion.
fn convert(
    data: Vec<u8>,
    req: &Request,
    textures: HashMap<String, Vec<u8>>,
) -> Result<(Value, Vec<u8>), Failure> {
    let drawing = dwg::read_bytes(data, &req.name).map_err(|e| (READ, e.to_string()))?;
    convert_drawing(drawing, req, textures)
}

fn convert_drawing(
    drawing: dwg::Drawing,
    req: &Request,
    textures: HashMap<String, Vec<u8>>,
) -> Result<(Value, Vec<u8>), Failure> {
    let missing = Rc::new(RefCell::new(BTreeSet::new()));
    let lookup = {
        let missing = missing.clone();
        Box::new(move |filename: &str| {
            let name = basename(filename);
            let found = textures.get(&name.to_lowercase()).cloned();
            if found.is_none() {
                missing.borrow_mut().insert(name);
            }
            found
        })
    };
    let doc = Document::with_texture_files(drawing, lookup);
    let series = match req.mode {
        Mode::Model => false,
        Mode::Series => true,
        Mode::Auto => doc.is_series(),
    };
    let mut output = Vec::new();
    let mut blocks = Vec::new();
    let mut report = Map::new();
    report.insert("abi".into(), json!(ABI));
    report.insert("version".into(), json!(env!("CARGO_PKG_VERSION")));
    let model_space = doc.model_space.is_some_and(|ms| doc.block_has_content(ms));
    report.insert("modelSpaceContent".into(), json!(model_space));
    if series {
        let mut chosen = doc.series_blocks();
        if chosen.is_empty() {
            return Err((
                NO_SERIES_BLOCKS,
                "not a series DWG: every named block is inserted somewhere or empty".into(),
            ));
        }
        if let Some(only) = &req.only {
            let names: Vec<String> = chosen.iter().map(|&b| block_name(&doc, b)).collect();
            if let Some(unknown) = only.iter().find(|n| !names.contains(n)) {
                return Err((
                    OPTIONS,
                    format!("only: {unknown:?} is not a block of this series DWG"),
                ));
            }
            chosen.retain(|&b| only.contains(&block_name(&doc, b)));
        }
        let done = dwg2gltf::series(&doc, &chosen, &req.options, |name, gltf| {
            let glb = gltf.to_glb()?;
            blocks.push(json!({"name": name, "offset": output.len(), "length": glb.len()}));
            output.extend_from_slice(&glb);
            Ok(())
        })
        .map_err(failure)?;
        report.insert("mode".into(), json!("series"));
        report.insert("blocks".into(), json!(blocks));
        report.insert("empty".into(), json!(done.empty));
        report.insert("stats".into(), stats_json(&done.stats));
        report.insert("summary".into(), json!(done.to_string()));
    } else {
        if req.only.is_some() {
            return Err((OPTIONS, "option only applies to series DWGs".into()));
        }
        let (gltf, stats) = dwg2gltf::convert_document(&doc, &req.options).map_err(failure)?;
        output = gltf.to_glb().map_err(failure)?;
        blocks.push(json!({"name": "*Model_Space", "offset": 0, "length": output.len()}));
        report.insert("mode".into(), json!("model"));
        report.insert("blocks".into(), json!(blocks));
        report.insert("empty".into(), json!([]));
        report.insert("stats".into(), stats_json(&stats));
        report.insert("summary".into(), json!(stats.to_string()));
    }
    let missing: Vec<String> = missing.borrow().iter().cloned().collect();
    report.insert("missingTextures".into(), json!(missing));
    Ok((Value::Object(report), output))
}

/// The report and the DWG file of one export.
fn export(glb: &[u8], options: &WriteOptions) -> Result<(Value, Vec<u8>), Failure> {
    let scene = dwg2gltf::input::gltf::parse(glb, None).map_err(|e| (INPUT, e.to_string()))?;
    let (dwg, stats) =
        dwg2gltf::export::write(&scene, options).map_err(|e| (WRITE, e.to_string()))?;
    let report = json!({
        "abi": ABI,
        "version": env!("CARGO_PKG_VERSION"),
        "stats": write_stats_json(&stats),
        "summary": stats.to_string(),
    });
    Ok((report, dwg))
}

#[cfg(test)]
mod tests {
    use super::*;
    use dwg2gltf::dwg::{
        BlockHeader, Color, Data, Drawing, Entity, Layer, Material, Object, Shape,
    };

    fn object(handle: u64, kind: &str, data: Data) -> Object {
        Object {
            handle,
            kind: kind.into(),
            owner: 0,
            xdic: 0,
            data,
        }
    }

    /// A face in model space with a MATERIAL whose image is a separate file.
    fn textured_face() -> Drawing {
        let face = Entity {
            entmode: 0,
            invisible: false,
            layer: 0x10,
            color: Color {
                index: Some(256),
                rgb: None,
            },
            material_flags: 3,
            material_refs: [0x20, 0],
            material_mapper: None,
            has_ds_data: false,
            thickness: 0.0,
            extrusion: Some([0.0, 0.0, 1.0]),
            shape: Shape::Face3d([
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
            ]),
        };
        let layer = Layer {
            name: "0".into(),
            color: Color {
                index: Some(256),
                rgb: Some(0xC3000007),
            },
            material: 0,
            off: false,
            frozen: false,
        };
        let material = Material {
            name: "wood".into(),
            diffuse: None,
            opacity: 1.0,
            transmatrix: None,
            filename: r"C:\textures\Wood.PNG".into(),
        };
        let model = BlockHeader {
            name: "*Model_Space".into(),
            base_pt: [0.0; 3],
            entities: Some(vec![0x30]),
            xref: None,
        };
        Drawing {
            decode_error: 0,
            insunits: 6,
            objects: vec![
                object(0x10, "LAYER", Data::Layer(layer)),
                object(0x20, "MATERIAL", Data::Material(material)),
                object(0x11, "BLOCK_HEADER", Data::BlockHeader(model)),
                object(0x30, "3DFACE", Data::Entity(Box::new(face))),
            ],
            acds: vec![],
        }
    }

    fn images(glb: &[u8]) -> Vec<Value> {
        let len = u32::from_le_bytes(glb[12..16].try_into().unwrap()) as usize;
        let json: Value = serde_json::from_slice(&glb[20..20 + len]).unwrap();
        json["images"].as_array().cloned().unwrap_or_default()
    }

    #[test]
    fn supplied_textures_match_by_file_name() {
        let req = Request::parse(b"{}").unwrap();
        let textures = HashMap::from([("wood.png".to_string(), b"\x89PNG".to_vec())]);
        let (report, glb) = convert_drawing(textured_face(), &req, textures).unwrap();
        assert_eq!(images(&glb).len(), 1);
        assert_eq!(report["missingTextures"], json!([]));

        let (report, glb) = convert_drawing(textured_face(), &req, HashMap::new()).unwrap();
        assert!(images(&glb).is_empty());
        assert_eq!(report["missingTextures"], json!(["Wood.PNG"]));
    }

    #[test]
    fn options_are_checked() {
        for (json, message) in [
            (&b"[]"[..], "must be a JSON object"),
            (b"{\"units\": \"parsec\"}", "unknown units"),
            (b"{\"lines\": 1}", "lines must be true or false"),
            (b"{\"colour\": true}", "unknown option"),
            (b"{\"mode\": \"blocks\"}", "option mode"),
        ] {
            let err = Request::parse(json).err().expect("an error");
            assert_eq!(err.0, OPTIONS);
            assert!(err.1.contains(message), "{}", err.1);
        }
        let req = Request::parse(br#"{"units": "cm", "zUp": true, "planSymbols": true}"#).unwrap();
        assert_eq!(req.options.units, Some(5));
        assert!(!req.options.auto_units);
        assert!(!req.options.y_up && req.options.plan_symbols);
        let req = Request::parse(br#"{"units": "auto"}"#).unwrap();
        assert_eq!((req.options.units, req.options.auto_units), (None, true));
        let req = Request::parse(br#"{"units": null}"#).unwrap();
        assert_eq!((req.options.units, req.options.auto_units), (None, false));
    }

    #[test]
    fn exports_a_glb_as_dwg() {
        let req = Request::parse(b"{}").unwrap();
        let (_, glb) = convert_drawing(textured_face(), &req, HashMap::new()).unwrap();
        let options = parse_write_options(b"{}").unwrap();
        let (report, dwg) = export(&glb, &options).unwrap();
        assert_eq!(&dwg[..6], b"AC1032");
        assert_eq!(report["stats"]["meshes"], json!(1));
        assert!(
            report["summary"]
                .as_str()
                .unwrap()
                .starts_with("written: MESH=1")
        );

        let options = parse_write_options(br#"{"version": "r2010", "units": "mm"}"#).unwrap();
        let (_, dwg) = export(&glb, &options).unwrap();
        assert_eq!(&dwg[..6], b"AC1024");
    }

    #[test]
    fn export_refuses_what_is_not_a_glb() {
        let options = parse_write_options(b"{}").unwrap();
        for bad in [&b""[..], b"glTF", b"not a glb at all, just text"] {
            let err = export(bad, &options).expect_err("an error");
            assert_eq!(err.0, INPUT, "{}", err.1);
        }
    }

    #[test]
    fn export_options_are_checked() {
        for (json, message) in [
            (&b"[]"[..], "must be a JSON object"),
            (b"{\"units\": \"parsec\"}", "unknown units"),
            (b"{\"units\": null}", "units must be a unit name"),
            (b"{\"units\": \"auto\"}", "unknown units"),
            (b"{\"version\": \"r14\"}", "version must be"),
            (b"{\"lines\": true}", "unknown option"),
        ] {
            let err = parse_write_options(json).expect_err("an error");
            assert_eq!(err.0, OPTIONS);
            assert!(err.1.contains(message), "{}", err.1);
        }
        let o = parse_write_options(br#"{"units": "cm", "version": "r2013"}"#).unwrap();
        assert_eq!(o.units, 5);
        assert_eq!(o.version, DwgVersion::R2013);
    }
}
