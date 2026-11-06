// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Input discovery and output naming for the command line tools.

use std::collections::{HashMap, HashSet};
use std::fmt::Display;
use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};
use std::process::ExitCode;

/// Exit code when every file converted except some with nothing to convert.
pub const EXIT_EMPTY: u8 = 3;

pub fn has_ext(p: &Path, exts: &[&str]) -> bool {
    p.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| exts.iter().any(|x| e.eq_ignore_ascii_case(x)))
}

/// Files as given, plus every file with one of `exts` below the directories
/// (recursively, sorted). Each comes with its directory relative to the
/// directory argument it was found in (empty for files given directly), so
/// outputs can mirror the input tree.
pub fn collect(inputs: &[PathBuf], exts: &[&str]) -> io::Result<Vec<(PathBuf, PathBuf)>> {
    let mut files = vec![];
    for p in inputs {
        if p.is_dir() {
            find(p, Path::new(""), exts, &mut files).map_err(|e| {
                io::Error::new(e.kind(), format!("cannot list {}: {e}", p.display()))
            })?;
        } else {
            files.push((p.clone(), PathBuf::new()));
        }
    }
    Ok(files)
}

fn find(
    dir: &Path,
    rel: &Path,
    exts: &[&str],
    out: &mut Vec<(PathBuf, PathBuf)>,
) -> io::Result<()> {
    let mut entries: Vec<PathBuf> = std::fs::read_dir(dir)?
        .map(|e| e.map(|e| e.path()))
        .collect::<Result<_, _>>()?;
    entries.sort();
    for p in entries {
        if p.is_dir() {
            let sub = rel.join(p.file_name().unwrap_or_default());
            find(&p, &sub, exts, out)?;
        } else if has_ext(&p, exts) {
            out.push((p, rel.to_path_buf()));
        }
    }
    Ok(())
}

/// `src`'s name with extension `ext`, in `dir` or next to `src`.
pub fn output_path(src: &Path, dir: Option<&Path>, ext: &str) -> PathBuf {
    let dir = dir.map_or_else(
        || src.parent().map(Path::to_path_buf).unwrap_or_default(),
        Path::to_path_buf,
    );
    // not with_extension: "a.1.dwg" must become "a.1.glb"
    let mut name = src.file_stem().unwrap_or_default().to_os_string();
    if !ext.is_empty() {
        name.push(format!(".{ext}"));
    }
    dir.join(name)
}

