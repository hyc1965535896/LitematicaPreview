//! Schematic re-serialization for exporting replaced previews as new files.
//! Nucleation provides writers for most formats; the vanilla structure block
//! binary writer is implemented here on top of quartz NBT.

use std::collections::BTreeMap;

use nucleation::block_position::BlockPosition;
use nucleation::formats::{litematic, mcstructure, schematic, snapshot, structure_snbt};
use nucleation::UniversalSchematic;
use quartz_nbt::{NbtCompound, NbtList, NbtTag};

use crate::replace::{apply_replacements, validate_replacements, BlockReplacement};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ExportFormat {
    Litematic,
    Sponge,
    StructureNbt,
    StructureSnbt,
    McStructure,
    Snapshot,
}

impl ExportFormat {
    /// Map a file extension (without the dot) to an export writer.
    pub fn from_extension(extension: &str) -> Option<Self> {
        Some(match extension.to_ascii_lowercase().as_str() {
            "litematic" => Self::Litematic,
            "schem" => Self::Sponge,
            "nbt" => Self::StructureNbt,
            "snbt" => Self::StructureSnbt,
            "mcstructure" => Self::McStructure,
            "nusn" => Self::Snapshot,
            // Legacy MCEdit numeric-id files have no writer; convert to Sponge.
            "schematic" => Self::Sponge,
            _ => return None,
        })
    }

    pub fn extension(self) -> &'static str {
        match self {
            Self::Litematic => "litematic",
            Self::Sponge => "schem",
            Self::StructureNbt => "nbt",
            Self::StructureSnbt => "snbt",
            Self::McStructure => "mcstructure",
            Self::Snapshot => "nusn",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Litematic => "Litematica",
            Self::Sponge => "Sponge",
            Self::StructureNbt => "结构方块",
            Self::StructureSnbt => "结构 SNBT",
            Self::McStructure => "基岩版结构",
            Self::Snapshot => "Nucleation 快照",
        }
    }
}

/// A serialized schematic plus the counts gathered during its decode.
pub struct ExportedSchematic {
    pub data: Vec<u8>,
    pub replaced: i64,
    pub block_count: i64,
    /// Regions whose absolute world origin was rewritten as a relative offset.
    pub repaired_regions: i64,
}

/// A region saved from an in-world selection can carry the absolute world
/// position instead of an offset from the schematic's own origin. Litematica
/// places blocks at `paste origin + Position + local`, so such a file lands
/// thousands of blocks away from where it was pasted and looks like nothing was
/// placed. Small offsets are legitimate sub-region placement and stay put; a
/// position this far from the origin cannot be one.
const ABSOLUTE_ORIGIN_LIMIT: i32 = 1024;

/// Rewrites plainly absolute region origins in a serialized litematic as
/// offsets from the schematic's own minimum corner. Returns how many regions
/// moved. Works on the written NBT because the writer re-derives each origin
/// from the region's tight content bounds, not from `Region.position`.
fn repair_serialized_origins(data: Vec<u8>) -> Result<(Vec<u8>, i64), String> {
    let mut cursor = std::io::Cursor::new(&data);
    let Ok((mut root, _)) =
        quartz_nbt::io::read_nbt(&mut cursor, quartz_nbt::io::Flavor::GzCompressed)
    else {
        return Ok((data, 0));
    };
    let mut positions: Vec<(i32, i32, i32)> = Vec::new();
    if let Ok(regions) = root.get::<_, &NbtCompound>("Regions") {
        for (_, region) in regions.inner() {
            let NbtTag::Compound(region) = region else {
                continue;
            };
            let Ok(pos) = region.get::<_, &NbtCompound>("Position") else {
                continue;
            };
            let (Ok(x), Ok(y), Ok(z)) = (
                pos.get::<_, i32>("x"),
                pos.get::<_, i32>("y"),
                pos.get::<_, i32>("z"),
            ) else {
                continue;
            };
            positions.push((x, y, z));
        }
    }
    let absolute = positions.iter().any(|(x, y, z)| {
        x.abs() > ABSOLUTE_ORIGIN_LIMIT
            || y.abs() > ABSOLUTE_ORIGIN_LIMIT
            || z.abs() > ABSOLUTE_ORIGIN_LIMIT
    });
    if !absolute || positions.is_empty() {
        return Ok((data, 0));
    }
    // Shift the whole schematic, not each region on its own: the offsets between
    // regions are real geometry, even when every region also carries the world
    // origin baked into its position.
    let mut min = (i32::MAX, i32::MAX, i32::MAX);
    for (x, y, z) in &positions {
        min.0 = min.0.min(*x);
        min.1 = min.1.min(*y);
        min.2 = min.2.min(*z);
    }
    let mut repaired = 0;
    if let Ok(regions) = root.get_mut::<_, &mut NbtCompound>("Regions") {
        for (_, region) in regions.inner_mut() {
            let NbtTag::Compound(region) = region else {
                continue;
            };
            let Ok(pos) = region.get_mut::<_, &mut NbtCompound>("Position") else {
                continue;
            };
            let (Ok(x), Ok(y), Ok(z)) = (
                pos.get::<_, i32>("x"),
                pos.get::<_, i32>("y"),
                pos.get::<_, i32>("z"),
            ) else {
                continue;
            };
            let shifted = (x - min.0, y - min.1, z - min.2);
            if shifted == (x, y, z) {
                continue;
            }
            pos.insert("x", NbtTag::Int(shifted.0));
            pos.insert("y", NbtTag::Int(shifted.1));
            pos.insert("z", NbtTag::Int(shifted.2));
            repaired += 1;
        }
    }
    if repaired == 0 {
        return Ok((data, 0));
    }
    let mut out = Vec::new();
    quartz_nbt::io::write_nbt(&mut out, None, &root, quartz_nbt::io::Flavor::GzCompressed)
        .map_err(|error| format!("无法写入修正后的区域坐标：{error}"))?;
    Ok((out, repaired))
}

