use nucleation::formats::limits::DecodeLimits;
use nucleation::formats::{
    classic_schematic, manager::SchematicImporter, mcstructure, snapshot, structure_snbt, world,
};
use nucleation::UniversalSchematic;
use regex::Regex;

use crate::meshing::CompactBlocks;

#[path = "bounded_nbt.rs"]
mod bounded_nbt;
#[path = "litematic.rs"]
mod litematic;
#[path = "schematic.rs"]
mod schematic;
#[path = "structure_nbt.rs"]
mod structure_nbt;

/// Bound content by representation rather than application preview quota.
/// Litematic preview streams packed states; other formats use bounded dense readers.
/// Recursive NBT parsing retains an independent stack-safety bound.
const MAX_NBT_DEPTH: usize = 64;

pub fn preview_limits() -> DecodeLimits {
    DecodeLimits {
        max_input_bytes: isize::MAX as usize,
        max_decompressed_bytes: isize::MAX as usize,
        max_dimension: i32::MAX as usize,
        max_volume: isize::MAX as usize / std::mem::size_of::<usize>(),
        max_regions: usize::MAX,
        max_palette_entries: usize::MAX,
        max_entities: usize::MAX,
        max_block_entities: usize::MAX,
        max_nbt_depth: MAX_NBT_DEPTH,
        max_nbt_string_bytes: isize::MAX as usize,
        max_nbt_collection_items: isize::MAX as usize,
        max_nbt_nodes: usize::MAX,
    }
}

