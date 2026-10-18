//! Sign text: reading it out of block entities and laying it onto the sign.
//!
//! Signs store up to four lines per side. Modern (1.20+) worlds use
//! `front_text`/`back_text` compounds with a `messages` list of JSON text
//! components; older worlds use `Text1`..`Text4`. Both shapes are read, along
//! with the dye colour and the glowing flag. Layout follows the client: lines
//! are centred, ten font-pixels apart, and shrink to fit the writeable width.

use nucleation::utils::NbtValue;
use nucleation::nbt::NbtMap;

use std::collections::HashMap;

use crate::fonts::Cell;

/// One side of a sign.
#[derive(Clone, Debug, Default)]
pub(super) struct Side {
    pub lines: Vec<String>,
    pub color: [f32; 3],
    pub glowing: bool,
}

impl Side {
    /// The ink a line starts with. Glowing text still uses the dye colour; the
    /// flag only tells the client to skip its shadow pass, which this renderer
    /// does not draw to begin with.
    fn ink(&self) -> [f32; 3] {
        self.color
    }
}

/// The text of one sign block, keyed by whether it faces the sign's front.
#[derive(Clone, Debug, Default)]
pub(super) struct SignText {
    pub front: Option<Side>,
    pub back: Option<Side>,
}

impl SignText {
    pub fn is_empty(&self) -> bool {
        [&self.front, &self.back].into_iter().all(|side| {
            side.as_ref()
                .is_none_or(|side| side.lines.iter().all(|line| line.trim().is_empty()))
        })
    }

    /// Every line the sign draws, both sides, in reading order.
    pub fn lines(&self) -> impl Iterator<Item = &str> + '_ {
        [&self.front, &self.back]
            .into_iter()
            .flatten()
            .flat_map(|side| side.lines.iter().map(String::as_str))
    }

    /// Every character the sign draws, for the glyph rasteriser.
    pub fn characters(&self) -> impl Iterator<Item = char> + '_ {
        self.lines().flat_map(str::chars)
    }
}

/// Quarter turns the sign's front faces, from a block state's `rotation`
/// (standing signs, 0-15) or `facing` (wall and hanging signs). 0 means the
/// text reads from the south side (`+Z`), then clockwise: 1 east, 2 north,
/// 3 west.
pub(crate) fn state_yaw(state: &nucleation::BlockState) -> u8 {
    let property = |key: &str| {
        state
            .properties
            .iter()
            .find(|(name, _)| name.as_str() == key)
            .map(|(_, value)| value.as_str())
    };
    if let Some(rotation) = property("rotation").and_then(|value| value.parse::<u8>().ok()) {
        // A standing sign's `rotation` runs counter-clockwise from south:
        // 0 = south, 4 = west, 8 = north, 12 = east.
        return match (rotation as u32 % 16) / 4 {
            1 => 3,
            2 => 2,
            3 => 1,
            _ => 0,
        };
    }
    // Wall and hanging signs store the direction the sign faces outright.
    match property("facing") {
        Some("east") => 1,
        Some("north") => 2,
        Some("west") => 3,
        // South and anything unexpected take the default facing.
        _ => 0,
    }
}

/// Reads one sign block entity's text, or `None` when it holds none.
pub(super) fn read(nbt: &NbtMap, id: &str) -> Option<SignText> {
    if !id.ends_with("sign") {
        return None;
    }
    parse(nbt)
}

/// [`read`] for the streamed reader, which holds a tile's NBT as parsed quartz
/// tags. The id check is the same; only the container type differs.
pub(super) fn read_quartz(nbt: &quartz_nbt::NbtCompound) -> Option<SignText> {
    let map = NbtMap::from_quartz_nbt(nbt);
    let id = map
        .get("id")
        .or_else(|| map.get("Id"))
        .and_then(|value| match value {
            NbtValue::String(id) => Some(id.as_str()),
            _ => None,
        })?;
    if !id.ends_with("sign") {
        return None;
    }
    parse(&map)
}