/// A file name (without extension) for `name` that is valid on Windows too
/// and not yet in `taken`, compared ignoring case; adds it to `taken`.
pub fn unique_stem(name: &str, taken: &mut HashSet<String>) -> String {
    let mut stem: String = name
        .chars()
        .map(|c| {
            if c.is_control() || r#"<>:"/\|?*"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect();
    // Windows drops trailing dots and spaces and reserves device names
    stem.truncate(stem.trim_end_matches(['.', ' ']).len());
    let device = stem.split('.').next().unwrap_or("").to_ascii_uppercase();
    if stem.is_empty()
        || ["CON", "PRN", "AUX", "NUL"].contains(&device.as_str())
        || (device.len() == 4
            && (device.starts_with("COM") || device.starts_with("LPT"))
            && device.as_bytes()[3].is_ascii_digit())
    {
        stem.push('_');
    }
    let mut unique = stem.clone();
    let mut n = 1;
    while !taken.insert(unique.to_lowercase()) {
        n += 1;
        unique = format!("{stem}~{n}");
    }
    unique
}

/// Convert every input (directories searched for `in_exts`), printing each
/// result and its summary. `-o` is a file when it has one of `out_exts`,
/// else a directory; outputs get extension `ext`. A file that fails, or
/// panics, is reported and the next one converted.
///
/// Exit code: 0 all converted, 1 a file failed, 2 bad arguments,
/// [`EXIT_EMPTY`] when the only failures are errors `empty` accepts
/// (nothing to convert).
pub fn batch<S: Display, E: Display>(
    inputs: &[PathBuf],
    in_exts: &[&str],
    output: Option<&Path>,
    out_exts: &[&str],
    ext: &str,
    convert: impl Fn(&Path, &Path) -> Result<S, E>,
    empty: impl Fn(&E) -> bool,
) -> ExitCode {
    let files = match collect(inputs, in_exts) {
        Ok(f) if !f.is_empty() => f,
        Ok(_) => {
            eprintln!("error: no input files found");
            return ExitCode::from(2);
        }
        Err(e) => {
            eprintln!("error: {e}");
            return ExitCode::from(2);
        }
    };
    let single_file_out = output.is_some_and(|o| has_ext(o, out_exts));
    if single_file_out && files.len() > 1 {
        eprintln!("error: -o names a file but several inputs were given; pass a directory");
        return ExitCode::from(2);
    }
    if let (Some(out), false) = (output, single_file_out) {
        if let Err(e) = std::fs::create_dir_all(out) {
            eprintln!("error: cannot create {}: {e}", out.display());
            return ExitCode::from(2);
        }
    }
    let mut failures = 0;
    let mut empties = 0;
    // which input each output was written for: two inputs must never share one
    let mut written: HashMap<PathBuf, &Path> = HashMap::new();
    for (src, rel) in &files {
        let dst = match output {
            Some(out) if single_file_out => out.to_path_buf(),
            Some(out) => output_path(src, Some(&out.join(rel)), ext),
            None => output_path(src, None, ext),
        };
        if let Some(first) = written.get(&dst) {
            failures += 1;
            eprintln!(
                "FAILED {}: {} was already written for {}; rename one of them",
                src.display(),
                dst.display(),
                first.display()
            );
            continue;
        }
        written.insert(dst.clone(), src);
        if let Some(parent) = dst.parent().filter(|p| !p.as_os_str().is_empty()) {
            if let Err(e) = std::fs::create_dir_all(parent) {
                failures += 1;
                eprintln!(
                    "FAILED {}: cannot create {}: {e}",
                    src.display(),
                    parent.display()
                );
                continue;
            }
        }
        // a bug (or a limit that is missing) must not end the whole run
        match catch_unwind(AssertUnwindSafe(|| convert(src, &dst))) {
            Ok(Ok(stats)) => {
                println!("{} -> {}", src.display(), dst.display());
                println!("  {}", stats.to_string().replace('\n', "\n  "));
            }
            Ok(Err(e)) => {
                if empty(&e) {
                    empties += 1;
                } else {
                    failures += 1;
                }
                eprintln!(
                    "FAILED {}: {}",
                    src.display(),
                    e.to_string().replace('\n', "\n  ")
                );
            }
            Err(panic) => {
                failures += 1;
                let msg = panic
                    .downcast_ref::<&str>()
                    .map(|s| s.to_string())
                    .or_else(|| panic.downcast_ref::<String>().cloned())
                    .unwrap_or_else(|| "unknown panic".into());
                eprintln!("FAILED {}: internal error: {msg}", src.display());
            }
        }
    }
    if failures > 0 {
        ExitCode::FAILURE
    } else if empties > 0 {
        ExitCode::from(EXIT_EMPTY)
    } else {
        ExitCode::SUCCESS
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_names() {
        assert_eq!(
            output_path(Path::new("d/a.1.dwg"), None, "glb"),
            Path::new("d/a.1.glb")
        );
        assert_eq!(
            output_path(Path::new("d/chair.glb"), Some(Path::new("out")), "dwg"),
            Path::new("out/chair.dwg")
        );
        // a directory per input (--seriesdwg)
        assert_eq!(
            output_path(Path::new("d/a.1.dwg"), None, ""),
            Path::new("d/a.1")
        );
    }

    #[test]
    fn block_file_names() {
        let mut taken = HashSet::new();
        let mut stem = |name| unique_stem(name, &mut taken);
        assert_eq!(stem("Chair A"), "Chair A");
        assert_eq!(stem("chair a"), "chair a~2");
        assert_eq!(stem("CHAIR A"), "CHAIR A~3");
        assert_eq!(stem("a/b:c*?"), "a_b_c__");
        assert_eq!(stem("end. "), "end");
        assert_eq!(stem("con"), "con_");
        assert_eq!(stem("Lpt1.x"), "Lpt1.x_");
        assert_eq!(stem("COMPANY"), "COMPANY");
        assert_eq!(stem(""), "_");
    }

    #[test]
    fn batch_mirrors_tree_and_refuses_collisions() {
        let root = std::env::temp_dir().join(format!("dwg2gltf-batch-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for f in ["in/a/model.gltf", "in/b/model.gltf", "in/b/model.stl"] {
            let p = root.join(f);
            std::fs::create_dir_all(p.parent().unwrap()).unwrap();
            std::fs::write(p, b"").unwrap();
        }
        let converted = std::cell::RefCell::new(vec![]);
        batch(
            &[root.join("in")],
            &["gltf", "stl"],
            Some(&root.join("out")),
            &["dwg"],
            "dwg",
            |src, dst| {
                std::fs::write(dst, b"x").unwrap();
                converted.borrow_mut().push(src.to_path_buf());
                Ok::<_, String>("ok")
            },
            |_| false,
        );
        assert!(root.join("out/a/model.dwg").exists());
        assert!(root.join("out/b/model.dwg").exists());
        // b/model.stl would overwrite b/model.dwg: refused, not converted
        assert_eq!(converted.borrow().len(), 2);
        std::fs::remove_dir_all(&root).unwrap();
    }

    /// One input per name in `names` (in a fresh directory), run through
    /// `batch` with `convert`; the exit code and the inputs attempted.
    fn run_batch(
        tag: &str,
        names: &[&str],
        convert: impl Fn(&str) -> Result<&'static str, String>,
    ) -> (String, Vec<String>) {
        let root = std::env::temp_dir().join(format!("dwg2gltf-{tag}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        std::fs::create_dir_all(&root).unwrap();
        for n in names {
            std::fs::write(root.join(format!("{n}.dwg")), b"").unwrap();
        }
        let tried = std::cell::RefCell::new(vec![]);
        let code = batch(
            std::slice::from_ref(&root),
            &["dwg"],
            Some(&root.join("out")),
            &[],
            "glb",
            |src, _| {
                let stem = src.file_stem().unwrap().to_string_lossy().into_owned();
                tried.borrow_mut().push(stem.clone());
                convert(&stem)
            },
            |e| e == "empty",
        );
        std::fs::remove_dir_all(&root).unwrap();
        (format!("{code:?}"), tried.into_inner())
    }

    #[test]
    fn batch_survives_a_panicking_file() {
        let (code, tried) = run_batch("panic", &["a", "b", "c"], |stem| {
            if stem == "b" {
                panic!("boom in {stem}");
            }
            Ok("ok")
        });
        assert_eq!(tried, ["a", "b", "c"]);
        assert_eq!(code, format!("{:?}", ExitCode::FAILURE));
    }

    #[test]
    fn batch_exit_codes() {
        let ok = format!("{:?}", ExitCode::SUCCESS);
        let empty = format!("{:?}", ExitCode::from(EXIT_EMPTY));
        let failed = format!("{:?}", ExitCode::FAILURE);
        assert_eq!(run_batch("ok", &["a"], |_| Ok("ok")).0, ok);
        let only_empty = |s: &str| match s {
            "a" => Err("empty".to_string()),
            _ => Ok("ok"),
        };
        assert_eq!(run_batch("empty", &["a", "b"], only_empty).0, empty);
        // a real failure wins over an empty file
        let both = |s: &str| match s {
            "a" => Err("empty".to_string()),
            _ => Err("broken".to_string()),
        };
        assert_eq!(run_batch("both", &["a", "b"], both).0, failed);
    }
}