/// Rewrite brace-form block states in structure SNBT `state` values, such as
/// `state: "minecraft:oak_log{axis=y}"`, into bracket form for one retry.
/// Other document fields are unchanged; `None` skips the retry.
fn normalize_structure_snbt(bytes: &[u8]) -> Option<Vec<u8>> {
    static BRACE_STATE: std::sync::LazyLock<Regex> = std::sync::LazyLock::new(|| {
        Regex::new(
            r#"state:\s*"([\w.-]+:[\w/.-]+)\{([\w.-]+[=:][\w.\-/]+(?:,[\w.-]+[=:][\w.\-/]+)*)\}""#,
        )
        .expect("brace-state pattern is static")
    });
    let text = std::str::from_utf8(bytes).ok()?;
    let brace_state = &*BRACE_STATE;

    if !brace_state.is_match(text) {
        return None;
    }
    Some(
        brace_state
            .replace_all(text, r#"state:"$1[$2]""#)
            .into_owned()
            .into_bytes(),
    )
}

#[derive(Debug)]
pub enum DecodeFailure {
    Format(String),
    Limit(String),
}

/// Decode a schematic with structural, addressability, and stack-safety checks,
/// including the supported compatibility fallbacks.
pub fn decode(bytes: &[u8]) -> Result<UniversalSchematic, DecodeFailure> {
    let limits = preview_limits();
    limits
        .check_input(bytes)
        .map_err(|error| DecodeFailure::Limit(error.to_string()))?;

    if let Ok(schematic) = read_bounded(bytes, &limits) {
        return Ok(schematic);
    }

    if let Some(normalized) = normalize_structure_snbt(bytes) {
        if let Ok(schematic) = read_bounded(&normalized, &limits) {
            return Ok(schematic);
        }
    }
    if let Some(result) = structure_nbt::try_load(bytes, &limits) {
        return result;
    }
    Err(DecodeFailure::Format(
        "此文件不是可读取的 Minecraft 投影文件，或其数据超出了受支持的表示或嵌套上限。".into(),
    ))
}

pub(crate) fn decode_preview(
    bytes: &[u8],
    chunk_size: Option<i32>,
    thread_count: Option<u8>,
    speed_first: bool,
    replacements: &[crate::replace::BlockReplacement],
    current: &impl Fn() -> Result<(), String>,
) -> Result<(CompactBlocks, i64), String> {
    let limits = preview_limits();
    limits
        .check_input(bytes)
        .map_err(|error| error.to_string())?;
    // Replacements need the decoded model, so the packed litematic streaming
    // path is skipped in favor of the bounded dense readers.
    if !replacements.is_empty() {
        let mut schematic = decode(bytes).map_err(|error| match error {
            DecodeFailure::Format(message) | DecodeFailure::Limit(message) => message,
        })?;
        current()?;
        let replaced = crate::replace::apply_replacements(&mut schematic, replacements)?;
        current()?;
        let source = CompactBlocks::from_schematic(
            schematic,
            chunk_size,
            thread_count,
            speed_first,
            current,
        )?;
        return Ok((source, replaced));
    }
    if let Some(source) = litematic::read_compact(
        bytes,
        &limits,
        chunk_size,
        thread_count,
        speed_first,
        current,
    )? {
        return Ok((source, 0));
    }
    current()?;
    let result = read_other_bounded(bytes, &limits);
    current()?;
    if let Ok(schematic) = result {
        return Ok((
            CompactBlocks::from_schematic(schematic, chunk_size, thread_count, speed_first, current)?,
            0,
        ));
    }
    if let Some(normalized) = normalize_structure_snbt(bytes) {
        let result = read_other_bounded(&normalized, &limits);
        current()?;
        if let Ok(schematic) = result {
            return Ok((
                CompactBlocks::from_schematic(schematic, chunk_size, thread_count, speed_first, current)?,
                0,
            ));
        }
    }
    let result = structure_nbt::try_load(bytes, &limits);
    current()?;
    if let Some(result) = result {
        let schematic = result.map_err(|error| match error {
            DecodeFailure::Format(message) | DecodeFailure::Limit(message) => message,
        })?;
        return Ok((
            CompactBlocks::from_schematic(schematic, chunk_size, thread_count, speed_first, current)?,
            0,
        ));
    }
    Err("此文件不是可读取的 Minecraft 投影文件，或其数据超出了受支持的表示或嵌套上限。".into())
}

fn read_bounded(bytes: &[u8], limits: &DecodeLimits) -> Result<UniversalSchematic, String> {
    limits
        .check_input(bytes)
        .map_err(|error| error.to_string())?;
    if let Ok(schematic) = litematic::read(bytes, limits) {
        return Ok(schematic);
    }
    read_other_bounded(bytes, limits)
}

/// Read non-Litematic formats without allocating a dense Litematic region.
fn read_other_bounded(bytes: &[u8], limits: &DecodeLimits) -> Result<UniversalSchematic, String> {
    limits
        .check_input(bytes)
        .map_err(|error| error.to_string())?;
    if let Ok(schematic) = schematic::read(bytes, limits) {
        return Ok(schematic);
    }
    if let Ok(schematic) = mcstructure::from_mcstructure_bounded(bytes, limits) {
        return Ok(schematic);
    }
    // Header-detected formats keep their terminal read-error semantics.
    if snapshot::SnapshotFormat.detect_bounded(bytes, limits) {
        return snapshot::from_snapshot_bounded(bytes, limits).map_err(|error| error.to_string());
    }
    if let Ok(schematic) = structure_snbt::from_structure_snbt_bounded(bytes, limits) {
        return Ok(schematic);
    }
    if let Ok(schematic) = classic_schematic::from_classic_schematic_bounded(bytes, limits) {
        return Ok(schematic);
    }
    if world::McaFormat.detect_bounded(bytes, limits) {
        return world::McaFormat
            .read_bounded(bytes, limits)
            .map_err(|error| error.to_string());
    }
    if world::WorldZipFormat.detect_bounded(bytes, limits) {
        return world::WorldZipFormat
            .read_bounded(bytes, limits)
            .map_err(|error| error.to_string());
    }
    Err("未知或不受支持的投影格式".into())
}

/// Apply the source palette limit while allowing for the ordinary-air entry
/// added during `Region` construction.
fn validate_with_implicit_air(
    schematic: &UniversalSchematic,
    limits: &DecodeLimits,
) -> Result<(), String> {
    let mut internal_limits = limits.clone();
    internal_limits.max_palette_entries = limits.max_palette_entries.saturating_add(1);
    internal_limits
        .validate_schematic(schematic)
        .map_err(|error| error.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use quartz_nbt::{NbtCompound, NbtList, NbtTag};

    #[test]
    #[ignore = "Explicit code-only timing of compact decode for LP_MEMORY_INPUT"]
    fn inspect_compact_decode_timing() {
        let path = std::env::var_os("LP_MEMORY_INPUT").expect("Set LP_MEMORY_INPUT");
        let bytes = std::fs::read(path).unwrap();
        let mut samples = Vec::new();
        for iteration in 0..6 {
            let start = std::time::Instant::now();
            let source = decode_preview(&bytes, Some(64), None, false, &[], &|| Ok(())).unwrap().0;
            let seconds = start.elapsed().as_secs_f64();
            println!("compact_decode iteration={iteration} seconds={seconds:.6} blocks={} block_entities={}", source.block_count(), source.block_entity_count());
            if iteration != 0 {
                samples.push(seconds);
            }
        }
        samples.sort_by(f64::total_cmp);
        println!(
            "compact_decode median_seconds={:.6} min_seconds={:.6} max_seconds={:.6}",
            samples[2], samples[0], samples[4]
        );
    }

    fn fixture(name: &str) -> Vec<u8> {
        std::fs::read(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../Fixtures/Formats")
                .join(name),
        )
        .unwrap()
    }

    #[test]
    fn native_dispatch_enforces_aggregate_volume_at_the_boundary() {
        let snbt = normalize_structure_snbt(&fixture("Structure.snbt")).unwrap();
        let cases = [
            fixture("Classic.schematic"),
            fixture("Sponge.schem"),
            fixture("Bedrock.mcstructure"),
            fixture("Snapshot.nusn"),
            snbt,
        ];
        for bytes in cases {
            let schematic = read_bounded(&bytes, &preview_limits()).unwrap();
            let volume = std::iter::once(&schematic.default_region)
                .chain(schematic.other_regions.values())
                .map(|region| region.volume())
                .sum();
            let mut limits = DecodeLimits {
                max_volume: volume,
                ..preview_limits()
            };
            let bounded = read_bounded(&bytes, &limits).unwrap();
            assert_eq!(bounded.total_blocks(), schematic.total_blocks());
            limits.max_volume -= 1;
            assert!(read_bounded(&bytes, &limits).is_err());
        }
    }

    #[test]
    fn litematic_preflight_counts_negative_extents_and_all_region_budgets() {
        let mut regions = NbtCompound::new();
        for (index, x) in [-2, 2].into_iter().enumerate() {
            let mut size = NbtCompound::new();
            size.insert("x", x);
            size.insert("y", 2);
            size.insert("z", 2);
            let mut position = NbtCompound::new();
            for key in ["x", "y", "z"] {
                position.insert(key, 0);
            }
            let mut air = NbtCompound::new();
            air.insert("Name", "minecraft:air");
            let mut region = NbtCompound::new();
            region.insert("Size", size);
            region.insert("Position", position);
            region.insert(
                "BlockStatePalette",
                NbtList::from(vec![NbtTag::Compound(air)]),
            );
            region.insert("BlockStates", NbtTag::LongArray(vec![0]));
            let mut entity = NbtCompound::new();
            entity.insert("id", "minecraft:pig");
            entity.insert("Pos", NbtList::from(vec![NbtTag::Double(0.0); 3]));
            region.insert("Entities", NbtList::from(vec![NbtTag::Compound(entity)]));
            let mut chest = NbtCompound::new();
            chest.insert("id", "minecraft:chest");
            region.insert("TileEntities", NbtList::from(vec![NbtTag::Compound(chest)]));
            regions.insert(format!("region{index}"), region);
        }
        let mut root = NbtCompound::new();
        root.insert("Version", 6);
        root.insert("Metadata", NbtCompound::new());
        root.insert("Regions", regions);
        let mut bytes = Vec::new();
        quartz_nbt::io::write_nbt(
            &mut bytes,
            None,
            &root,
            quartz_nbt::io::Flavor::GzCompressed,
        )
        .unwrap();
        let limits = DecodeLimits {
            max_volume: 16,
            max_regions: 2,
            max_entities: 2,
            max_block_entities: 2,
            ..preview_limits()
        };
        let schematic = read_bounded(&bytes, &limits).unwrap();
        assert_eq!(schematic.total_volume(), 16);
        assert_eq!(
            schematic.default_region.entities.len()
                + schematic
                    .other_regions
                    .values()
                    .map(|region| region.entities.len())
                    .sum::<usize>(),
            2
        );
        assert_eq!(schematic.get_block_entities_as_list().len(), 2);
        for limits in [
            DecodeLimits {
                max_volume: 15,
                ..limits.clone()
            },
            DecodeLimits {
                max_regions: 1,
                ..limits.clone()
            },
            DecodeLimits {
                max_entities: 1,
                ..limits.clone()
            },
            DecodeLimits {
                max_block_entities: 1,
                ..limits.clone()
            },
        ] {
            assert!(read_bounded(&bytes, &limits).is_err());
        }
    }
}
