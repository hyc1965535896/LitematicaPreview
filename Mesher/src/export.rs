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
    result.map(|data| ExportedSchematic {
        data,
        replaced,
        block_count: schematic.total_blocks() as i64,
    }).map_err(|error| format!("无法写入{}文件：{error}", format.label()))
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
