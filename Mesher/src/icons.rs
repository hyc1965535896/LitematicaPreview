//! Software-rendered block icons for the graphical replace picker.
//!
//! Every blockstate in the bundled resource pack becomes a small PNG. The item
//! definition decides the shape the way vanilla inventory icons do: a model
//! with elements renders as shaded isometric boxes, a generated item model
//! renders as a flat sprite; without an item definition the blockstate path
//! mirrors the meshing resolution, falling back to a flat sprite whenever an
//! element is rotated (crosses, torches) or too thin to read as a cube.

use std::collections::HashMap;
use std::io::Cursor;
use std::rc::Rc;

use base64::engine::general_purpose::STANDARD as BASE64_STANDARD;
use base64::Engine as _;
use image::RgbaImage;
use nucleation::meshing::ResourcePackSource;
use schematic_mesher::resolver::{resolve_block, ModelResolver};
use schematic_mesher::{Direction, InputBlock, ResourcePack};
use serde::Serialize;

/// Icon edge length in pixels.
const ICON_SIZE: u32 = 64;
/// Outer margin kept clear around the rendered block.
const MARGIN: f32 = 3.0;
// Dimetric projection seen from the north-west-top — the same three faces
// (top, north, west) vanilla inventory icons expose, with vanilla's face
// brightness (top 1.0, north 0.8, west 0.6). The depth axis is (-1, 1, -1),
// so the y term moves twice as fast as the horizontal sum and the three
// visible faces tile the hexagon without overlapping.
const COS_YAW: f32 = std::f32::consts::FRAC_1_SQRT_2; // per unit of (z - x)
const UNIT_Y: f32 = std::f32::consts::FRAC_1_SQRT_2; // per unit of y
const UNIT_XZ: f32 = std::f32::consts::FRAC_1_SQRT_2 / 2.0; // per unit of (x + z)
// Scale so a full cube spans the icon height minus margins.
const SCALE: f32 = (ICON_SIZE as f32 - 2.0 * MARGIN) / (16.0 * UNIT_Y + 32.0 * UNIT_XZ);
const CENTER_X: f32 = ICON_SIZE as f32 / 2.0;
const CENTER_Y: f32 = ICON_SIZE as f32 - MARGIN;

/// One selectable block: id plus a `data:image/png;base64,…` icon.
#[derive(Debug, Clone, Serialize)]
pub struct BlockIcon {
    pub name: String,
    pub icon: String,
}

/// Renders an icon for every blockstate in the pack, sorted by block id.
///
/// Blocks whose models cannot be resolved still get an entry with an empty
/// icon so the picker can offer them by id.
pub fn block_icons(pack_bytes: &[u8]) -> Result<Vec<BlockIcon>, String> {
    let source = ResourcePackSource::from_bytes(pack_bytes)
        .map_err(|e| format!("无法读取随附的方块资源：{e}"))?;
    let item_models = item_model_locations(pack_bytes);
    let pack = source.pack();
    let mut icons = Vec::new();
    for name in source.list_blockstates() {
        // Ordinary air stays selectable as a replacement target (deleting
        // blocks); the technical air variants are not worth picking.
        if name != "minecraft:air" && is_air(&name) {
            continue;
        }
        let icon = render_block(pack, &name, item_models.get(&name).map(String::as_str));
        icons.push(BlockIcon {
            name,
            icon: icon.unwrap_or_default(),
        });
    }
    icons.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(icons)
}

