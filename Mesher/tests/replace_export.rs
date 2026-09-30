use litematica_preview_native::{
    apply_replacements, decode, export_schematic, BlockReplacement, ExportFormat,
};

#[path = "../src/meshing/tests/fixtures.rs"]
mod fixtures;
use fixtures::schematic;

fn stone_gold_litematic() -> Vec<u8> {
    nucleation::formats::litematic::to_litematic(&schematic(&[
        (0, 0, 0, "minecraft:stone"),
        (1, 0, 0, "minecraft:gold_block"),
    ]))
    .unwrap()
}

#[test]
fn replacement_rewrites_palette_states_and_preserves_other_blocks() {
    let data = stone_gold_litematic();
    let mut schematic = decode(&data).unwrap();
    let replaced = apply_replacements(
        &mut schematic,
        &[BlockReplacement {
            from: "minecraft:stone".into(),
            to: "minecraft:quartz_block".into(),
        }],
    )
    .unwrap();
    assert_eq!(replaced, 1);
    assert_eq!(
        schematic.get_block(0, 0, 0).unwrap().name,
        "minecraft:quartz_block"
    );
    assert_eq!(
        schematic.get_block(1, 0, 0).unwrap().name,
        "minecraft:gold_block"
    );
}

#[test]
fn replacement_copies_original_state_properties() {
    let mut source = nucleation::UniversalSchematic::new("stairs".into());
    source.set_block(
        0,
        0,
        0,
        &nucleation::BlockState::new("minecraft:oak_stairs")
            .with_property("facing", "east")
            .with_property("half", "top"),
    );
    let stairs = nucleation::formats::litematic::to_litematic(&source).unwrap();
    let mut schematic = decode(&stairs).unwrap();
    apply_replacements(
        &mut schematic,
        &[BlockReplacement {
            from: "minecraft:oak_stairs".into(),
            to: "minecraft:stone_brick_stairs".into(),
        }],
    )
    .unwrap();
    let state = schematic.get_block(0, 0, 0).unwrap();
    assert_eq!(state.name, "minecraft:stone_brick_stairs");
    assert_eq!(
        state.get_property("facing").map(|value| value.as_str()),
        Some("east")
    );
    assert_eq!(
        state.get_property("half").map(|value| value.as_str()),
        Some("top")
    );
}

#[test]
fn export_applies_replacements_and_round_trips_all_writers() {
    let data = stone_gold_litematic();
    let rules = [BlockReplacement {
        from: "minecraft:stone".into(),
        to: "minecraft:quartz_block".into(),
    }];
    for format in [
        ExportFormat::Litematic,
        ExportFormat::Sponge,
        ExportFormat::StructureSnbt,
        ExportFormat::Snapshot,
    ] {
        let exported = export_schematic(&data, &rules, format).unwrap();
        assert_eq!(exported.replaced, 1, "{format:?}");
        assert_eq!(exported.block_count, 2, "{format:?}");
        let reloaded = decode(&exported.data)
            .unwrap_or_else(|error| panic!("{format:?}: {error:?}"));
        assert_eq!(
            reloaded.get_block(0, 0, 0).unwrap().name,
            "minecraft:quartz_block",
            "{format:?}"
        );
        assert_eq!(
            reloaded.get_block(1, 0, 0).unwrap().name,
            "minecraft:gold_block",
            "{format:?}"
        );
    }
}

#[test]
fn vanilla_structure_export_survives_its_own_reader() {
    let data = stone_gold_litematic();
    let exported = export_schematic(&data, &[], ExportFormat::StructureNbt).unwrap();
    assert_eq!(exported.block_count, 2);
    let reloaded = decode(&exported.data).unwrap();
    assert_eq!(reloaded.total_blocks(), 2);
    assert_eq!(
        reloaded.get_block(0, 0, 0).unwrap().name,
        "minecraft:stone"
    );
}

#[test]
fn replacements_validate_names_and_limits() {
    let data = stone_gold_litematic();
    let mut schematic = decode(&data).unwrap();
    assert!(apply_replacements(
        &mut schematic,
        &[BlockReplacement {
            from: "not-a-block".into(),
            to: "minecraft:stone".into(),
        }],
    )
    .is_err());
    assert!(apply_replacements(
        &mut schematic,
        &[BlockReplacement {
            from: "minecraft:stone".into(),
            to: "minecraft:stone".into(),
        }],
    )
    .is_err());
}