/// Decode, apply material replacements, and serialize to the target format.
pub fn export_schematic(
    bytes: &[u8],
    replacements: &[BlockReplacement],
    format: ExportFormat,
) -> Result<ExportedSchematic, String> {
    validate_replacements(replacements)?;
    let mut schematic = crate::decode::decode(bytes).map_err(|error| match error {
        crate::decode::DecodeFailure::Format(message) | crate::decode::DecodeFailure::Limit(message) => {
            message
        }
    })?;
    let replaced = apply_replacements(&mut schematic, replacements)?;
    if schematic.total_blocks() == 0 {
        return Err("投影文件不包含可见的几何体。".into());
    }
    let result: Result<Vec<u8>, String> = match format {
        ExportFormat::Litematic => litematic::to_litematic(&schematic).map_err(|e| e.to_string()),
        ExportFormat::Sponge => schematic::to_schematic(&schematic).map_err(|e| e.to_string()),
        ExportFormat::StructureSnbt => {
            structure_snbt::to_structure_snbt(&schematic).map_err(|e| e.to_string())
        }
        ExportFormat::McStructure => {
            mcstructure::to_mcstructure(&schematic).map_err(|e| e.to_string())
        }
        ExportFormat::Snapshot => snapshot::to_snapshot(&schematic).map_err(|e| e.to_string()),
        ExportFormat::StructureNbt => write_vanilla_structure(&schematic),
    };
    // Files saved from a world selection sometimes carry absolute coordinates
    // in `Region.Position`; placed in game they land thousands of blocks away.
    // Litematica's own format is the one that places blocks, so that output is
    // rewritten to relative origins; the structure formats below have a single
    // origin and are already relative.
    let (data, repaired_regions) = match (result, format) {
        (Ok(data), ExportFormat::Litematic) => repair_serialized_origins(data)?,
        (Ok(data), _) => (data, 0),
        (Err(error), _) => return Err(format!("无法写入{}文件：{error}", format.label())),
    };
    Ok(ExportedSchematic {
        data,
        replaced,
        block_count: schematic.total_blocks() as i64,
        repaired_regions,
    })
}

