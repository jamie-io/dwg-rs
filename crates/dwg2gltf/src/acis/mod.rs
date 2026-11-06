// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! ACIS solid model data (3DSOLID, BODY, REGION, surfaces): storage,
//! tokenizing, B-rep and tessellation. Needs no ACIS kernel.

pub mod acds;
pub mod brep;
pub mod nurbs;
pub mod sab;
pub mod tessellate;
