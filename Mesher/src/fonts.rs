//! Sign text glyphs.
//!
//! Glyphs come from two places, in Minecraft's own order of preference:
//!
//! * the loaded resource packs' bitmap font providers (`font/*.json` with their
//!   page textures), so a pack that reskins text is respected;
//! * the embedded GNU Unifont table — the same font the vanilla client ships as
//!   `unifont.zip` — which covers CJK and everything else a sign may hold.
//!
//! Only the glyphs a schematic actually uses are rasterised, into one extra
//! sprite appended to the mesh atlas, so a Latin-only build pays almost nothing
//! for the CJK coverage.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use schematic_mesher::atlas::AtlasRegion;
use schematic_mesher::resource_pack::TextureData;
use schematic_mesher::ResourcePack;

use crate::font_defs::BitmapProvider;

/// Side of one glyph cell in the text sprite. Uniform cells keep the sprite
/// small and its UVs trivial; 16px is unifont's native glyph size.
pub(super) const CELL: u32 = 16;
/// Atlas key of the text sprite. Double underscores cannot appear in a
/// resource location, so no pack texture can collide with it.
pub(super) const ATLAS_KEY: &str = "__sign_text__";
/// Missing glyphs render as this, so an absent glyph is visible rather than invisible.
const REPLACEMENT: char = '\u{fffd}';

/// Where one character's glyph lives in the text sprite, and how it lays out.
#[derive(Clone, Copy)]
pub(super) struct Cell {
    pub u0: f32,
    pub v0: f32,
    pub u1: f32,
    pub v1: f32,
    /// Ink box in font pixels: quads keep the glyph's own aspect ratio.
    pub ink_width: f32,
    pub ink_height: f32,
    /// Advance in font pixels.
    pub advance: f32,
    /// Cell height in font pixels (8 for pack pages, 16 for unifont).
    pub cell_height: f32,
}

/// Everything a schematic's sign text needs.
pub(super) struct Sheet {
    pub texture: TextureData,
    pub cells: HashMap<char, Cell>,
}

impl Sheet {
    /// Resolves every glyph's UV against the atlas region the sprite was packed
    /// into. Cell UVs are relative to the sprite, so scaling by the region's
    /// extent keeps them exact regardless of where the packer placed it.
    pub(super) fn resolved(&self, region: &AtlasRegion) -> HashMap<char, Cell> {
        let mut cells = HashMap::with_capacity(self.cells.len());
        for (character, cell) in &self.cells {
            cells.insert(
                *character,
                Cell {
                    u0: region.u_min + cell.u0 * (region.u_max - region.u_min),
                    v0: region.v_min + cell.v0 * (region.v_max - region.v_min),
                    u1: region.u_min + cell.u1 * (region.u_max - region.u_min),
                    v1: region.v_min + cell.v1 * (region.v_max - region.v_min),
                    ..*cell
                },
            );
        }
        cells
    }
}

/// The atlas region the text sprite is packed into maps a sheet cell to atlas
/// UVs. Cell UVs are relative to the sprite, so they scale by the region's
/// extent — the packing only has to keep the sprite's aspect intact.
pub(super) fn resolve_cells(sheet: &Sheet, region: &AtlasRegion) -> HashMap<char, Cell> {
    let mut cells = HashMap::with_capacity(sheet.cells.len());
    for (character, cell) in &sheet.cells {
        cells.insert(
            *character,
            Cell {
                u0: region.u_min + cell.u0 * (region.u_max - region.u_min),
                v0: region.v_min + cell.v0 * (region.v_max - region.v_min),
                u1: region.u_min + cell.u1 * (region.u_max - region.u_min),
                v1: region.v_min + cell.v1 * (region.v_max - region.v_min),
                ..*cell
            },
        );
    }
    cells
}

/// The glyph sources for one preview: the loaded packs' providers first, then
/// the bundled pack's, then unifont.
pub(super) struct Sources<'a> {
    pub packs: Vec<(&'a ResourcePack, Vec<BitmapProvider>)>,
}