/// The shared text extraction for both NBT containers.
fn parse(nbt: &NbtMap) -> Option<SignText> {
    let mut text = SignText::default();
    // Modern format: both sides carry their own messages, colour and glow.
    for (key, front) in [("front_text", true), ("back_text", false)] {
        let Some(NbtValue::Compound(side)) = nbt.get(key) else {
            continue;
        };
        let lines = side
            .get("messages")
            .and_then(|messages| match messages {
                NbtValue::List(lines) => Some(
                    lines
                        .iter()
                        .filter_map(|line| match line {
                            NbtValue::String(line) => Some(plain_text(line)),
                            _ => None,
                        })
                        .collect::<Vec<_>>(),
                ),
                _ => None,
            })
            .unwrap_or_default();
        let color = side
            .get("color")
            .and_then(|value| match value {
                NbtValue::String(name) => dye_color(name),
                _ => None,
            })
            .unwrap_or([0.0, 0.0, 0.0]);
        let glowing = matches!(
            side.get("has_glowing_text"),
            Some(NbtValue::Byte(1) | NbtValue::Int(1))
        );
        let side = Side {
            lines,
            color,
            glowing,
        };
        if front {
            text.front = Some(side);
        } else {
            text.back = Some(side);
        }
    }
    if text.front.is_none() && text.back.is_none() {
        // Legacy format: four flat strings with a single colour.
        let mut lines = Vec::new();
        for key in ["Text1", "Text2", "Text3", "Text4"] {
            if let Some(NbtValue::String(line)) = nbt.get(key) {
                lines.push(plain_text(line));
            }
        }
        if lines.is_empty() {
            return None;
        }
        let color = nbt
            .get("Color")
            .and_then(|value| match value {
                NbtValue::String(name) => dye_color(name),
                NbtValue::Int(value) => Some(minecraft_color(*value)),
                _ => None,
            })
            .unwrap_or([0.0, 0.0, 0.0]);
        let glowing = matches!(
            nbt.get("GlowingText"),
            Some(NbtValue::Byte(1) | NbtValue::Int(1))
        );
        text.front = Some(Side {
            lines,
            color,
            glowing,
        });
        text.back = None;
    }
    Some(text)
}

/// Strips the JSON quoting a message is stored with and keeps its text. A
/// component may be a bare string or an object with `text`/`extra` children.
fn plain_text(message: &str) -> String {
    let trimmed = message.trim();
    if let Ok(serde_json::Value::String(text)) = serde_json::from_str::<serde_json::Value>(trimmed)
    {
        return text;
    }
    if let Ok(value) = serde_json::from_str::<serde_json::Value>(trimmed) {
        return json_text(&value);
    }
    trimmed.to_string()
}

fn json_text(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(text) => text.clone(),
        serde_json::Value::Array(values) => values.iter().map(json_text).collect(),
        serde_json::Value::Object(map) => {
            let mut text = map
                .get("text")
                .map(json_text)
                .unwrap_or_default();
            if let Some(extra) = map.get("extra") {
                text.push_str(&json_text(extra));
            }
            text
        }
        _ => String::new(),
    }
}

/// A sign's dye colour as an RGB tint.
fn dye_color(name: &str) -> Option<[f32; 3]> {
    Some(match name {
        "white" => [1.0, 1.0, 1.0],
        "orange" => [0.95, 0.62, 0.2],
        "magenta" => [0.79, 0.4, 0.79],
        "light_blue" => [0.4, 0.68, 0.95],
        "yellow" => [0.95, 0.95, 0.2],
        "lime" => [0.5, 0.9, 0.2],
        "pink" => [0.95, 0.62, 0.7],
        "gray" => [0.3, 0.3, 0.3],
        "light_gray" => [0.6, 0.6, 0.6],
        "cyan" => [0.2, 0.62, 0.62],
        "purple" => [0.55, 0.2, 0.79],
        "blue" => [0.22, 0.28, 0.75],
        "brown" => [0.45, 0.3, 0.18],
        "green" => [0.22, 0.5, 0.14],
        "red" => [0.65, 0.2, 0.18],
        "black" => [0.0, 0.0, 0.0],
        _ => return None,
    })
}

