//! Every resource pack the user points the app at must layer over the bundled
//! vanilla pack without losing blockstates, and block-texture packs must
//! actually change what the preview uploads.
//!
//! Set `LP_RESOURCE_PACK_DIR` to a folder of pack `.zip` files to run this
//! against real downloads; the test is skipped when that folder is absent, so
//! it stays inert in CI.

use nucleation::meshing::ResourcePackSource;

#[path = "../src/meshing/tests/fixtures.rs"]
mod fixtures;
use fixtures::schematic;

/// Vanilla block textures a typical block pack overrides.
const PROBE_TEXTURES: [&str; 6] = [
    "block/stone",
    "block/oak_planks",
    "block/glass",
    "block/anvil",
    "block/deepslate",
    "block/bricks",
];

fn bundled_pack() -> Vec<u8> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../Assets/pack.zip");
    std::fs::read(path).expect("read bundled resource pack")
}

/// A mesh-relevant fingerprint of the textures the preview would upload.
fn texture_fingerprint(pack: &ResourcePackSource, texture: &str) -> Option<u64> {
    let pixels = pack.get_texture_pixels(texture)?;
    let mut hash = 0xcbf29ce484222325u64;
    for byte in pixels {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    Some(hash)
}

#[test]
fn local_resource_packs_layer_over_the_bundled_pack() {
    let Some(directory) = std::env::var_os("LP_RESOURCE_PACK_DIR").map(std::path::PathBuf::from)
    else {
        eprintln!("skipping: set LP_RESOURCE_PACK_DIR to a folder of pack zips");
        return;
    };
    if !directory.is_dir() {
        eprintln!("skipping: {} is not a directory", directory.display());
        return;
    }

    let vanilla_bytes = bundled_pack();
    let vanilla = ResourcePackSource::from_bytes(&vanilla_bytes).expect("load bundled pack");
    let baseline: Vec<Option<u64>> = PROBE_TEXTURES
        .iter()
        .map(|texture| texture_fingerprint(&vanilla, texture))
        .collect();

    let stone_blockstate = vanilla
        .get_blockstate_json("minecraft:stone")
        .expect("bundled pack defines stone");

    let mut pack_paths: Vec<std::path::PathBuf> = std::fs::read_dir(&directory)
        .expect("read the pack folder")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .filter(|path| {
            path.extension()
                .is_some_and(|extension| extension.eq_ignore_ascii_case("zip"))
        })
        .collect();
    pack_paths.sort();
    assert!(!pack_paths.is_empty(), "no resource packs found");

    let mut textured = Vec::new();
    for path in pack_paths {
        let name = path
            .file_name()
            .unwrap_or_default()
            .to_string_lossy()
            .to_string();
        let bytes = std::fs::read(&path).expect("read the resource pack");
        let layered = match ResourcePackSource::from_bytes_list([vanilla_bytes.clone(), bytes]) {
            Ok(layered) => layered,
            Err(error) => panic!("{name}: layering over the bundled pack failed: {error}"),
        };

        // Vanilla blockstates must survive any overlay, or blocks stop meshing.
        let overlaid_stone = layered
            .get_blockstate_json("minecraft:stone")
            .unwrap_or_else(|| panic!("{name}: stone blockstate vanished after overlaying"));
        assert_eq!(
            overlaid_stone, stone_blockstate,
            "{name} must not silently rewrite vanilla's stone blockstate"
        );

        let changed: Vec<&str> = PROBE_TEXTURES
            .iter()
            .zip(&baseline)
            .filter(|(texture, before)| texture_fingerprint(&layered, texture) != **before)
            .map(|(texture, _)| *texture)
            .collect();
        println!("{name}: changed {changed:?}");

        // Blocks it does not cover must still resolve to the bundled texture.
        for (texture, before) in PROBE_TEXTURES.iter().zip(&baseline) {
            if !changed.contains(texture) {
                assert_eq!(
                    texture_fingerprint(&layered, texture),
                    *before,
                    "{name}: unrelated texture {texture} changed"
                );
            }
        }
        if !changed.is_empty() {
            textured.push((name, changed));
        }
    }

    // A preview that shows no block-texture packs would make this test useless.
    assert!(
        !textured.is_empty(),
        "no pack in {} changed a block texture; expected at least one",
        directory.display()
    );
    for (name, changed) in &textured {
        println!("block textures from {name}: {changed:?}");
    }

    // And the whole path end to end: a layered pack must still mesh a schematic
    // whose blocks come from vanilla definitions.
    let first = &textured[0];
    let bytes = std::fs::read(directory.join(&first.0)).expect("re-read the textured pack");
    let layered = ResourcePackSource::from_bytes_list([vanilla_bytes, bytes])
        .expect("layer the textured pack");
    let data = nucleation::formats::litematic::to_litematic(&schematic(&[
        (0, 0, 0, "minecraft:stone"),
        (1, 0, 0, "minecraft:oak_planks"),
        (0, 1, 0, "minecraft:glass"),
    ]))
    .expect("encode the fixture");
    let mut textures = 0;
    litematica_preview_native::load_chunks(
        &data,
        &layered,
        litematica_preview_native::PreviewOptions {
            thread_count: Some(2),
            ..litematica_preview_native::PreviewOptions::default()
        },
        &[],
        |preview| {
            textures += preview.textures.len();
            Ok(())
        },
        |_, _| Ok(()),
        || Ok(()),
    )
    .expect("mesh with a layered pack");
    assert!(textures > 0, "a layered pack produced no textures");
}