impl Sources<'_> {
    /// Rasterises every glyph the given strings need into one text sprite.
    pub(super) fn build<'a>(
        &self,
        texts: impl Iterator<Item = &'a str>,
        max_size: u32,
    ) -> Option<Sheet> {
        let mut order: Vec<char> = Vec::new();
        let mut wanted = HashSet::new();
        for text in texts {
            for character in text.chars() {
                if character.is_control() || !wanted.insert(character) {
                    continue;
                }
                order.push(character);
            }
        }
        if order.is_empty() {
            return None;
        }
        if wanted.insert(REPLACEMENT) {
            order.push(REPLACEMENT);
        }

        let columns = (max_size / CELL).clamp(1, 64);
        // Prefer the characters that appear first; a pathological schematic with
        // tens of thousands of distinct glyphs still renders the common ones.
        order.truncate((columns * (max_size / CELL)) as usize);
        let rows = order.len().div_ceil(columns as usize) as u32;
        let width = columns * CELL;
        let height = (rows * CELL).max(CELL);
        let mut pixels = vec![0u8; (width * height * 4) as usize];

        let mut cells = HashMap::new();
        for (index, character) in order.iter().enumerate() {
            let column = index as u32 % columns;
            let row = index as u32 / columns;
            let Some(glyph) = self.lookup(*character).or_else(|| self.lookup(REPLACEMENT)) else {
                continue;
            };
            let origin_x = column * CELL;
            let origin_y = row * CELL;
            blit(&mut pixels, width, origin_x, origin_y, &glyph);
            cells.insert(
                *character,
                Cell {
                    u0: origin_x as f32 / width as f32,
                    v0: origin_y as f32 / height as f32,
                    u1: (origin_x + CELL) as f32 / width as f32,
                    v1: (origin_y + CELL) as f32 / height as f32,
                    ink_width: glyph.ink_width,
                    ink_height: glyph.ink_height,
                    advance: glyph.advance,
                    cell_height: glyph.cell_height,
                },
            );
        }
        Some(Sheet {
            texture: TextureData::new(width, height, pixels),
            cells,
        })
    }

    fn lookup(&self, character: char) -> Option<Glyph> {
        for (pack, providers) in &self.packs {
            if let Some(glyph) = pack_bitmap(pack, providers, character) {
                return Some(glyph);
            }
        }
        unifont(character)
    }
}

/// One decoded glyph at its native resolution.
struct Glyph {
    pixels: Vec<u8>,
    pixel_width: u32,
    pixel_height: u32,
    ink_width: f32,
    ink_height: f32,
    advance: f32,
    cell_height: f32,
}

/// Copies a glyph into its cell, scaling the native pixels to CELL x CELL.
fn blit(sprite: &mut [u8], width: u32, origin_x: u32, origin_y: u32, glyph: &Glyph) {
    if glyph.pixel_width == 0 || glyph.pixel_height == 0 {
        return;
    }
    for y in 0..CELL {
        let sy = y * glyph.pixel_height / CELL;
        for x in 0..CELL {
            let sx = x * glyph.pixel_width / CELL;
            let source = ((sy * glyph.pixel_width + sx) * 4) as usize;
            let Some(pixel) = glyph.pixels.get(source..source + 4) else {
                continue;
            };
            let target = (((origin_y + y) * width + origin_x + x) * 4) as usize;
            if let Some(slot) = sprite.get_mut(target..target + 4) {
                // Text is tinted per vertex, so keep the mask neutral.
                slot.copy_from_slice(&[255, 255, 255, pixel[3]]);
            }
        }
    }
}