/// The legacy integer colour, in Minecraft's dye order.
fn minecraft_color(value: i32) -> [f32; 3] {
    match value {
        1 => [0.95, 0.62, 0.2],
        2 => [0.79, 0.4, 0.79],
        3 => [0.4, 0.68, 0.95],
        4 => [0.95, 0.95, 0.2],
        5 => [0.5, 0.9, 0.2],
        6 => [0.95, 0.62, 0.7],
        7 => [0.3, 0.3, 0.3],
        8 => [0.6, 0.6, 0.6],
        9 => [0.2, 0.62, 0.62],
        10 => [0.55, 0.2, 0.79],
        11 => [0.22, 0.28, 0.75],
        12 => [0.45, 0.3, 0.18],
        13 => [0.22, 0.5, 0.14],
        14 => [0.65, 0.2, 0.18],
        15 => [0.0, 0.0, 0.0],
        _ => [0.0, 0.0, 0.0],
    }
}

/// A quad of sign text, in the sign block's local space.
pub(super) struct TextQuad {
    pub positions: [[f32; 3]; 4],
    pub uv: [[f32; 2]; 4],
    pub color: [f32; 4],
}

/// One glyph of a laid-out line: where it sits and what ink it uses.
struct GlyphRun {
    cell: Cell,
    ink: [f32; 3],
    bold: bool,
}

/// Where a sign's board sits in the mesh, so text lands on its face rather than
/// on a guessed plane. The mesher's sign model is an approximation — a wall
/// sign's board is a thin slab at the block edge, a standing sign's sits above
/// its post — so the bounds come from the geometry emitted for that block.
#[derive(Clone, Copy)]
pub(crate) struct Board {
    /// Text anchor: the block's centre horizontally, the board's centre
    /// vertically.
    pub center: [f32; 3],
    /// Axis the board is thin along: 0 (x) or 2 (z).
    pub normal_axis: usize,
    /// Extent along `normal_axis`.
    pub low: f32,
    pub high: f32,
    /// Board half-height, used to size the text.
    pub half_height: f32,
}

/// Lays out every line of a side and emits its quads, positioned on the board's
/// face. `yaw` is the quarter turns the front faces (0 = `+Z`), and `mirrored`
/// is set for the back side, which the client draws to be read from behind.
pub(super) fn quads(
    cells: &HashMap<char, Cell>,
    side: &Side,
    yaw: u8,
    mirrored: bool,
    board: Board,
    px: f32,
) -> Vec<TextQuad> {
    let mut quads = Vec::new();
    let lines: Vec<&String> = side
        .lines
        .iter()
        .filter(|line| !line.trim().is_empty())
        .collect();
    if lines.is_empty() {
        return quads;
    }
    // The reader's right runs along the board's wide axis; the front face is the
    // one the sign faces, and the back side is its opposite.
    let perp = 2 - board.normal_axis;
    let front_positive = match yaw % 4 {
        0 => true,  // +Z
        1 => true,  // +X
        2 => false, // -Z
        _ => false, // -X
    };
    let front_positive = front_positive != mirrored;
    let plane = if front_positive {
        board.high + 0.005
    } else {
        board.low - 0.005
    };
    let right_sign = if board.normal_axis == 2 {
        if front_positive { 1.0 } else { -1.0 }
    } else if front_positive {
        -1.0
    } else {
        1.0
    };
    let line_height = 10.0_f32;
    let centre_y = (lines.len() as f32 - 1.0) * line_height / 2.0;
    for (index, line) in lines.iter().enumerate() {
        let runs = layout_line(cells, line, side.color);
        let width: f32 = runs
            .iter()
            .map(|run| run.cell.advance + if run.bold { 1.0 } else { 0.0 })
            .sum();
        let mut pen = 8.0 - width / 2.0;
        let baseline = index as f32 * line_height - centre_y;
        for run in runs {
            let advance = run.cell.advance + if run.bold { 1.0 } else { 0.0 };
            // Font pixels: x from the line's left edge, y up from the baseline.
            let x0 = pen - 8.0;
            let x1 = x0 + run.cell.ink_width;
            let y1 = baseline + 1.0;
            let y0 = y1 - run.cell.ink_height;
            let tint = [run.ink[0], run.ink[1], run.ink[2], 1.0];
            let [[u0, v0], [u1, v1]] = [[run.cell.u0, run.cell.v0], [run.cell.u1, run.cell.v1]];
            let corners = [
                ([x0, y1], [u0, v1]),
                ([x1, y1], [u1, v1]),
                ([x1, y0], [u1, v0]),
                ([x0, y0], [u0, v0]),
            ];
            let mut positions = [[0.0f32; 3]; 4];
            let mut uv = [[0.0f32; 2]; 4];
            for (corner, (point, texture)) in corners.iter().enumerate() {
                let [x, y] = *point;
                let mut position = [0.0f32; 3];
                position[perp] = board.center[perp] + right_sign * x * px;
                position[1] = board.center[1] + y * px;
                position[board.normal_axis] = plane;
                positions[corner] = position;
                uv[corner] = *texture;
            }
            quads.push(TextQuad {
                positions,
                uv,
                color: tint,
            });
            if run.bold {
                // Vanilla fakes bold by drawing the glyph again one pixel over.
                let mut shifted = positions;
                for position in &mut shifted {
                    position[perp] += right_sign * px;
                }
                quads.push(TextQuad {
                    positions: shifted,
                    uv,
                    color: tint,
                });
            }
            pen += advance;
        }
    }
    quads
}

