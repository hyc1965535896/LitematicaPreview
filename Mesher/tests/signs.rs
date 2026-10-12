//! Sign text must appear in the preview: this builds schematics with signs and
//! checks the meshes carry glyph geometry, using the pack-provided font pages
//! and the embedded unifont fallback.

use litematica_preview_native::{load_chunks, PreviewOptions};
use nucleation::block_entity::BlockEntity;
use nucleation::block_position::BlockPosition;
use nucleation::meshing::ResourcePackSource;
use nucleation::{BlockState, UniversalSchematic};
use quartz_nbt::{NbtCompound, NbtList, NbtTag};

#[path = "../src/meshing/tests/fixtures.rs"]
mod fixtures;
use fixtures::{schematic, test_pack};

fn bundled_pack() -> Vec<u8> {
    let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../Assets/pack.zip");
    std::fs::read(path).expect("read bundled resource pack")
}

/// A sign whose front side reads `lines`, formatted the way 1.20+ saves them.
fn sign_schematic(lines: &[&str], rotation: i32) -> UniversalSchematic {
    let mut model = schematic(&[(0, 0, 0, "minecraft:oak_sign")]);
    let mut state = BlockState::new("minecraft:oak_sign");
    state.set_property("rotation", rotation.to_string());
    model.set_block(0, 0, 0, &state);

    let messages: Vec<NbtTag> = lines
        .iter()
        .map(|line| NbtTag::String(format!("\"{line}\"")))
        .collect();
    let mut front = NbtCompound::new();
    front.insert("messages", NbtTag::List(NbtList::from(messages)));
    front.insert("color", NbtTag::String("black".into()));
    front.insert("has_glowing_text", NbtTag::Byte(0));
    let mut nbt = NbtCompound::new();
    nbt.insert("id", NbtTag::String("minecraft:sign".into()));
    nbt.insert("front_text", NbtTag::Compound(front));
    let mut entity = BlockEntity::from_nbt(&nbt);
    entity.position = (0, 0, 0);
    model.set_block_entity(BlockPosition { x: 0, y: 0, z: 0 }, entity);
    model
}

/// Cutout triangles and vertices for one encoded schematic: text lives in the
/// cutout layer, so its quads show up here.
fn cutout_counts(data: &[u8], pack: &ResourcePackSource) -> (usize, usize) {
    cutout_counts_with(
        data,
        pack,
        PreviewOptions {
            thread_count: None,
            ..PreviewOptions::default()
        },
    )
}

fn cutout_counts_with(
    data: &[u8],
    pack: &ResourcePackSource,
    options: PreviewOptions,
) -> (usize, usize) {
    let mut triangles = 0usize;
    let mut vertices = 0usize;
    load_chunks(data, pack, options, &[], |preview| {
        triangles += preview.mesh.cutout.triangle_count();
        vertices += preview.mesh.cutout.vertex_count();
        Ok(())
    }, |_, _| Ok(()), || Ok(())).expect("mesh the sign fixture");
    (triangles, vertices)
}

fn encoded(schematic: &UniversalSchematic) -> Vec<u8> {
    nucleation::formats::litematic::to_litematic(schematic).expect("encode the fixture")
}

#[test]
fn sign_text_renders_as_cutout_geometry() {
    let pack = ResourcePackSource::from_bytes(&bundled_pack()).expect("load the bundled pack");
    let empty = encoded(&sign_schematic(&["", "", "", ""], 0));
    let filled = encoded(&sign_schematic(&["Hello", "", "", ""], 0));
    let (board_triangles, _) = cutout_counts(&empty, &pack);
    let (text_triangles, _) = cutout_counts(&filled, &pack);
    assert!(
        text_triangles >= board_triangles + 2 * "Hello".len(),
        "each glyph must add a quad (board {board_triangles}, with text {text_triangles})"
    );
}

#[test]
fn chinese_text_comes_from_the_embedded_unifont() {
    // The bundled pack's font pages hold no CJK, so these glyphs can only come
    // from the embedded unifont table.
    let pack = ResourcePackSource::from_bytes(&bundled_pack()).expect("load the bundled pack");
    let empty = encoded(&sign_schematic(&["", "", "", ""], 0));
    let filled = encoded(&sign_schematic(&["北", "", "", ""], 0));
    let (board, _) = cutout_counts(&empty, &pack);
    let (with_text, _) = cutout_counts(&filled, &pack);
    assert!(
        with_text >= board + 2,
        "CJK text must render (board {board}, with text {with_text})"
    );
}

#[test]
fn text_never_removes_board_geometry_across_rotations() {
    let pack = ResourcePackSource::from_bytes(&bundled_pack()).expect("load the bundled pack");
    for rotation in [0, 4, 8] {
        let (board, board_vertices) = cutout_counts(&encoded(&sign_schematic(&["", "", "", ""], rotation)), &pack);
        let (text, text_vertices) = cutout_counts(&encoded(&sign_schematic(&["A", "", "", ""], rotation)), &pack);
        assert!(text >= board, "rotation {rotation}: text must not remove geometry");
        assert!(
            text_vertices > board_vertices,
            "rotation {rotation}: text must add vertices"
        );
    }
}

#[test]
fn text_renders_with_a_pack_that_ships_no_fonts() {
    let pack = test_pack();
    let (board, _) = cutout_counts(&encoded(&sign_schematic(&["", "", "", ""], 0)), &pack);
    let (text, _) = cutout_counts(&encoded(&sign_schematic(&["AB", "", "", ""], 0)), &pack);
    assert!(text >= board + 4, "text must render with a synthetic pack too");
}

#[test]
fn text_survives_chunked_multithreaded_decoding() {
    // The app's default settings decode packed litematics through the parallel
    // block walk, which has its own path for attaching text.
    let pack = ResourcePackSource::from_bytes(&bundled_pack()).expect("load the bundled pack");
    let options = PreviewOptions {
        chunk_size: Some(64),
        thread_count: Some(4),
        speed_first: true,
        ..PreviewOptions::default()
    };
    let (board, _) = cutout_counts_with(&encoded(&sign_schematic(&["", "", "", ""], 0)), &pack, options.clone());
    let (text, _) = cutout_counts_with(&encoded(&sign_schematic(&["A", "", "", ""], 0)), &pack, options);
    assert!(
        text >= board + 2,
        "chunked threaded decoding must still render sign text (board {board}, with text {text})"
    );
}
