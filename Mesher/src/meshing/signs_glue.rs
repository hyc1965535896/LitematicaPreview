//! Sign text glue: builds the glyph sprite for a preview and appends each
//! chunk's text quads to its mesh.
//!
//! Text is drawn as atlas geometry in the mesh's cutout layer, which the viewer
//! alpha-tests, so glyph masks land as crisp pixels. All glyphs share one sprite
//! packed into the shared atlas, so a chunk's text costs only the quads it shows.
//!
//! Placement uses the sign's own emitted geometry: the mesher renders signs
//! through an approximation, so the text is written on the face of whatever box
//! the block actually produced instead of on a hard-coded plane.

use std::collections::HashMap;

use schematic_mesher::{BlockPosition, MeshOutput, MeshLayer, ResourcePack, TextureAtlas};

use crate::fonts::{self, Cell, Sheet};
use crate::meshing::SignRef;
use crate::signs::Board;

/// Rasterises every glyph this preview's signs need into one sprite.
pub(super) fn build_sheet<'a>(
    signs: impl Iterator<Item = &'a str>,
    providers: &[crate::font_defs::BitmapProvider],
    pack: &ResourcePack,
) -> Option<Sheet> {
    let sources = fonts::Sources {
        packs: vec![(pack, providers.to_vec())],
    };
    sources.build(signs, 512)
}

/// Resolves the sheet's glyphs against the region the atlas gave the sprite.
pub(super) fn resolve(atlas: &TextureAtlas, sheet: &Sheet) -> HashMap<char, Cell> {
    match atlas.regions.get(fonts::ATLAS_KEY) {
        Some(region) => sheet.resolved(region),
        None => HashMap::new(),
    }
}

/// Appends the chunk's sign text to its mesh.
pub(super) fn append_text(
    signs: &[SignRef],
    cells: &HashMap<char, Cell>,
    mesh: &mut MeshOutput,
) {
    for sign in signs {
        let Some(board) = board_box(mesh, sign.position) else {
            continue;
        };
        // Font pixels: a sign's text is drawn at 1/96 block per pixel, and
        // four 10px lines must fit the board's face.
        let height = board.half_height * 2.0;
        let px = (height * 0.9 / 40.0).min(0.010_416_667 * 1.2);
        for (side, mirrored) in [(&sign.text.front, false), (&sign.text.back, true)] {
            let Some(side) = side else {
                continue;
            };
            for quad in crate::signs::quads(cells, side, sign.yaw, mirrored, board, px) {
                push_quad(&mut mesh.cutout, &quad);
            }
        }
    }
}

/// Where one sign's text goes, measured from the geometry the mesher emitted for
/// that block. The sign model is an approximation: a wall sign's board is a thin
/// slab flush with the block edge, a standing sign's board sits above its post.
/// The thin axis is the board's normal, its outer face is the plane to write on,
/// and the board's own vertical band (the wide part, not the post) centres the
/// text.
fn board_box(mesh: &MeshOutput, position: BlockPosition) -> Option<Board> {
    let center = [position.x as f32, position.y as f32, position.z as f32];
    let layers = std::iter::once(&mesh.opaque)
        .chain(mesh.greedy_materials.iter().map(|material| &material.opaque));
    let mut min = [f32::INFINITY; 3];
    let mut max = [f32::NEG_INFINITY; 3];
    let mut found = false;
    for layer in layers {
        for vertex in &layer.positions {
            if (vertex[0] - center[0]).abs() > 0.5 + 1e-3
                || (vertex[1] - center[1]).abs() > 0.5 + 1e-3
                || (vertex[2] - center[2]).abs() > 0.5 + 1e-3
            {
                continue;
            }
            found = true;
            for axis in 0..3 {
                min[axis] = min[axis].min(vertex[axis]);
                max[axis] = max[axis].max(vertex[axis]);
            }
        }
    }
    if !found {
        return None;
    }
    // The board is thin along its normal and wide across it.
    let normal_axis = if max[0] - min[0] <= max[2] - min[2] {
        0
    } else {
        2
    };
    let perp = 2 - normal_axis;
    let perp_center = (min[perp] + max[perp]) / 2.0;
    // A standing sign's post is narrow; only the wide part is text-bearing.
    let mut board_low = f32::INFINITY;
    let mut board_high = f32::NEG_INFINITY;
    let layers = std::iter::once(&mesh.opaque)
        .chain(mesh.greedy_materials.iter().map(|material| &material.opaque));
    for layer in layers {
        for vertex in &layer.positions {
            if (vertex[0] - center[0]).abs() > 0.5 + 1e-3
                || (vertex[1] - center[1]).abs() > 0.5 + 1e-3
                || (vertex[2] - center[2]).abs() > 0.5 + 1e-3
            {
                continue;
            }
            if (vertex[perp] - perp_center).abs() < 0.3 {
                continue;
            }
            board_low = board_low.min(vertex[1]);
            board_high = board_high.max(vertex[1]);
        }
    }
    // A board with no wide vertices (a degenerate model) falls back to the box.
    let (board_low, board_high) = if board_low.is_finite() && board_high > board_low {
        (board_low, board_high)
    } else {
        (min[1], max[1])
    };
    Some(Board {
        center: [
            center[0],
            (board_low + board_high) / 2.0,
            center[2],
        ],
        normal_axis,
        low: min[normal_axis],
        high: max[normal_axis],
        half_height: ((board_high - board_low) / 2.0).max(0.05),
    })
}

/// Pushes one text quad into a layer, tinting the white glyph mask with the
/// side's ink.
fn push_quad(layer: &mut MeshLayer, quad: &crate::signs::TextQuad) {
    let Some(first) = u32::try_from(layer.positions.len()).ok() else {
        return;
    };
    let edge = |a: [f32; 3], b: [f32; 3]| [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
    let u = edge(quad.positions[0], quad.positions[1]);
    let v = edge(quad.positions[0], quad.positions[3]);
    let mut normal = [
        u[1] * v[2] - u[2] * v[1],
        u[2] * v[0] - u[0] * v[2],
        u[0] * v[1] - u[1] * v[0],
    ];
    let length = (normal[0] * normal[0] + normal[1] * normal[1] + normal[2] * normal[2]).sqrt();
    if length > 0.0 {
        for value in &mut normal {
            *value /= length;
        }
    } else {
        normal = [0.0, 0.0, 1.0];
    }
    for corner in 0..4 {
        layer.positions.push(quad.positions[corner]);
        layer.normals.push(normal);
        layer.uvs.push(quad.uv[corner]);
        layer.colors.push(quad.color);
    }
    layer
        .indices
        .extend_from_slice(&[first, first + 1, first + 2, first, first + 2, first + 3]);
}
