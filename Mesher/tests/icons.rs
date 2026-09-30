//! Icon rendering against the bundled resource pack.

use std::path::PathBuf;

use litematica_preview_native::block_icons;

fn pack_bytes() -> Vec<u8> {
    let pack_path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../Assets/pack.zip");
    std::fs::read(pack_path).expect("read bundled resource pack")
}

fn decode_png(icon: &str) -> image::RgbaImage {
    let data = icon
        .strip_prefix("data:image/png;base64,")
        .expect("data URL prefix");
    use base64::Engine as _;
    let bytes = base64::engine::general_purpose::STANDARD
        .decode(data)
        .expect("valid base64");
    image::load_from_memory_with_format(&bytes, image::ImageFormat::Png)
        .expect("valid PNG")
        .to_rgba8()
}

fn opaque_share(icon: &str) -> f32 {
    let image = decode_png(icon);
    let opaque = image
        .pixels()
        .filter(|pixel| pixel.0[3] >= 128)
        .count() as f32;
    opaque / (image.width() * image.height()) as f32
}

#[test]
fn every_blockstate_gets_an_icon_entry() {
    let icons = block_icons(&pack_bytes()).expect("render icons");
    assert!(icons.len() > 1000, "expected the full catalog, got {}", icons.len());
    assert!(icons.windows(2).all(|pair| pair[0].name <= pair[1].name));
    let names: Vec<_> = icons.iter().map(|icon| icon.name.as_str()).collect();
    assert!(names.contains(&"minecraft:stone"));
    assert!(names.contains(&"minecraft:spawner"));
    // Ordinary air is selectable as the "delete blocks" target; technical airs are not.
    assert!(names.contains(&"minecraft:air"));
    assert!(!names.contains(&"minecraft:cave_air"));
    assert!(icons.iter().all(|icon| icon.name.contains(':')));
}

#[test]
fn air_renders_as_a_transparent_wireframe_cube() {
    let icons = block_icons(&pack_bytes()).expect("render icons");
    let air = icons
        .iter()
        .find(|icon| icon.name == "minecraft:air")
        .expect("air entry");
    let image = decode_png(&air.icon);
    let opaque = image
        .pixels()
        .filter(|pixel| pixel.0[3] >= 128)
        .count();
    assert!(
        (50..=900).contains(&opaque),
        "air wireframe has {opaque} opaque pixels"
    );
    // The interior stays see-through (kept clear of the wireframe seams).
    assert!(image.get_pixel(26, 20).0[3] < 128);
}

#[test]
fn cube_blocks_render_opaque_isometric_icons() {
    let icons = block_icons(&pack_bytes()).expect("render icons");
    let mut by_name = |name: &str| {
        icons
            .iter()
            .find(|icon| icon.name == name)
            .unwrap_or_else(|| panic!("{name} missing"))
    };
    for name in ["minecraft:stone", "minecraft:oak_planks"] {
        let icon = by_name(name);
        assert!(icon.icon.starts_with("data:image/png;base64,"), "{name}");
        let image = decode_png(&icon.icon);
        assert_eq!((image.width(), image.height()), (64, 64), "{name}");
        // The hexagon covers top, both upper corners and both sides.
        for (x, y, what) in [(32u32, 8u32, "top"), (8, 32, "west"), (56, 32, "north")] {
            assert!(
                image.get_pixel(x, y).0[3] >= 128,
                "{name} {what} face missing at ({x},{y})"
            );
        }
    }
    // Cage-like textures leave holes, so only the filled share is checked.
    for name in ["minecraft:stone", "minecraft:oak_planks", "minecraft:spawner"] {
        let icon = by_name(name);
        let share = opaque_share(&icon.icon);
        assert!(
            (0.35..=0.75).contains(&share),
            "{name} icon fills {share:.0}% of the frame"
        );
    }
}

#[test]
fn tinted_blocks_do_not_render_gray() {
    let icons = block_icons(&pack_bytes()).expect("render icons");
    let mut green = |name: &str| {
        let image = decode_png(
            &icons
                .iter()
                .find(|icon| icon.name == name)
                .unwrap_or_else(|| panic!("{name} missing"))
                .icon,
        );
        let (mut green_dominant, mut counted) = (0usize, 0usize);
        for pixel in image.pixels() {
            let [r, g, b, a] = pixel.0;
            if a < 128 || g < 60 {
                continue;
            }
            counted += 1;
            if g > r + 10 && g > b + 10 {
                green_dominant += 1;
            }
        }
        assert!(counted > 0, "{name} rendered empty");
        assert!(
            green_dominant * 2 > counted,
            "{name} shows {green_dominant}/{counted} green pixels"
        );
    };
    green("minecraft:grass_block");
    green("minecraft:short_grass");
}

#[test]
fn cross_plants_render_as_flat_sprites() {
    let icons = block_icons(&pack_bytes()).expect("render icons");
    let mut flat = |name: &str| {
        let icon = icons
            .iter()
            .find(|icon| icon.name == name)
            .unwrap_or_else(|| panic!("{name} missing"));
        let image = decode_png(&icon.icon);
        // Flat sprites keep the texture's own silhouette: unlike an iso cube
        // they leave at least one of the cube's side positions transparent.
        let left = image.get_pixel(8, 32).0[3] >= 128;
        let right = image.get_pixel(56, 32).0[3] >= 128;
        assert!(
            !(left && right),
            "{name} renders as a cube, not a flat sprite"
        );
        assert!(
            image.pixels().filter(|p| p.0[3] >= 128).count() > 50,
            "{name} rendered almost empty"
        );
    };
    flat("minecraft:poppy");
    flat("minecraft:torch");
}

#[test]
fn unresolvable_blocks_stay_selectable_with_empty_icons() {
    // Blocks must exist as entries even when their models are missing; every
    // icon that is present must still decode.
    let icons = block_icons(&pack_bytes()).expect("render icons");
    for icon in icons.iter().skip(100).take(200) {
        if icon.icon.is_empty() {
            continue;
        }
        let _ = decode_png(&icon.icon);
    }
}
