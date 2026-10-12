//! A user-selected resource pack must re-texture the preview.
//!
//! The host layers the user's pack over the bundled vanilla pack with
//! `ResourcePackSource::from_bytes_list`, so this test builds the same layered
//! source from the real `Assets/pack.zip` plus a synthetic pack that overrides
//! one stone texture, and checks the meshed preview really uses it.

use std::io::Write;

use litematica_preview_native::{load_chunks, PreviewOptions};
use nucleation::meshing::ResourcePackSource;
use nucleation::UniversalSchematic;

#[path = "../src/meshing/tests/fixtures.rs"]
mod fixtures;
use fixtures::schematic;

const MAGENTA: [u8; 4] = [255, 0, 255, 255];

fn bundled_pack() -> Vec<u8> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../Assets/pack.zip");
    std::fs::read(path).expect("read bundled resource pack")
}

/// A pack that overrides exactly one texture, the way real packs usually do:
/// no blockstates, no models, just `assets/minecraft/textures/block/stone.png`.
fn overlay_pack(texture: &str, color: [u8; 4]) -> Vec<u8> {
    let mut pixels = Vec::with_capacity(16 * 16 * 4);
    for _ in 0..16 * 16 {
        pixels.extend_from_slice(&color);
    }
    let mut png = Vec::new();
    let image = image::RgbaImage::from_raw(16, 16, pixels).expect("16x16 RGBA image");
    image::DynamicImage::ImageRgba8(image)
        .write_to(&mut std::io::Cursor::new(&mut png), image::ImageFormat::Png)
        .expect("encode overlay texture");

    let mut archive = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
    let options: zip::write::FileOptions<()> =
        zip::write::FileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    archive
        .start_file("pack.mcmeta", options)
        .expect("start pack.mcmeta");
    archive
        .write_all(br#"{"pack":{"pack_format":42,"description":"test overlay"}}"#)
        .expect("write pack.mcmeta");
    archive
        .start_file(format!("assets/minecraft/textures/{texture}.png"), options)
        .expect("start overlay texture");
    archive.write_all(&png).expect("write overlay texture");
    archive.finish().expect("finish overlay pack").into_inner()
}

/// Every texture pixel the preview uploads for a schematic, sorted so runs with
/// a different greedy-material ordering still compare equal.
fn preview_pixels(pack: &ResourcePackSource, schematic: &UniversalSchematic) -> Vec<Vec<u8>> {
    let data = nucleation::formats::litematic::to_litematic(schematic).expect("encode fixture");
    let mut textures = Vec::new();
    load_chunks(
        &data,
        pack,
        PreviewOptions {
            thread_count: Some(2),
            ..PreviewOptions::default()
        },
        &[],
        |preview| {
            for texture in &preview.textures {
                textures.push(texture.pixels.clone());
            }
            Ok(())
        },
        |_, _| Ok(()),
        || Ok(()),
    )
    .expect("mesh the fixture");
    textures.sort();
    textures
}

fn has_color(textures: &[Vec<u8>], color: [u8; 4]) -> bool {
    textures
        .iter()
        .any(|pixels| pixels.chunks_exact(4).any(|pixel| pixel == color))
}

fn stone_preview(pack: &ResourcePackSource) -> Vec<Vec<u8>> {
    preview_pixels(pack, &schematic(&[(0, 0, 0, "minecraft:stone")]))
}

#[test]
fn a_custom_pack_overrides_the_bundled_texture_and_leaves_missing_ones_vanilla() {
    let vanilla_bytes = bundled_pack();
    let vanilla = ResourcePackSource::from_bytes(&vanilla_bytes).expect("load bundled pack");
    let overlaid =
        ResourcePackSource::from_bytes_list([vanilla_bytes.clone(), overlay_pack("block/stone", MAGENTA)])
            .expect("layer a custom pack over vanilla");
    let vanilla_overlaid =
        ResourcePackSource::from_bytes_list([vanilla_bytes, overlay_pack("block/dirt", MAGENTA)])
            .expect("layer a custom pack that misses stone");

    // Baseline: the bundled pack alone never renders the overlay colour.
    assert!(!has_color(&stone_preview(&vanilla), MAGENTA));

    // The overlaying pack's texture wins over the bundled one.
    assert!(
        has_color(&stone_preview(&overlaid), MAGENTA),
        "the custom pack's stone texture must replace the bundled one"
    );

    // A pack that does not mention stone must still fall through to vanilla,
    // which is what makes single-block packs usable without a full copy.
    assert!(
        !has_color(&stone_preview(&vanilla_overlaid), MAGENTA),
        "unrelated textures must keep falling back to the bundled pack"
    );
}

#[test]
fn several_packs_layer_in_minecraft_order_with_the_last_winning() {
    // Mirrors the decoder: the bundled pack is the base, then the user's
    // selection is applied in order, so a later pack overrides an earlier one —
    // the same relative priority the resource-pack screen shows.
    let blue = [0, 0, 255, 255];
    let green = [0, 255, 0, 255];
    let vanilla_bytes = bundled_pack();

    let last_wins = ResourcePackSource::from_bytes_list([
        vanilla_bytes.clone(),
        overlay_pack("block/stone", MAGENTA),
        overlay_pack("block/stone", blue),
    ])
    .expect("layer two packs that both define stone");
    let pixels = stone_preview(&last_wins);
    assert!(has_color(&pixels, blue), "the later pack must win the conflict");
    assert!(
        !has_color(&pixels, MAGENTA),
        "the overridden pack's texture must not appear anywhere in the atlas"
    );

    // Reversing the selection reverses the winner.
    let other_wins = ResourcePackSource::from_bytes_list([
        vanilla_bytes,
        overlay_pack("block/stone", blue),
        overlay_pack("block/stone", MAGENTA),
    ])
    .expect("layer the same two packs in the other order");
    assert!(has_color(&stone_preview(&other_wins), MAGENTA));

    // Disjoint packs combine: stone from one, dirt from the other, both visible.
    let combined = ResourcePackSource::from_bytes_list([
        bundled_pack(),
        overlay_pack("block/stone", MAGENTA),
        overlay_pack("block/dirt", green),
    ])
    .expect("layer packs that cover different blocks");
    assert!(has_color(&stone_preview(&combined), MAGENTA));
    let dirt = preview_pixels(&combined, &schematic(&[(0, 0, 0, "minecraft:dirt")]));
    assert!(has_color(&dirt, green), "the second pack's dirt texture must apply");
}

#[test]
fn an_unrelated_overlay_leaves_other_blocks_untouched() {

    // Real packs rarely ship blockstates or models, so the blocks they do not
    // mention must keep meshing from vanilla's definitions and textures.
    let vanilla_bytes = bundled_pack();
    let vanilla = ResourcePackSource::from_bytes(&vanilla_bytes).expect("load bundled pack");
    let pack =
        ResourcePackSource::from_bytes_list([vanilla_bytes, overlay_pack("block/stone", MAGENTA)])
            .expect("layer a custom pack over vanilla");
    for name in ["minecraft:oak_planks", "minecraft:furnace", "minecraft:birch_planks"] {
        let block = schematic(&[(0, 0, 0, name)]);
        let expected = preview_pixels(&vanilla, &block);
        assert!(!expected.is_empty(), "{name} must render in vanilla");
        assert_eq!(
            preview_pixels(&pack, &block),
            expected,
            "an overlay that misses {name} must not change how it renders"
        );
    }
}
