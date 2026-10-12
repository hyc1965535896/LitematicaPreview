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
        let height = board.half[1] * 2.0;
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

/// Where one sign's text goes: the block's own centre for the horizontal axes,
/// the board's height above it, and the face the mesher actually emitted for the
/// depth. The sign model keeps its board in the upper half of the block — the
/// post fills the rest — so the text is centred on that board, and four 10px
/// lines are sized to fit roughly half a block.
fn board_box(mesh: &MeshOutput, position: BlockPosition) -> Option<Board> {
    // The board's depth comes from the geometry: the mesher's sign model is an
    // approximation, so the text is written just outside whatever face exists.
    let mut half_depth = 0.0f32;
    let mut found = false;
    let layers = std::iter::once(&mesh.opaque)
        .chain(mesh.greedy_materials.iter().map(|material| &material.opaque));
    for layer in layers {
        for vertex in &layer.positions {
            let inside_xz = (vertex[0] - position.x as f32).abs() <= 0.5 + 1e-3
                && (vertex[2] - position.z as f32).abs() <= 0.5 + 1e-3
                && (vertex[1] - position.y as f32).abs() <= 0.5 + 1e-3;
            if !inside_xz {
                continue;
            }
            found = true;
            half_depth = half_depth.max((vertex[2] - position.z as f32).abs());
        }
    }
    if !found {
        return None;
    }
    Some(Board {
        center: [
            position.x as f32,
            // The board sits above the block's middle; its centre is about a
            // third of a block up, which is where vanilla centres sign text.
            position.y as f32 + 1.0 / 3.0,
            position.z as f32,
        ],
        half: [0.5, 0.25, half_depth],
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
