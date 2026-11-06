// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Resource limits against crafted or corrupt drawings.
//!
//! Each limit was measured with `examples/limit_probe.rs` (release build,
//! run under `systemd-run --user --scope -p MemoryMax=8G` and `timeout`):
//! the parameter was raised until the process was killed, overflowed its
//! stack or took minutes, then the limit set below that point. Where the
//! crash point depends on the machine's memory, the limits share a budget of
//! about 6 GB peak for one drawing (8 GB was the kill point when measuring);
//! two limits reached in the same drawing can add up. "Real" is the largest
//! value seen in real-world drawings.
//!
//! Going over a limit leaves the affected entities out with a `skipped:`
//! reason and a warning; it never aborts the conversion.

/// INSERTs nested deeper than this are left out ("INSERT (nesting too
/// deep)"). Converting recurses once per level: a chain of distinct blocks
/// overflowed the stack between 2 000 and 5 000 levels on a 2 MiB thread
/// and between 20 000 and 50 000 on the 8 MiB main thread (was 64 before
/// it was measured). Blocks inserting themselves are caught separately.
/// Real: 16.
pub const MAX_NESTING: usize = 1000;

/// Block instances (glTF nodes) per drawing, MINSERT cells included.
/// Measured: 2.45 KB per instance from nested INSERTs (2.1 M instances:
/// 5.1 GB peak, 4.2 M: killed at 8 GB), 5.8 KB per MINSERT cell (1 M:
/// 5.8 GB, 2 M: killed). Real: 5 708 instances, no MINSERT with more than
/// one cell.
pub const MAX_INSTANCES: usize = 1_000_000;

/// Triangles per drawing (all meshes and ACIS bodies, counted before
/// instancing). Measured: about 600 bytes and 4 us per triangle end to end
/// (8 M: 4.8 GB in 33 s, 12 M: 7.0 GB in 55 s, 16 M: killed at 8 GB).
/// Real: 2.2 M per drawing.
pub const MAX_TRIANGLES: usize = 10_000_000;

/// Polyline points per drawing (lines, arcs, curves). Measured: about
/// 82 bytes per point end to end (25.6 M points: 2.1 GB, 256 M: killed at
/// 8 GB, crash point about 95 M). Real: about 142 000.
pub const MAX_LINE_POINTS: usize = 60_000_000;

/// SPLINE degree. Sampling costs degree^3: degree 300 takes 0.25 s per
/// spline, 1000 takes 8.5 s, 2000 takes 82 s. AutoCAD allows at most 25;
/// real: 5.
pub const MAX_SPLINE_DEGREE: usize = 100;

/// Nesting of ACIS subtype blocks (`{ ... }`). Reading, walking and freeing
/// them recurses once per level: the stack overflowed between 20 000 and
/// 40 000 levels on a 2 MiB thread (cargo test's default) and between
/// 200 000 and 250 000 on the 8 MiB main thread. Real: 8.
pub const MAX_ACIS_NESTING: usize = 10_000;

/// Triangles per ACIS face before refinement stops (the face is then
/// reported as "refined to the triangle limit"). A crafted face reaching it
/// takes about 3.4 s (105 000 triangles).
pub const MAX_TRIANGLES_PER_FACE: usize = 100_000;

/// The per-drawing limits a conversion uses ([`crate::Options::limits`]);
/// the defaults are the constants above, tests use small ones.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Limits {
    pub instances: usize,
    pub triangles: usize,
    pub line_points: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            instances: MAX_INSTANCES,
            triangles: MAX_TRIANGLES,
            line_points: MAX_LINE_POINTS,
        }
    }
}
