//! Export must place blocks where the user pastes them.
//!
//! Litematica computes a paste position as `origin + Region.Position + local`,
//! so a file whose `Position` holds an absolute world coordinate places its
//! blocks thousands of blocks away and renders as empty ground. Exports repair
//! that, while leaving legitimate small sub-region offsets alone.

use litematica_preview_native::{export_schematic, ExportFormat};
use nucleation::{BlockState, UniversalSchematic};
use quartz_nbt::{io, NbtCompound, NbtTag};

fn trapdoor_schematic(length: i32) -> UniversalSchematic {
    let mut model = UniversalSchematic::new("positions".into());
    let mut state = BlockState::new("minecraft:iron_trapdoor");
    state.set_property("facing", "east");
    for z in 0..length {
        model.set_block(0, 0, z, &state);
    }
    model
}

/// Rewrites the region origin (and optionally size) in the raw NBT, the way a
/// generator that used world coordinates would write it.
fn with_region_origin(
    bytes: &[u8],
    position: (i32, i32, i32),
    size: Option<(i32, i32, i32)>,
) -> Vec<u8> {
    let (mut root, _) =
        io::read_nbt(&mut std::io::Cursor::new(bytes), io::Flavor::GzCompressed).unwrap();
    let regions = root.get_mut::<_, &mut NbtCompound>("Regions").unwrap();
    for (_, region) in regions.inner_mut() {
        let NbtTag::Compound(region) = region else {
            continue;
        };
        let mut pos = NbtCompound::new();
        pos.insert("x", NbtTag::Int(position.0));
        pos.insert("y", NbtTag::Int(position.1));
        pos.insert("z", NbtTag::Int(position.2));
        region.insert("Position", NbtTag::Compound(pos));
        if let Some((x, y, z)) = size {
            let mut size = NbtCompound::new();
            size.insert("x", NbtTag::Int(x));
            size.insert("y", NbtTag::Int(y));
            size.insert("z", NbtTag::Int(z));
            region.insert("Size", NbtTag::Compound(size));
        }
    }
    let mut out = Vec::new();
    io::write_nbt(&mut out, None, &root, io::Flavor::GzCompressed).unwrap();
    out
}

/// The regions of an exported litematic as `(position, size)` per region name.
fn exported_regions(bytes: &[u8]) -> Vec<((i32, i32, i32), (i32, i32, i32))> {
    let (root, _) = io::read_nbt(&mut std::io::Cursor::new(bytes), io::Flavor::GzCompressed).unwrap();
    let regions = root.get::<_, &NbtCompound>("Regions").unwrap();
    let mut out = Vec::new();
    for (_, region) in regions.inner() {
        let NbtTag::Compound(region) = region else {
            continue;
        };
        let pos = region.get::<_, &NbtCompound>("Position").unwrap();
        let size = region.get::<_, &NbtCompound>("Size").unwrap();
        out.push((
            (
                pos.get::<_, i32>("x").unwrap(),
                pos.get::<_, i32>("y").unwrap(),
                pos.get::<_, i32>("z").unwrap(),
            ),
            (
                size.get::<_, i32>("x").unwrap(),
                size.get::<_, i32>("y").unwrap(),
                size.get::<_, i32>("z").unwrap(),
            ),
        ));
    }
    out
}

#[test]
fn an_absolute_world_origin_is_written_back_as_relative() {
    let model = trapdoor_schematic(58);
    let bytes = nucleation::formats::litematic::to_litematic(&model).unwrap();
    // The shape of the files that place blank: absolute origin, negative Z size.
    let broken = with_region_origin(&bytes, (-18334, -22, 14485), Some((1, 1, -58)));

    let exported = export_schematic(&broken, &[], ExportFormat::Litematic).unwrap();
    assert_eq!(exported.repaired_regions, 1, "the export must repair the origin");
    let regions = exported_regions(&exported.data);
    assert_eq!(regions.len(), 1);
    assert_eq!(regions[0].0, (0, 0, 0), "relative origin expected");
    assert_eq!(regions[0].1, (1, 1, 58), "the negative size becomes positive");
}

#[test]
fn a_small_sub_region_offset_is_left_alone() {
    let model = trapdoor_schematic(3);
    let bytes = nucleation::formats::litematic::to_litematic(&model).unwrap();
    let offset = with_region_origin(&bytes, (2, 0, 5), None);

    let exported = export_schematic(&offset, &[], ExportFormat::Litematic).unwrap();
    assert_eq!(exported.repaired_regions, 0, "small offsets are legitimate geometry");
    assert_eq!(exported_regions(&exported.data)[0].0, (2, 0, 5));
}

#[test]
fn the_other_writers_still_export_a_broken_origin_file() {
    // Only the Litematica output places blocks by `Region.Position`; the
    // structure writers use one origin for the whole schematic. They must still
    // serialize such a file rather than fail on it.
    let model = trapdoor_schematic(4);
    let bytes = nucleation::formats::litematic::to_litematic(&model).unwrap();
    let broken = with_region_origin(&bytes, (90000, -5, -70000), None);
    for format in [
        ExportFormat::Sponge,
        ExportFormat::StructureNbt,
        ExportFormat::McStructure,
    ] {
        let exported = export_schematic(&broken, &[], format).unwrap();
        assert!(!exported.data.is_empty(), "{format:?} produced no data");
    }
}