/// The board's `right` and outward `out` unit vectors for a facing and side.
fn board_frame(yaw: u8, mirrored: bool) -> ([f32; 3], [f32; 3]) {
    // A standing sign's front points along its rotation: 0 = +Z, 1 = +X, ...
    let mut out = match yaw % 4 {
        1 => [1.0, 0.0, 0.0],
        2 => [0.0, 0.0, -1.0],
        3 => [-1.0, 0.0, 0.0],
        _ => [0.0, 0.0, 1.0],
    };
    // The back side is read from behind, so it faces the opposite way and runs
    // the other way round; `right` follows the flipped normal.
    if mirrored {
        out = [-out[0], 0.0, -out[2]];
    }
    let right = [out[2], 0.0, -out[0]];
    (right, out)
}

/// Splits one line into glyphs, honouring `§` format codes.
fn layout_line(cells: &HashMap<char, Cell>, line: &str, default_ink: [f32; 3]) -> Vec<GlyphRun> {
    let mut runs = Vec::new();
    let mut ink = default_ink;
    let mut bold = false;
    let mut characters = line.chars().peekable();
    while let Some(character) = characters.next() {
        if character == '§' {
            let Some(code) = characters.next() else {
                break;
            };
            match code.to_ascii_lowercase() {
                'l' => bold = true,
                'r' => {
                    ink = default_ink;
                    bold = false;
                }
                other => {
                    if let Some(color) = format_color(other) {
                        ink = color;
                    }
                }
            }
            continue;
        }
        let Some(cell) = cells.get(&character) else {
            continue;
        };
        runs.push(GlyphRun {
            cell: *cell,
            ink,
            bold,
        });
    }
    runs
}