fn is_air(name: &str) -> bool {
    matches!(
        name,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}

/// Maps block ids to the model their item definition points at. Vanilla picks
/// inventory icons from `assets/<namespace>/items/<block>.json`; blocks
/// without one fall back to their blockstate.
fn item_model_locations(pack_bytes: &[u8]) -> HashMap<String, String> {
    let mut models = HashMap::new();
    let Ok(mut archive) = zip::ZipArchive::new(Cursor::new(pack_bytes)) else {
        return models;
    };
    for index in 0..archive.len() {
        let Ok(mut entry) = archive.by_index(index) else {
            continue;
        };
        let entry_name = entry.name().to_string();
        let Some(rest) = entry_name.strip_prefix("assets/") else {
            continue;
        };
        let Some((namespace, tail)) = rest.split_once('/') else {
            continue;
        };
        let Some(block) = tail.strip_prefix("items/") else {
            continue;
        };
        let Some(block) = block.strip_suffix(".json") else {
            continue;
        };
        let Ok(text) = std::io::read_to_string(&mut entry) else {
            continue;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&text) else {
            continue;
        };
        if let Some(location) = find_item_model(&value) {
            models.insert(format!("{namespace}:{block}"), location);
        }
    }
    models
}

/// Depth-first search for the first concrete `minecraft:model` node.
fn find_item_model(value: &serde_json::Value) -> Option<String> {
    if let Some(object) = value.as_object() {
        if object.get("type").and_then(|t| t.as_str()) == Some("minecraft:model") {
            if let Some(model) = object.get("model").and_then(|m| m.as_str()) {
                return Some(model.to_string());
            }
        }
        for child in object.values() {
            if let Some(found) = find_item_model(child) {
                return Some(found);
            }
        }
    } else if let Some(array) = value.as_array() {
        for child in array {
            if let Some(found) = find_item_model(child) {
                return Some(found);
            }
        }
    }
    None
}

/// Fallback tints for blocks the game colors by biome; mirrors nucleation's
/// item renderer so grass, leaves and water do not show their gray base maps.
fn tint_for_block(name: &str) -> Option<[u8; 3]> {
    let name = name.strip_prefix("minecraft:").unwrap_or(name);
    match name {
        "redstone_wire" => Some([255, 0, 0]),
        "grass_block" | "grass" | "short_grass" | "tall_grass" | "fern" | "large_fern" => {
            Some([124, 189, 107])
        }
        "oak_leaves" | "jungle_leaves" | "acacia_leaves" | "dark_oak_leaves"
        | "mangrove_leaves" => Some([106, 173, 51]),
        "birch_leaves" => Some([128, 167, 85]),
        "spruce_leaves" => Some([97, 153, 97]),
        "water" | "water_cauldron" => Some([63, 118, 228]),
        "lily_pad" => Some([32, 128, 48]),
        "vine" | "hanging_roots" => Some([106, 173, 51]),
        _ => None,
    }
}

fn tint_without_index(name: &str) -> bool {
    matches!(
        name.strip_prefix("minecraft:").unwrap_or(name),
        "water" | "water_cauldron"
    )
}

struct Texture {
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Texture {
    fn from_resource(pack: &ResourcePack, location: &str) -> Option<Texture> {
        let data = pack.get_texture(location)?.first_frame();
        Some(Texture {
            width: data.width,
            height: data.height,
            pixels: data.pixels,
        })
    }
}

struct FaceDraw {
    texture: Rc<Texture>,
    uv: [f32; 4],
    tint: Option<[u8; 3]>,
    brightness: f32,
}

struct BoxDraw {
    min: [f32; 3],
    max: [f32; 3],
    top: Option<FaceDraw>,
    north: Option<FaceDraw>,
    west: Option<FaceDraw>,
}

/// Resolves one block id to a base64 PNG, or `None` when nothing renders.
fn render_block(pack: &ResourcePack, name: &str, item_model: Option<&str>) -> Option<String> {
    if name == "minecraft:air" {
        return Some(encode(&draw_air()));
    }
    let tint = tint_for_block(name);
    let mut boxes = Vec::new();
    let mut sprite: Option<(Rc<Texture>, Option<[u8; 3]>)> = None;

    if let Some(location) = item_model {
        let resolver = ModelResolver::new(pack);
        if let Ok(model) = resolver.resolve(location) {
            if model.has_elements() {
                if !model.elements.iter().any(|element| element.rotation.is_some()) {
                    let cache = TextureCache::new(pack);
                    collect_boxes(&resolver, &model, &[0.0, 0.0], tint, name, &cache, &mut boxes);
                }
            } else {
                sprite = flat_sprite(pack, &model, tint);
            }
        }
    }

    if boxes.is_empty() && sprite.is_none() {
        // No usable item model: resolve the blockstate like the meshing path.
        let resolver = ModelResolver::new(pack);
        if let Ok(resolved) = resolve_block(pack, &default_input(name)) {
            for variant in &resolved {
                let turns = [variant.transform.x as f32, variant.transform.y as f32];
                let rotated_elements = variant
                    .model
                    .elements
                    .iter()
                    .any(|element| element.rotation.is_some());
                if rotated_elements || quarter_turns_of(&turns).is_none() {
                    boxes.clear();
                    break;
                }
                let cache = TextureCache::new(pack);
                collect_boxes(&resolver, &variant.model, &turns, tint, name, &cache, &mut boxes);
            }
        }
        if boxes.is_empty() {
            // Particle-only models (water, chests) have no elements to place;
            // a shaded cube of their particle texture still reads as a block.
            if let Some((texture, tint)) = flat_from_blockstate(pack, name) {
                let face = |brightness| FaceDraw {
                    texture: texture.clone(),
                    uv: [0.0, 0.0, 16.0, 16.0],
                    tint,
                    brightness,
                };
                boxes.push(BoxDraw {
                    min: [0.0, 0.0, 0.0],
                    max: [16.0, 16.0, 16.0],
                    top: Some(face(1.0)),
                    north: Some(face(0.8)),
                    west: Some(face(0.6)),
                });
            }
        }
        if boxes.is_empty() {
            sprite = flat_from_blockstate(pack, name);
        }
    }

    // Ground plates without item definitions (redstone wire) read as their
    // flat dust texture, like the vanilla item icons.
    let plates = !boxes.is_empty()
        && boxes.iter().all(|b| {
            b.max[1] - b.min[1] <= 1.01
                && b.max[0] - b.min[0] >= 14.0
                && b.max[2] - b.min[2] >= 14.0
        });
    if plates {
        if let Some((texture, tint)) = boxes.iter().find_map(|b| {
            b.top
                .as_ref()
                .map(|face| (face.texture.clone(), face.tint))
        }) {
            return Some(encode(&draw_sprite(&texture, tint)));
        }
    }

    if boxes.is_empty() {
        let (texture, tint) = sprite?;
        return Some(encode(&draw_sprite(&texture, tint)));
    }

    // Thin sticks (torches, fences posts read poorly mid-air) match the flat
    // vanilla item icons they replace.
    let thin = boxes.iter().any(|b| {
        let size = [
            b.max[0] - b.min[0],
            b.max[1] - b.min[1],
            b.max[2] - b.min[2],
        ];
        size.iter().filter(|&&s| s < 4.0).count() >= 2
    });
    if thin {
        if let Some((texture, tint)) = sprite.or_else(|| flat_from_blockstate(pack, name)) {
            return Some(encode(&draw_sprite(&texture, tint)));
        }
    }

    Some(encode(&draw_iso(&boxes)))
}

fn default_input(name: &str) -> InputBlock {
    let mut input = InputBlock::new(name);
    if let Some(facts) = nucleation::blockpedia::get_block(name) {
        for (key, value) in facts.default_state {
            input.properties.insert(key.to_string(), value.to_string());
        }
    }
    input
}

/// Shares decoded textures across the faces of one block.
struct TextureCache<'a> {
    pack: &'a ResourcePack,
    entries: std::cell::RefCell<HashMap<String, Option<Rc<Texture>>>>,
}

impl<'a> TextureCache<'a> {
    fn new(pack: &'a ResourcePack) -> Self {
        Self {
            pack,
            entries: std::cell::RefCell::new(HashMap::new()),
        }
    }

    fn get(&self, location: &str) -> Option<Rc<Texture>> {
        self.entries
            .borrow_mut()
            .entry(location.to_string())
            .or_insert_with(|| Texture::from_resource(self.pack, location).map(Rc::new))
            .clone()
    }
}

/// Turns a resolved model's elements into drawable axis-aligned boxes for the
/// three camera-facing sides. Only faces present on the element are drawn.
fn collect_boxes(
    resolver: &ModelResolver,
    model: &schematic_mesher::resource_pack::BlockModel,
    quarter_turns: &[f32; 2],
    tint: Option<[u8; 3]>,
    name: &str,
    cache: &TextureCache<'_>,
    boxes: &mut Vec<BoxDraw>,
) {
    let textures = resolver.resolve_textures(model);
    for element in &model.elements {
        let mut min = element.from;
        let mut max = element.to;
        if quarter_turns != &[0.0, 0.0] {
            let Some((rotated_min, rotated_max)) = rotate_bounds(min, max, quarter_turns) else {
                return;
            };
            min = rotated_min;
            max = rotated_max;
        }
        // World-facing side -> original face texture, walked back through the
        // block rotation. UVs are taken as authored; rotations are rare and a
        // mirrored sprite is acceptable for a picker.
        let face_for = |world: Direction| -> Option<FaceDraw> {
            let original = rotate_direction(world, quarter_turns, true)?;
            let face = element.faces.get(&original)?;
            let key = face.texture.strip_prefix('#').unwrap_or(&face.texture);
            let location = textures
                .get(key)
                .map(|location| {
                    if location.contains(':') {
                        location.clone()
                    } else {
                        format!("minecraft:{location}")
                    }
                })
                .or_else(|| {
                    face.texture
                        .contains(':')
                        .then(|| face.texture.clone())
                })?;
            let texture = cache.get(&location)?;
            // Water and friends are biome-tinted outside the model format, so
            // they carry no tint index yet still need their color applied.
            let applied = if face.tintindex >= 0 || tint_without_index(name) {
                tint
            } else {
                None
            };
            Some(FaceDraw {
                texture,
                uv: face.uv.unwrap_or([0.0, 0.0, 16.0, 16.0]),
                tint: applied,
                brightness: match world {
                    Direction::Up => 1.0,
                    Direction::North => 0.8,
                    _ => 0.6,
                },
            })
        };
        boxes.push(BoxDraw {
            min,
            max,
            top: face_for(Direction::Up),
            north: face_for(Direction::North),
            west: face_for(Direction::West),
        });
    }
}

/// Rotation as quarter turns: `[x_steps_deg, y_steps_deg]` in degrees that
/// must be multiples of 90. Returns `None` for non-axis-aligned rotations.
fn quarter_turns_of(quarter_turns: &[f32; 2]) -> Option<(i32, i32)> {
    let pitch = quarter_turns[0] / 90.0;
    let yaw = quarter_turns[1] / 90.0;
    if (pitch - pitch.round()).abs() > 0.01 || (yaw - yaw.round()).abs() > 0.01 {
        return None;
    }
    Some((pitch.rem_euclid(4.0) as i32, yaw.rem_euclid(4.0) as i32))
}

fn rotate_point(point: [f32; 3], pitch: i32, yaw: i32) -> [f32; 3] {
    let [mut x, mut y, mut z] = point;
    for _ in 0..pitch.rem_euclid(4) {
        let next_y = -z;
        z = y;
        y = next_y;
    }
    for _ in 0..yaw.rem_euclid(4) {
        let next_x = z;
        z = -x;
        x = next_x;
    }
    [x, y, z]
}

fn rotate_bounds(
    min: [f32; 3],
    max: [f32; 3],
    quarter_turns: &[f32; 2],
) -> Option<([f32; 3], [f32; 3])> {
    let (pitch, yaw) = quarter_turns_of(quarter_turns)?;
    let mut corners = [[0.0f32; 3]; 8];
    for (index, corner) in corners.iter_mut().enumerate() {
        let source = [
            if index & 1 == 0 { min[0] } else { max[0] },
            if index & 2 == 0 { min[1] } else { max[1] },
            if index & 4 == 0 { min[2] } else { max[2] },
        ];
        *corner = rotate_point(source, pitch, yaw);
    }
    let mut rotated = corners[0];
    let mut opposite = corners[0];
    for corner in &corners[1..] {
        for axis in 0..3 {
            rotated[axis] = rotated[axis].min(corner[axis]);
            opposite[axis] = opposite[axis].max(corner[axis]);
        }
    }
    Some((rotated, opposite))
}

fn rotate_direction(
    direction: Direction,
    quarter_turns: &[f32; 2],
    inverse: bool,
) -> Option<Direction> {
    let (pitch, yaw) = quarter_turns_of(quarter_turns)?;
    let vector = match direction {
        Direction::Down => [0.0, -1.0, 0.0],
        Direction::Up => [0.0, 1.0, 0.0],
        Direction::North => [0.0, 0.0, -1.0],
        Direction::South => [0.0, 0.0, 1.0],
        Direction::West => [-1.0, 0.0, 0.0],
        Direction::East => [1.0, 0.0, 0.0],
    };
    // Forward is pitch then yaw, so the inverse applies yaw first.
    let rotated = if inverse {
        rotate_point(
            rotate_point(vector, 0, (-yaw).rem_euclid(4)),
            (-pitch).rem_euclid(4),
            0,
        )
    } else {
        rotate_point(vector, pitch, yaw)
    };
    match rotated {
        [0.0, -1.0, 0.0] => Some(Direction::Down),
        [0.0, 1.0, 0.0] => Some(Direction::Up),
        [0.0, 0.0, -1.0] => Some(Direction::North),
        [0.0, 0.0, 1.0] => Some(Direction::South),
        [-1.0, 0.0, 0.0] => Some(Direction::West),
        [1.0, 0.0, 0.0] => Some(Direction::East),
        _ => None,
    }
}

/// Picks the sprite texture for a flat (generated) item model.
fn flat_sprite(
    pack: &ResourcePack,
    model: &schematic_mesher::resource_pack::BlockModel,
    tint: Option<[u8; 3]>,
) -> Option<(Rc<Texture>, Option<[u8; 3]>)> {
    let resolver = ModelResolver::new(pack);
    let textures = resolver.resolve_textures(model);
    let location = ["layer0", "particle", "all", "side", "north", "up"]
        .into_iter()
        .find_map(|key| textures.get(key).cloned())
        .or_else(|| textures.values().next().cloned())?;
    let location = if location.contains(':') {
        location
    } else {
        format!("minecraft:{location}")
    };
    let texture = Rc::new(Texture::from_resource(pack, &location)?);
    Some((texture, tint))
}

/// Last-resort sprite for blocks without a usable item model: the particle
/// texture of the resolved blockstate, else any face texture it has.
fn flat_from_blockstate(pack: &ResourcePack, name: &str) -> Option<(Rc<Texture>, Option<[u8; 3]>)> {
    let resolver = ModelResolver::new(pack);
    let Ok(resolved) = resolve_block(pack, &default_input(name)) else {
        return None;
    };
    let tint = tint_for_block(name);
    for variant in &resolved {
        let textures = resolver.resolve_textures(&variant.model);
        let location = ["particle", "all", "side", "north", "up", "layer0"]
            .into_iter()
            .find_map(|key| textures.get(key).cloned())
            .or_else(|| textures.values().next().cloned())?;
        let location = if location.contains(':') {
            location
        } else {
            format!("minecraft:{location}")
        };
        if let Some(texture) = Texture::from_resource(pack, &location) {
            return Some((Rc::new(texture), tint));
        }
    }
    None
}

fn project(x: f32, y: f32, z: f32) -> (f32, f32) {
    (
        CENTER_X + (z - x) * COS_YAW * SCALE,
        CENTER_Y - (y * UNIT_Y + (x + z) * UNIT_XZ) * SCALE,
    )
}

/// Renders the boxes back-to-front for the north-west-top camera: larger
/// depth along the direction toward the camera (-1, 1, -1) is closer and
/// paints last.
fn draw_iso(boxes: &[BoxDraw]) -> RgbaImage {
    let mut image = RgbaImage::new(ICON_SIZE, ICON_SIZE);
    let mut order: Vec<&BoxDraw> = boxes.iter().collect();
    order.sort_by(|left, right| {
        let depth = |b: &BoxDraw| {
            (b.min[1] + b.max[1]) / 2.0
                - ((b.min[0] + b.max[0]) + (b.min[2] + b.max[2])) / 4.0
        };
        depth(left)
            .total_cmp(&depth(right))
            .then_with(|| left.min[1].total_cmp(&right.min[1]))
    });
    for b in &order {
        if let Some(face) = &b.top {
            paint_face(
                &mut image,
                face,
                &[
                    [b.min[0], b.max[1], b.min[2]],
                    [b.max[0], b.max[1], b.min[2]],
                    [b.max[0], b.max[1], b.max[2]],
                    [b.min[0], b.max[1], b.max[2]],
                ],
            );
        }
        if let Some(face) = &b.north {
            paint_face(
                &mut image,
                face,
                &[
                    [b.max[0], b.max[1], b.min[2]],
                    [b.min[0], b.max[1], b.min[2]],
                    [b.min[0], b.min[1], b.min[2]],
                    [b.max[0], b.min[1], b.min[2]],
                ],
            );
        }
        if let Some(face) = &b.west {
            paint_face(
                &mut image,
                face,
                &[
                    [b.min[0], b.max[1], b.min[2]],
                    [b.min[0], b.max[1], b.max[2]],
                    [b.min[0], b.min[1], b.max[2]],
                    [b.min[0], b.min[1], b.min[2]],
                ],
            );
        }
    }
    image
}

/// Paints one textured quad. `corners` are ordered (u0,v0), (u1,v0),
/// (u1,v1), (u0,v1); the projection makes every face a parallelogram, so the
/// inverse mapping is a straight 2×2 solve per pixel.
fn paint_face(image: &mut RgbaImage, face: &FaceDraw, corners: &[[f32; 3]; 4]) {
    let projected: Vec<(f32, f32)> = corners.iter().map(|c| project(c[0], c[1], c[2])).collect();
    let (Some(&(a_x, a_y)), Some(&(b_x, b_y)), Some(&(d_x, d_y))) =
        (projected.first(), projected.get(1), projected.get(3))
    else {
        return;
    };
    let eu = (b_x - a_x, b_y - a_y);
    let ev = (d_x - a_x, d_y - a_y);
    let det = eu.0 * ev.1 - eu.1 * ev.0;
    if det.abs() < 1e-6 {
        return;
    }
    let corner_c = (b_x + d_x - a_x, b_y + d_y - a_y);
    let min_x = a_x.min(b_x).min(corner_c.0).min(d_x).floor() as i32;
    let max_x = a_x.max(b_x).max(corner_c.0).max(d_x).ceil() as i32;
    let min_y = a_y.min(b_y).min(corner_c.1).min(d_y).floor() as i32;
    let max_y = a_y.max(b_y).max(corner_c.1).max(d_y).ceil() as i32;
    let width = image.width() as i32;
    let height = image.height() as i32;
    let tex_w = face.texture.width as f32;
    let tex_h = face.texture.height as f32;
    for py in min_y.max(0)..max_y.min(height) {
        for px in min_x.max(0)..max_x.min(width) {
            let (dx, dy) = (px as f32 + 0.5 - a_x, py as f32 + 0.5 - a_y);
            let u = (dx * ev.1 - dy * ev.0) / det;
            let v = (eu.0 * dy - eu.1 * dx) / det;
            if !(-0.001..=1.001).contains(&u) || !(-0.001..=1.001).contains(&v) {
                continue;
            }
            let texel_x = ((face.uv[0] + u * (face.uv[2] - face.uv[0])) / 16.0 * tex_w)
                .floor()
                .clamp(0.0, tex_w - 1.0) as u32;
            let texel_y = ((face.uv[1] + v * (face.uv[3] - face.uv[1])) / 16.0 * tex_h)
                .floor()
                .clamp(0.0, tex_h - 1.0) as u32;
            let index = ((texel_y * tex_w as u32 + texel_x) * 4) as usize;
            let Some(pixel) = face.texture.pixels.get(index..index + 4) else {
                continue;
            };
            let (mut r, mut g, mut b) = (
                f32::from(pixel[0]) * face.brightness,
                f32::from(pixel[1]) * face.brightness,
                f32::from(pixel[2]) * face.brightness,
            );
            if let Some([tr, tg, tb]) = face.tint {
                r *= f32::from(tr) / 255.0;
                g *= f32::from(tg) / 255.0;
                b *= f32::from(tb) / 255.0;
            }
            blend_pixel(image, px as u32, py as u32, [r, g, b], pixel[3]);
        }
    }
}

fn blend_pixel(image: &mut RgbaImage, x: u32, y: u32, rgb: [f32; 3], alpha: u8) {
    let source_a = f32::from(alpha) / 255.0;
    if source_a <= 0.0 {
        return;
    }
    let target = image.get_pixel_mut(x, y);
    let channels = target.0;
    let target_a = f32::from(channels[3]) / 255.0;
    let out_a = source_a + target_a * (1.0 - source_a);
    if out_a <= 0.0 {
        return;
    }
    let mix = |source: f32, target: f32| {
        ((source * source_a + target * target_a * (1.0 - source_a)) / out_a).round() as u8
    };
    *target = image::Rgba([
        mix(rgb[0], f32::from(channels[0])),
        mix(rgb[1], f32::from(channels[1])),
        mix(rgb[2], f32::from(channels[2])),
        (out_a * 255.0).round() as u8,
    ]);
}

/// Air has no texture or model; a wireframe cube keeps it recognizable and
/// selectable as the "delete these blocks" replacement target.
fn draw_air() -> RgbaImage {
    let mut image = RgbaImage::new(ICON_SIZE, ICON_SIZE);
    let corners = [
        [0.0, 0.0, 0.0],
        [16.0, 0.0, 0.0],
        [0.0, 0.0, 16.0],
        [16.0, 0.0, 16.0],
        [0.0, 16.0, 0.0],
        [16.0, 16.0, 0.0],
        [0.0, 16.0, 16.0],
        [16.0, 16.0, 16.0],
    ];
    let projected: Vec<(f32, f32)> = corners.iter().map(|c| project(c[0], c[1], c[2])).collect();
    let edges = [
        (0, 1),
        (0, 2),
        (1, 3),
        (2, 3),
        (4, 5),
        (4, 6),
        (5, 7),
        (6, 7),
        (0, 4),
        (1, 5),
        (2, 6),
        (3, 7),
    ];
    for (a, b) in edges {
        stroke_line(&mut image, projected[a], projected[b], [160, 160, 160]);
    }
    image
}

fn stroke_line(image: &mut RgbaImage, from: (f32, f32), to: (f32, f32), rgb: [u8; 3]) {
    let (dx, dy) = (to.0 - from.0, to.1 - from.1);
    let steps = dx.abs().max(dy.abs()).ceil().max(1.0) as u32;
    for step in 0..=steps {
        let t = step as f32 / steps as f32;
        let x = (from.0 + dx * t).round() as i32;
        let y = (from.1 + dy * t).round() as i32;
        if x < 0 || y < 0 || x >= image.width() as i32 || y >= image.height() as i32 {
            continue;
        }
        blend_pixel(image, x as u32, y as u32, [f32::from(rgb[0]), f32::from(rgb[1]), f32::from(rgb[2])], 255);
    }
}

/// Draws a flat sprite scaled to fill the icon, the way generated items do.
fn draw_sprite(texture: &Texture, tint: Option<[u8; 3]>) -> RgbaImage {
    let mut image = RgbaImage::new(ICON_SIZE, ICON_SIZE);
    let size = ICON_SIZE as i32;
    for py in 0..size {
        for px in 0..size {
            // Half-texel inset keeps the edges from sampling out of range.
            let u = ((px as f32 + 0.5) / size as f32 * texture.width as f32 - 0.5)
                .floor()
                .clamp(0.0, texture.width as f32 - 1.0) as u32;
            let v = ((py as f32 + 0.5) / size as f32 * texture.height as f32 - 0.5)
                .floor()
                .clamp(0.0, texture.height as f32 - 1.0) as u32;
            let index = ((v * texture.width + u) * 4) as usize;
            let Some(pixel) = texture.pixels.get(index..index + 4) else {
                continue;
            };
            let (mut r, mut g, mut b) = (
                f32::from(pixel[0]),
                f32::from(pixel[1]),
                f32::from(pixel[2]),
            );
            if let Some([tr, tg, tb]) = tint {
                r *= f32::from(tr) / 255.0;
                g *= f32::from(tg) / 255.0;
                b *= f32::from(tb) / 255.0;
            }
            blend_pixel(&mut image, px as u32, py as u32, [r, g, b], pixel[3]);
        }
    }
    image
}

fn encode(image: &RgbaImage) -> String {
    let mut png = Vec::new();
    if image
        .write_to(&mut Cursor::new(&mut png), image::ImageFormat::Png)
        .is_err()
    {
        return String::new();
    }
    format!("data:image/png;base64,{}", BASE64_STANDARD.encode(png))
}
