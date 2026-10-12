//! Bitmap font provider definitions from a resource pack's `font/*.json`.
//!
//! Only the parts a sign renderer needs are kept: which page texture holds a
//! codepoint's glyph, the page grid, and the ascent. The provider chain is
//! resolved through `reference` entries the way the client does, so a pack that
//! ships the same files as the vanilla jar (the bundled one does) works
//! unchanged.

use std::io::Read;

/// One bitmap provider: a page texture plus the codepoints it holds, in order.
#[derive(Clone, Debug)]
pub struct BitmapProvider {
    /// Page texture, e.g. `font/ascii` (no extension), as resource packs name it.
    pub texture: String,
    pub columns: u32,
    pub rows: u32,
    pub ascent: i32,
    /// Codepoint ranges in page order.
    pub ranges: Vec<(u32, u32)>,
}

impl BitmapProvider {
    /// Page cell of a codepoint, if this provider holds it.
    pub fn position(&self, codepoint: u32) -> Option<(u32, u32)> {
        let mut index = 0u32;
        for (start, end) in &self.ranges {
            if (*start..=*end).contains(&codepoint) {
                let slot = index + (codepoint - start);
                return Some((slot % self.columns, slot / self.columns));
            }
            index += end - start + 1;
        }
        None
    }
}

/// Parses a pack's `font/default.json` chain into flat bitmap providers, in
/// resolution order. Returns an empty list when the pack ships no fonts.
pub fn parse(archive: &[u8]) -> Vec<BitmapProvider> {
    let Ok(mut zip) = zip::ZipArchive::new(std::io::Cursor::new(archive)) else {
        return Vec::new();
    };
    let mut providers = Vec::new();
    let mut visited = std::collections::HashSet::new();
    collect(
        &mut zip,
        "assets/minecraft/font/default.json",
        &mut visited,
        &mut providers,
        0,
    );
    providers
}

fn collect(
    zip: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>,
    path: &str,
    visited: &mut std::collections::HashSet<String>,
    providers: &mut Vec<BitmapProvider>,
    depth: u32,
) {
    if depth > 8 || !visited.insert(path.to_string()) {
        return;
    }
    let Some(document) = read_json(zip, path) else {
        return;
    };
    let Some(list) = document.get("providers").and_then(|value| value.as_array()) else {
        return;
    };
    for provider in list {
        match provider.get("type").and_then(|value| value.as_str()) {
            Some("reference") => {
                let Some(id) = provider.get("id").and_then(|value| value.as_str()) else {
                    continue;
                };
                if let Some(reference) = font_path(id) {
                    collect(zip, &reference, visited, providers, depth + 1);
                }
            }
            Some("bitmap") => {
                let Some(texture) = provider.get("file").and_then(|value| value.as_str()) else {
                    continue;
                };
                let ascent = provider
                    .get("ascent")
                    .and_then(serde_json::Value::as_i64)
                    .unwrap_or(7) as i32;
                let rows = provider
                    .get("height")
                    .and_then(serde_json::Value::as_u64)
                    .map_or(16, |value| value as u32);
                let Some(texture) = texture.strip_prefix("minecraft:") else {
                    continue;
                };
                let Some(chars) = provider.get("chars").and_then(|value| value.as_array()) else {
                    continue;
                };
                let mut ranges = Vec::new();
                for row in chars {
                    let Some(row) = row.as_str() else {
                        continue;
                    };
                    ranges.extend(char_ranges(row));
                }
                if ranges.is_empty() {
                    continue;
                }
                let columns = ranges.len().div_ceil(rows as usize).max(1) as u32;
                providers.push(BitmapProvider {
                    texture: texture.to_string(),
                    columns,
                    rows,
                    ascent,
                    ranges,
                });
            }
            _ => {}
        }
    }
}

/// Consecutive codepoints collapse into one range, matching how the client
/// walks a provider's `chars` string.
fn char_ranges(row: &str) -> Vec<(u32, u32)> {
    let mut ranges: Vec<(u32, u32)> = Vec::new();
    for character in row.chars() {
        match ranges.last_mut() {
            Some((_, end)) if *end + 1 == character as u32 => *end = character as u32,
            _ => ranges.push((character as u32, character as u32)),
        }
    }
    ranges
}

/// `minecraft:include/unifont` -> `assets/minecraft/font/include/unifont.json`.
fn font_path(id: &str) -> Option<String> {
    let path = id.strip_prefix("minecraft:")?;
    Some(format!("assets/minecraft/font/{path}.json"))
}

fn read_json(zip: &mut zip::ZipArchive<std::io::Cursor<&[u8]>>, path: &str) -> Option<serde_json::Value> {
    let mut file = zip.by_name(path).ok()?;
    let mut contents = String::new();
    file.read_to_string(&mut contents).ok()?;
    serde_json::from_str(&contents).ok()
}
