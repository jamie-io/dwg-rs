// SPDX-FileCopyrightText: 2026 Jamie Jahn
// SPDX-License-Identifier: GPL-3.0-or-later

//! Writes a small series DWG (block library) for the three.js loader's
//! tests: model space is empty; the articles "Chair A" and "Table B" are
//! blocks that nothing inserts ("Table B" inserts "Leg" four times), and
//! "Empty" has no content.
//!
//!     cargo run --example series_fixture -- series.dwg

use dwg2gltf::dwg_writer::{Block, DwgBuilder, DwgVersion, MeshData, Style};

/// An axis-aligned box from `lo` to `hi` (mm), faces wound outwards.
fn cuboid(b: &mut DwgBuilder, block: Block, lo: [f64; 3], hi: [f64; 3], style: Style) {
    let vertices: Vec<[f64; 3]> = (0..8)
        .map(|i| {
            [
                if i & 1 == 0 { lo[0] } else { hi[0] },
                if i & 2 == 0 { lo[1] } else { hi[1] },
                if i & 4 == 0 { lo[2] } else { hi[2] },
            ]
        })
        .collect();
    #[rustfmt::skip]
    let faces = [
        4, 0, 2, 3, 1, // bottom
        4, 4, 5, 7, 6, // top
        4, 0, 1, 5, 4, // front
        4, 2, 6, 7, 3, // back
        4, 0, 4, 6, 2, // left
        4, 1, 3, 7, 5, // right
    ];
    let mesh = MeshData {
        vertices: &vertices,
        faces: &faces,
        normals: None,
        uvs: None,
        double_sided: None,
    };
    b.mesh(block, &mesh, style).unwrap();
}

fn main() {
    let path = std::env::args()
        .nth(1)
        .expect("usage: series_fixture OUTPUT.dwg");
    let mut b = DwgBuilder::new(DwgVersion::R2018, 4).unwrap(); // mm
    let style = Style {
        layer: b.layer_zero(),
        material: None,
    };

    let leg = b.block("Leg").unwrap();
    cuboid(&mut b, leg, [0.0; 3], [40.0, 40.0, 700.0], style);
    b.end_block(leg).unwrap();

    let chair = b.block("Chair A").unwrap();
    cuboid(&mut b, chair, [0.0; 3], [450.0, 450.0, 450.0], style);
    cuboid(
        &mut b,
        chair,
        [0.0, 410.0, 450.0],
        [450.0, 450.0, 900.0],
        style,
    );
    b.end_block(chair).unwrap();

    let table = b.block("Table B").unwrap();
    cuboid(
        &mut b,
        table,
        [0.0, 0.0, 700.0],
        [1200.0, 800.0, 730.0],
        style,
    );
    for (x, y) in [(0.0, 0.0), (1160.0, 0.0), (0.0, 760.0), (1160.0, 760.0)] {
        let ins = [x, y, 0.0];
        b.insert(table, "Leg", ins, [1.0; 3], 0.0, [0.0, 0.0, 1.0], style)
            .unwrap();
    }
    b.end_block(table).unwrap();

    let empty = b.block("Empty").unwrap();
    b.end_block(empty).unwrap();

    std::fs::write(&path, b.encode().unwrap()).unwrap();
}