/// Glyph from a pack's bitmap providers.
fn pack_bitmap(pack: &ResourcePack, providers: &[BitmapProvider], character: char) -> Option<Glyph> {
    let codepoint = character as u32;
    for provider in providers {
        let Some((x, y)) = provider.position(codepoint) else {
            continue;
        };
        // The page texture is read from whichever pack supplied that provider,
        // so a pack's own page is used when the pack defines the font.
        let Some(texture) = pack.get_texture(&provider.texture) else {
            continue;
        };
        let cell_width = texture.width / provider.columns.max(1);
        let cell_height = texture.height / provider.rows.max(1);
        if cell_width == 0 || cell_height == 0 {
            continue;
        }
        let mut pixels = Vec::with_capacity((cell_width * cell_height * 4) as usize);
        let mut ink_width = 0.0f32;
        let mut ink_rows = 0.0f32;
        for row in 0..cell_height {
            let mut row_has_ink = false;
            for column in 0..cell_width {
                let px = x * cell_width + column;
                let py = y * cell_height + row;
                let offset = ((py * texture.width + px) * 4) as usize;
                match texture.pixels.get(offset..offset + 4) {
                    Some(pixel) => {
                        if pixel[3] > 0 {
                            ink_width = ink_width.max((column + 1) as f32);
                            row_has_ink = true;
                        }
                        pixels.extend_from_slice(pixel);
                    }
                    None => pixels.extend_from_slice(&[0, 0, 0, 0]),
                }
            }
            if row_has_ink {
                ink_rows += 1.0;
            }
        }
        // The page's own ascent describes how far the ink sits above the
        // baseline; ink below the baseline is whatever the cell still holds.
        let ink_height = (provider.ascent as f32).max(ink_rows);
        return Some(Glyph {
            pixels,
            pixel_width: cell_width,
            pixel_height: cell_height,
            ink_width: if ink_width > 0.0 { ink_width } else { 1.0 },
            ink_height: ink_height.max(1.0),
            advance: advance(codepoint),
            cell_height: 8.0,
        });
    }
    None
}

/// Advance in font pixels, following vanilla's ranges: CJK-style glyphs are full
/// width, spaces narrower, everything else six wide.
fn advance(codepoint: u32) -> f32 {
    if codepoint == ' ' as u32 {
        return 4.0;
    }
    if (0x2e80..=0x9fff).contains(&codepoint)
        || (0xac00..=0xd7af).contains(&codepoint)
        || (0xf900..=0xfaff).contains(&codepoint)
        || (0xff00..=0xff60).contains(&codepoint)
    {
        return 16.0;
    }
    6.0
}

/// Glyph from the embedded GNU Unifont table.
fn unifont(character: char) -> Option<Glyph> {
    static TABLE: OnceLock<HashMap<u32, UnifontGlyph>> = OnceLock::new();
    let table = TABLE.get_or_init(load_unifont);
    let glyph = table.get(&(character as u32))?;
    Some(Glyph {
        pixels: glyph.pixels.clone(),
        pixel_width: 16,
        pixel_height: 16,
        ink_width: glyph.ink_width,
        ink_height: 16.0,
        advance: advance(character as u32),
        cell_height: 16.0,
    })
}

struct UnifontGlyph {
    pixels: Vec<u8>,
    ink_width: f32,
}

/// Decodes the embedded GNU Unifont table into 16x16 alpha masks. The format is
/// one line per glyph: `HEXCODE:32 HEX DIGITS`, one 16-bit big-endian row per
/// hex quartet, top row first.
fn load_unifont() -> HashMap<u32, UnifontGlyph> {
    let bytes: &[u8] = include_bytes!("../data/unifont-cn.hex.gz");
    let mut decoder = flate2::read::GzDecoder::new(std::io::Cursor::new(bytes));
    let mut text = String::new();
    if std::io::Read::read_to_string(&mut decoder, &mut text).is_err() {
        return HashMap::new();
    }
    let mut table = HashMap::new();
    for line in text.lines() {
        let Some((codepoint, bitmap)) = line.split_once(':') else {
            continue;
        };
        let Ok(codepoint) = u32::from_str_radix(codepoint, 16) else {
            continue;
        };
        let digits = bitmap.as_bytes();
        let mut pixels = Vec::with_capacity(16 * 16 * 4);
        let mut ink_width = 0.0f32;
        for row in 0..16u32 {
            for column in 0..16u32 {
                let index = row * 16 + column;
                let nibble = index / 4;
                let shift = 3 - index % 4;
                let on = digits
                    .get(nibble as usize)
                    .and_then(|value| (*value as char).to_digit(16))
                    .is_some_and(|digit| (digit >> shift) & 1 == 1);
                if on {
                    ink_width = ink_width.max((column + 1) as f32);
                    pixels.extend_from_slice(&[255, 255, 255, 255]);
                } else {
                    pixels.extend_from_slice(&[0, 0, 0, 0]);
                }
            }
        }
        table.insert(codepoint, UnifontGlyph { pixels, ink_width });
    }
    table
}