/// The client's `§0`..`§f` palette.
fn format_color(code: char) -> Option<[f32; 3]> {
    Some(match code {
        '0' => [0.0, 0.0, 0.0],
        '1' => [0.0, 0.0, 0.67],
        '2' => [0.0, 0.67, 0.0],
        '3' => [0.0, 0.67, 0.67],
        '4' => [0.67, 0.0, 0.0],
        '5' => [0.67, 0.0, 0.67],
        '6' => [1.0, 0.67, 0.0],
        '7' => [0.67, 0.67, 0.67],
        '8' => [0.33, 0.33, 0.33],
        '9' => [0.33, 0.33, 1.0],
        'a' => [0.33, 1.0, 0.33],
        'b' => [0.33, 1.0, 1.0],
        'c' => [1.0, 0.33, 0.33],
        'd' => [1.0, 0.33, 1.0],
        'e' => [1.0, 1.0, 0.33],
        'f' => [1.0, 1.0, 1.0],
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(name: &str, key: &str, value: &str) -> nucleation::BlockState {
        let mut state = nucleation::BlockState::new(name);
        state.set_property(key, value);
        state
    }

    #[test]
    fn yaw_matches_the_blocks_own_orientation() {
        // Standing signs number their rotations counter-clockwise from south.
        assert_eq!(state_yaw(&state("minecraft:oak_sign", "rotation", "0")), 0);
        assert_eq!(state_yaw(&state("minecraft:oak_sign", "rotation", "4")), 3);
        assert_eq!(state_yaw(&state("minecraft:oak_sign", "rotation", "8")), 2);
        assert_eq!(state_yaw(&state("minecraft:oak_sign", "rotation", "12")), 1);
        assert_eq!(state_yaw(&state("minecraft:oak_sign", "rotation", "15")), 1);
        // Wall signs store the direction they face outright; the user's real
        // schematics are almost all `facing=south`.
        assert_eq!(state_yaw(&state("minecraft:oak_wall_sign", "facing", "south")), 0);
        assert_eq!(state_yaw(&state("minecraft:oak_wall_sign", "facing", "east")), 1);
        assert_eq!(state_yaw(&state("minecraft:oak_wall_sign", "facing", "north")), 2);
        assert_eq!(state_yaw(&state("minecraft:oak_wall_sign", "facing", "west")), 3);
    }

    fn cells() -> HashMap<char, Cell> {
        HashMap::from([(
            'A',
            Cell {
                u0: 0.0,
                v0: 0.0,
                u1: 1.0,
                v1: 1.0,
                ink_width: 6.0,
                ink_height: 8.0,
                advance: 6.0,
                cell_height: 8.0,
            },
        )])
    }

    fn first_line(text: &str) -> Side {
        Side {
            lines: vec![text.to_string(), String::new(), String::new(), String::new()],
            color: [0.0, 0.0, 0.0],
            glowing: false,
        }
    }

    /// A south-facing board: thin along z, spanning x ±0.5.
    fn board() -> Board {
        Board {
            center: [0.0, 0.0, 0.0],
            normal_axis: 2,
            low: -0.04,
            high: 0.04,
            half_height: 0.25,
        }
    }

    /// An east-facing board like a wall sign: thin along x, flush with +x.
    fn east_board() -> Board {
        Board {
            center: [0.0, 0.0, 0.0],
            normal_axis: 0,
            low: 0.4,
            high: 0.48,
            half_height: 0.25,
        }
    }

    #[test]
    fn text_lands_on_the_face_the_sign_faces() {
        // yaw 0 = +Z, 1 = +X, 2 = -Z, 3 = -X.
        for (yaw, axis, sign) in [(0u8, 2usize, 1.0f32), (2, 2, -1.0)] {
            let quads = quads(&cells(), &first_line("A"), yaw, false, board(), 0.01);
            assert!(!quads.is_empty(), "yaw {yaw} produced no text");
            for quad in &quads {
                for position in &quad.positions {
                    assert!(
                        position[axis] * sign > 0.0,
                        "yaw {yaw}: {position:?} is not on the front face"
                    );
                }
            }
        }
    }

    #[test]
    fn east_and_west_text_sits_on_their_board_face() {
        // The wall-sign board is flush with one side of the block; text must be
        // written just outside that face, not half a block away.
        for (yaw, expected) in [(1u8, 0.485f32), (3, 0.395)] {
            let quads = quads(&cells(), &first_line("A"), yaw, false, east_board(), 0.01);
            assert!(!quads.is_empty(), "yaw {yaw} produced no text");
            for quad in &quads {
                for position in &quad.positions {
                    assert!(
                        (position[0] - expected).abs() < 1e-3,
                        "yaw {yaw}: {position:?} is not on the board face"
                    );
                }
            }
        }
    }

    #[test]
    fn back_text_sits_on_the_opposite_face() {
        for (yaw, axis, sign) in [(0u8, 2usize, -1.0f32), (2, 2, 1.0)] {
            let quads = quads(&cells(), &first_line("A"), yaw, true, board(), 0.01);
            assert!(!quads.is_empty(), "yaw {yaw} produced no back text");
            for quad in &quads {
                for position in &quad.positions {
                    assert!(
                        position[axis] * sign > 0.0,
                        "yaw {yaw}: back text {position:?} is on the front side"
                    );
                }
            }
        }
    }

    #[test]
    fn formatting_codes_colour_a_line_and_setting_them_off_restores_the_dye() {
        let ink = |text: &str| {
            let side = first_line(text);
            layout_line(&cells(), text, side.ink())[0].ink
        };
        assert_eq!(ink("A"), [0.0, 0.0, 0.0]);
        assert_eq!(ink("§cA"), [1.0, 0.33, 0.33]);
        assert_eq!(ink("§c§rA"), [0.0, 0.0, 0.0]);
    }
}
