// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Raw bindings to the vendored LibreDWG 0.14 decoder (see the crate's README.md).

#![allow(
    non_upper_case_globals,
    non_camel_case_types,
    non_snake_case,
    unsafe_op_in_unsafe_fn
)]
// bindgen's allow(unnecessary_transmutes) is unknown before Rust 1.88
#![allow(unknown_lints)]
#![allow(clippy::all)]

#[cfg(target_pointer_width = "64")]
include!("bindings.rs");
#[cfg(target_arch = "wasm32")]
include!("bindings_wasm32.rs");

use std::sync::{Mutex, MutexGuard, PoisonError};

/// LibreDWG keeps decoder and encoder state in static variables (loop
/// counters, the current version), so two threads must not use it at the
/// same time. Hold this guard across every call, including `dwg_free`.
pub fn lock() -> MutexGuard<'static, ()> {
    static LOCK: Mutex<()> = Mutex::new(());
    LOCK.lock().unwrap_or_else(PoisonError::into_inner)
}