/// Serialize to the vanilla structure block binary format. Structure files
/// cover one region, so all schematic regions are merged over their combined
/// bounding box; every position is stored, including air.
fn write_vanilla_structure(schematic: &UniversalSchematic) -> Result<Vec<u8>, String> {
    let bounds = schematic.get_bounding_box();
    let size = [
        bounds.max.0 - bounds.min.0 + 1,
        bounds.max.1 - bounds.min.1 + 1,
        bounds.max.2 - bounds.min.2 + 1,
    ];
    if size.iter().any(|&axis| axis <= 0 || axis > 48 * 1024) {
        return Err("结构尺寸超出结构方块的表示范围。".into());
    }
    let mut palette: Vec<NbtCompound> = Vec::new();
    let mut palette_index: BTreeMap<String, usize> = BTreeMap::new();
    let mut blocks = NbtList::new();
    let mut volume = 0usize;
    for y in bounds.min.1..=bounds.max.1 {
        for z in bounds.min.2..=bounds.max.2 {
            for x in bounds.min.0..=bounds.max.0 {
                volume += 1;
                let state = schematic
                    .get_block(x, y, z)
                    .cloned()
                    .unwrap_or_else(|| nucleation::BlockState::new("minecraft:air"));
                let key = format_block_state(&state);
                let index = match palette_index.get(&key) {
                    Some(&index) => index,
                    None => {
                        let mut entry = NbtCompound::new();
                        entry.insert("Name", state.name.to_string());
                        if !state.properties.is_empty() {
                        let mut properties = NbtCompound::new();
                        for (key, value) in &state.properties {
                            properties.insert(key.as_str(), value.to_string());
                        }
                            entry.insert("Properties", properties);
                        }
                        palette.push(entry);
                        palette_index.insert(key, palette.len() - 1);
                        palette.len() - 1
                    }
                };
                let mut block = NbtCompound::new();
                block.insert("state", NbtTag::Int(index as i32));
                block.insert(
                    "pos",
                    NbtList::from(vec![
                        NbtTag::Int(x - bounds.min.0),
                        NbtTag::Int(y - bounds.min.1),
                        NbtTag::Int(z - bounds.min.2),
                    ]),
                );
                if let Some(block_entity) =
                    schematic.get_block_entity_owned(BlockPosition { x, y, z })
                {
                    let mut nbt = block_entity.nbt.to_quartz_nbt();
                    if !nbt.contains_key("id") && !nbt.contains_key("Id") {
                        nbt.insert("id", NbtTag::String(block_entity.id.clone()));
                    }
                    block.insert("nbt", nbt);
                }
                blocks.push(block);
            }
        }
    }
    if volume == 0 {
        return Err("投影文件不包含可见的几何体。".into());
    }
    let mut entities = NbtList::new();
    for entity in schematic.get_entities_as_list() {
        let mut nbt = NbtCompound::new();
        for (key, value) in &entity.nbt {
            nbt.insert(key, nbt_value_to_quartz(value));
        }
        if !nbt.contains_key("id") && !nbt.contains_key("Id") {
            nbt.insert("id", NbtTag::String(entity.id.clone()));
        }
        let mut entry = NbtCompound::new();
        entry.insert(
            "pos",
            NbtList::from(vec![
                NbtTag::Double(entity.position.0 - f64::from(bounds.min.0)),
                NbtTag::Double(entity.position.1 - f64::from(bounds.min.1)),
                NbtTag::Double(entity.position.2 - f64::from(bounds.min.2)),
            ]),
        );
        entry.insert("nbt", nbt);
        entities.push(entry);
    }
    let mut root = NbtCompound::new();
    root.insert(
        "size",
        NbtList::from(size.into_iter().map(NbtTag::Int).collect::<Vec<_>>()),
    );
    root.insert("palette", NbtList::from(palette.into_iter().map(NbtTag::Compound).collect::<Vec<_>>()));
    root.insert("blocks", blocks);
    root.insert("entities", entities);
    root.insert(
        "DataVersion",
        NbtTag::Int(schematic.metadata.source_data_version.unwrap_or(3955)),
    );
    let mut bytes = Vec::new();
    quartz_nbt::io::write_nbt(
        &mut bytes,
        None,
        &root,
        quartz_nbt::io::Flavor::GzCompressed,
    )
    .map_err(|e| format!("无法编码结构方块数据：{e}"))?;
    Ok(bytes)
}

fn format_block_state(state: &nucleation::BlockState) -> String {
    if state.properties.is_empty() {
        return state.name.to_string();
    }
    let mut properties: Vec<String> = state
        .properties
        .iter()
        .map(|(key, value)| format!("{key}={value}"))
        .collect();
    properties.sort();
    format!("{}[{}]", state.name, properties.join(","))
}

/// Convert the entity crate's NBT values into quartz NBT tags.
fn nbt_value_to_quartz(value: &nucleation::NbtValue) -> NbtTag {
    use nucleation::NbtValue;
    match value {
        NbtValue::String(v) => NbtTag::String(v.clone()),
        NbtValue::Int(v) => NbtTag::Int(*v),
        NbtValue::Long(v) => NbtTag::Long(*v),
        NbtValue::Float(v) => NbtTag::Float(*v),
        NbtValue::Double(v) => NbtTag::Double(*v),
        NbtValue::Byte(v) => NbtTag::Byte(*v),
        NbtValue::Short(v) => NbtTag::Short(*v),
        NbtValue::Boolean(v) => NbtTag::Byte(i8::from(*v)),
        NbtValue::IntArray(v) => NbtTag::IntArray(v.clone()),
        NbtValue::LongArray(v) => NbtTag::LongArray(v.clone()),
        NbtValue::ByteArray(v) => NbtTag::ByteArray(v.clone()),
        NbtValue::List(v) => {
            NbtList::from(v.iter().map(nbt_value_to_quartz).collect::<Vec<_>>()).into()
        }
        NbtValue::Compound(map) => {
            let mut compound = NbtCompound::new();
            for (key, value) in map {
                compound.insert(key, nbt_value_to_quartz(value));
            }
            NbtTag::Compound(compound)
        }
    }
}
