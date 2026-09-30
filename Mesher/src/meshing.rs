//! Application-owned compact chunk scheduling over Nucleation's public data model.
//!
//! Nucleation 0.10.14 block/entity and chunk scheduling; see NOTICE and
//! ThirdParty/Nucleation-LICENSE.txt.
use nucleation::meshing::{MeshConfig, MeshOutput, ResourcePackSource};
use nucleation::{BlockState, Entity, NbtValue, UniversalSchematic};
use schematic_mesher::{
    BlockPosition, BoundingBox, InputBlock, MesherConfig, ResourcePack, TextureAtlas,
};
use std::collections::{hash_map::Entry, HashMap};

mod atlas;
mod builder;

type ChunkCoord = (i32, i32, i32);
type IndexedBlock = (BlockPosition, u32);

pub(crate) struct CompactBlocks {
    chunks: Vec<(ChunkCoord, Vec<IndexedBlock>)>,
    palette: Vec<InputBlock>,
    block_count: i64,
    block_entity_count: i64,
}

pub(crate) struct PaletteEntry {
    index: Option<u32>,
    counted: bool,
}

pub(crate) struct CompactBlocksBuilder {
    chunks: HashMap<ChunkCoord, Vec<IndexedBlock>>,
    palette: Vec<InputBlock>,
    states: HashMap<BlockState, u32>,
    chunk_size: Option<i32>,
    block_count: i64,
    block_entity_count: i64,
    // Streaming regions arrive in file order. Temporary palette aliases carry
    // their logical source order without growing every stored block index.
    source_order: Option<Vec<(usize, u32)>>,
    source: usize,
}

impl CompactBlocksBuilder {
    pub(crate) fn new(chunk_size: Option<i32>) -> Result<Self, String> {
        if chunk_size.is_some_and(|size| size <= 0) {
            return Err("网格区块大小必须为正。".into());
        }
        Ok(Self {
            chunks: HashMap::new(),
            palette: Vec::new(),
            states: HashMap::new(),
            chunk_size,
            block_count: 0,
            block_entity_count: 0,
            source_order: None,
            source: 0,
        })
    }

    pub(crate) fn begin_source(&mut self) -> Result<(), String> {
        self.source = self.source.checked_add(1).ok_or("source order overflow")?;
        self.source_order.get_or_insert_with(Vec::new);
        Ok(())
    }

    fn source_index(&mut self, index: u32) -> Result<u32, String> {
        let Some(order) = &mut self.source_order else {
            return Ok(index);
        };
        let alias = u32::try_from(order.len())
            .map_err(|_| "投影文件的源方块状态过多。")?;
        order.try_reserve(1).map_err(|error| error.to_string())?;
        order.push((self.source, index));
        Ok(alias)
    }

    pub(crate) fn register_palette(
        &mut self,
        palette: &[BlockState],
    ) -> Result<Vec<PaletteEntry>, String> {
        let mut remap = Vec::new();
        remap
            .try_reserve_exact(palette.len())
            .map_err(|e| e.to_string())?;
        for state in palette {
            // Ordinary air is implicit; cave and void air count as blocks but do not render.
            let counted = state.name != "minecraft:air" || !state.properties.is_empty();
            let index = if is_air(&state.name) {
                None
            } else {
                let index = match self.states.entry(state.clone()) {
                    Entry::Occupied(entry) => *entry.get(),
                    Entry::Vacant(entry) => {
                        let index = u32::try_from(self.palette.len())
                            .map_err(|_| "投影文件的方块状态过多。")?;
                        self.palette.try_reserve(1).map_err(|e| e.to_string())?;
                        self.palette.push(block_state_to_input_block(entry.key()));
                        entry.insert(index);
                        index
                    }
                };
                Some(self.source_index(index)?)
            };
            remap.push(PaletteEntry { index, counted });
        }
        Ok(remap)
    }

    pub(crate) fn push_block(
        &mut self,
        position: BlockPosition,
        entry: &PaletteEntry,
    ) -> Result<(), String> {
        if entry.counted {
            self.block_count = self
                .block_count
                .checked_add(1)
                .ok_or("方块数量超出 i64 上限。")?;
        }
        if let Some(index) = entry.index {
            self.push_index(position, index)?;
        }
        Ok(())
    }

    pub(crate) fn push_entity(&mut self, entity: &Entity) -> Result<(), String> {
        let position = BlockPosition::new(
            entity.position.0.floor() as i32,
            entity.position.1.floor() as i32,
            entity.position.2.floor() as i32,
        );
        let index = u32::try_from(self.palette.len())
            .map_err(|_| "投影文件的方块状态过多。")?;
        self.palette.try_reserve(1).map_err(|e| e.to_string())?;
        self.palette.push(entity_to_input_block(entity));
        let index = self.source_index(index)?;
        self.push_index(position, index)
    }

    pub(crate) fn add_block_entities(&mut self, count: usize) -> Result<(), String> {
        self.block_entity_count = self
            .block_entity_count
            .checked_add(i64::try_from(count).map_err(|_| "方块实体数量超出 i64 上限。")?)
            .ok_or("方块实体数量超出 i64 上限。")?;
        Ok(())
    }

    fn push_index(&mut self, position: BlockPosition, index: u32) -> Result<(), String> {
        let coord = self
            .chunk_size
            .map_or((0, 0, 0), |size| chunk_coord(position, size));
        let blocks = self.chunks.entry(coord).or_default();
        blocks.try_reserve(1).map_err(|e| e.to_string())?;
        blocks.push((position, index));
        Ok(())
    }

    pub(crate) fn finish(self) -> CompactBlocks {
        let mut chunks: Vec<_> = self.chunks.into_iter().collect();
        chunks.sort_unstable_by_key(|(coord, _)| *coord);
        for (_, blocks) in &mut chunks {
            // Stable ties preserve additive entries; streamed aliases restore
            // source precedence before resolving to the global palette.
            if let Some(order) = &self.source_order {
                blocks.sort_by_key(|(pos, index)| (pos.y, pos.z, pos.x, order[*index as usize].0));
                for (_, index) in blocks {
                    *index = order[*index as usize].1;
                }
            } else {
                blocks.sort_by_key(|(pos, _)| (pos.y, pos.z, pos.x));
            }
        }
        CompactBlocks {
            chunks,
            palette: self.palette,
            block_count: self.block_count,
            block_entity_count: self.block_entity_count,
        }
    }
}

impl CompactBlocks {
    pub(crate) fn from_schematic(
        schematic: UniversalSchematic,
        chunk_size: Option<i32>,
        thread_count: Option<u8>,
        speed_first: bool,
        current: &impl Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        let mut builder = CompactBlocksBuilder::new(chunk_size)?;
        let UniversalSchematic {
            default_region,
            other_regions,
            ..
        } = schematic;
        let mut regions: Vec<_> = other_regions.into_iter().collect();
        regions.sort_unstable_by(|(left, _), (right, _)| left.cmp(right));
        for region in std::iter::once(default_region).chain(regions.into_iter().map(|(_, r)| r)) {
            current()?;
            builder.block_count = builder
                .block_count
                .checked_add(
                    i64::try_from(region.count_blocks()).map_err(|_| "方块数量超出 i64 上限。")?,
                )
                .ok_or("方块数量超出 i64 上限。")?;
            let remap = builder.register_palette(&region.get_palette())?;
            if let Some(count) = thread_count {
                let jobs = region
                    .blocks
                    .chunks(crate::parallel::BATCH_BLOCKS)
                    .enumerate()
                    .map(|(batch, blocks)| Ok((batch * crate::parallel::BATCH_BLOCKS, blocks)));
                crate::parallel::ordered(
                    jobs,
                    usize::from(count),
                    speed_first,
                    |(start, blocks), cancelled| {
                        let mut converted = Vec::new();
                        converted
                            .try_reserve_exact(blocks.len())
                            .map_err(|e| e.to_string())?;
                        for (offset, &state) in blocks.iter().enumerate() {
                            if offset % 1024 == 0 {
                                crate::parallel::check_cancelled(cancelled)?;
                            }
                            let entry = remap
                                .get(state)
                                .ok_or("投影文件包含无效的调色板索引。")?;
                            if let Some(state) = entry.index {
                                let (x, y, z) = region.index_to_coords(start + offset);
                                converted.push((BlockPosition::new(x, y, z), state));
                            }
                        }
                        Ok(converted)
                    },
                    |converted| {
                        for (position, state) in converted {
                            builder.push_index(position, state)?;
                        }
                        Ok(())
                    },
                    current,
                )?;
            } else {
                for (index, &state) in region.blocks.iter().enumerate() {
                    let entry = remap
                        .get(state)
                        .ok_or("投影文件包含无效的调色板索引。")?;
                    if let Some(state) = entry.index {
                        let (x, y, z) = region.index_to_coords(index);
                        builder.push_index(BlockPosition::new(x, y, z), state)?;
                    }
                }
            }
            for entity in &region.entities {
                builder.push_entity(entity)?;
            }
            builder.add_block_entities(region.block_entities.len())?;
            // Drop each dense region before loading the next to bound peak volume storage.
        }
        Ok(builder.finish())
    }

    pub(crate) fn block_count(&self) -> i64 {
        self.block_count
    }

    /// Distinct non-air block states with voxel counts, sorted by count
    /// descending. Counts scan the packed chunk index arrays directly.
    pub(crate) fn materials(&self) -> Vec<crate::MaterialEntry> {
        let mut counts = vec![0i64; self.palette.len()];
        for (_, blocks) in &self.chunks {
            for (_, index) in blocks {
                let count = &mut counts[*index as usize];
                *count = count.saturating_add(1);
            }
        }
        let mut materials: Vec<crate::MaterialEntry> = self
            .palette
            .iter()
            .zip(&counts)
            .filter(|(block, &count)| count > 0 && !block.is_air() && !block.name.starts_with("entity:"))
            .map(|(block, &count)| crate::MaterialEntry {
                name: block.name.clone(),
                properties: {
                    let mut properties: Vec<(String, String)> = block
                        .properties
                        .iter()
                        .map(|(key, value)| (key.clone(), value.clone()))
                        .collect();
                    properties.sort();
                    properties
                },
                count,
            })
            .collect();
        materials.sort_unstable_by(|left, right| {
            right
                .count
                .cmp(&left.count)
                .then_with(|| left.name.cmp(&right.name))
                .then_with(|| left.properties.cmp(&right.properties))
        });
        materials
    }

    pub(crate) fn block_entity_count(&self) -> i64 {
        self.block_entity_count
    }

    pub(crate) fn chunk_count(&self) -> usize {
        self.chunks.len()
    }

    fn atlas(
        &self,
        pack: &ResourcePackSource,
        config: &MesherConfig,
    ) -> Result<TextureAtlas, String> {
        let positioned: Vec<_> = self
            .palette
            .iter()
            .map(|block| block.name.starts_with("entity:") || atlas::position_dependent(block))
            .collect();
        let representatives = self
            .palette
            .iter()
            .zip(&positioned)
            .filter(|(_, positioned)| !**positioned)
            .map(|(block, _)| (BlockPosition::new(0, 0, 0), block));
        // Static palettes need no additional volume scan. Entities and generated
        // position-keyed textures must be discovered at every actual position.
        let chunks = if positioned.iter().any(|&value| value) {
            self.chunks.as_slice()
        } else {
            &[]
        };
        let actual_positions = chunks.iter().flat_map(|(_, blocks)| {
            let positioned = &positioned;
            blocks.iter().filter_map(move |&(pos, index)| {
                positioned[index as usize].then_some((pos, &self.palette[index as usize]))
            })
        });
        atlas::build(pack.pack(), config, representatives.chain(actual_positions))
            .map_err(|error| format!("无法准备投影纹理：{error}"))
    }

    fn context(
        &self,
        coord: ChunkCoord,
        chunk_size: Option<i32>,
    ) -> Vec<(BlockPosition, &InputBlock)> {
        let Some(chunk_size) = chunk_size else {
            // Unseparated means one real core and complete neighbor context,
            // not a hidden series of meshing windows merged after greedy meshing.
            return self
                .chunks
                .iter()
                .flat_map(|(_, blocks)| {
                    blocks
                        .iter()
                        .map(|&(pos, state)| (pos, &self.palette[state as usize]))
                })
                .collect();
        };
        let (min, max) = chunk_bounds(coord, chunk_size);
        let capacity = self
            .chunks
            .binary_search_by_key(&coord, |(coord, _)| *coord)
            .map_or(0, |index| self.chunks[index].1.len());
        let mut context = Vec::with_capacity(capacity);
        // Face culling, corner AO, and liquid heights (including one block above
        // diagonals) all need the complete one-block Chebyshev halo.
        for dx in -1i32..=1 {
            for dy in -1i32..=1 {
                for dz in -1i32..=1 {
                    let (Some(x), Some(y), Some(z)) = (
                        coord.0.checked_add(dx),
                        coord.1.checked_add(dy),
                        coord.2.checked_add(dz),
                    ) else {
                        continue;
                    };
                    let Ok(index) = self
                        .chunks
                        .binary_search_by_key(&(x, y, z), |(coord, _)| *coord)
                    else {
                        continue;
                    };
                    let blocks = &self.chunks[index].1;
                    let start = blocks.partition_point(|(pos, _)| i64::from(pos.y) < min[1] - 1);
                    let end = blocks.partition_point(|(pos, _)| i64::from(pos.y) < max[1] + 1);
                    for &(pos, state) in &blocks[start..end] {
                        if i64::from(pos.x) >= min[0] - 1
                            && i64::from(pos.x) < max[0] + 1
                            && i64::from(pos.z) >= min[2] - 1
                            && i64::from(pos.z) < max[2] + 1
                        {
                            context.push((pos, &self.palette[state as usize]));
                        }
                    }
                }
            }
        }
        context
    }
}

fn chunk_coord(pos: BlockPosition, size: i32) -> ChunkCoord {
    (
        pos.x.div_euclid(size),
        pos.y.div_euclid(size),
        pos.z.div_euclid(size),
    )
}

fn chunk_bounds(coord: ChunkCoord, size: i32) -> ([i64; 3], [i64; 3]) {
    let size = i64::from(size);
    let min = [coord.0, coord.1, coord.2].map(|value| i64::from(value) * size);
    (min, min.map(|value| value + size))
}

/// Retain compact source data through the final chunk for backward-looking halo
/// queries. Only the current output is owned by the caller; no mesh is retained.
pub(super) struct ChunkMeshes<'a> {
    source: CompactBlocks,
    index: usize,
    chunk_size: Option<i32>,
    pack: &'a ResourcePack,
    config: MesherConfig,
    atlas: TextureAtlas,
    atlas_only: Vec<bool>,
}

impl<'a> ChunkMeshes<'a> {
    #[cfg(test)]
    pub(super) fn new(
        schematic: UniversalSchematic,
        pack: &'a ResourcePackSource,
        config: &MeshConfig,
        chunk_size: Option<i32>,
        current: impl Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        let source = CompactBlocks::from_schematic(schematic, chunk_size, None, false, &current)?;
        Self::from_source(source, pack, config, chunk_size, current)
    }

    pub(super) fn from_source(
        source: CompactBlocks,
        pack: &'a ResourcePackSource,
        config: &MeshConfig,
        chunk_size: Option<i32>,
        current: impl Fn() -> Result<(), String>,
    ) -> Result<Self, String> {
        current()?;
        let config = mesher_config(config);
        let atlas = source.atlas(pack, &config)?;
        let atlas_only = source
            .palette
            .iter()
            .map(|block| config.greedy_meshing && builder::atlas_only(pack.pack(), block))
            .collect();
        current()?;
        Ok(Self {
            source,
            index: 0,
            chunk_size,
            pack: pack.pack(),
            config,
            atlas,
            atlas_only,
        })
    }

    /// Memory-first windows share a 128K-entry context-allocation budget; speed
    /// mode uses the requested workers and refills consumed slots immediately.
    /// Both modes retain at most N outputs, including the one being consumed.
    /// Models have no fixed output-byte bound. Source, atlas and pack are
    /// borrowed, never copied.
    pub(super) fn consume(
        &mut self,
        thread_count: Option<u8>,
        speed_first: bool,
        mut consume: impl FnMut(MeshOutput) -> Result<(), String>,
        current: &impl Fn() -> Result<(), String>,
    ) -> Result<(), String> {
        let Some(count) = thread_count else {
            loop {
                current()?;
                let Some(mesh) = self.next() else {
                    return Ok(());
                };
                let mesh = mesh.map_err(|error| {
                    format!("该投影文件过于复杂，无法预览：{error}")
                })?;
                consume(mesh)?;
            }
        };
        let admission = self.worker_admission(count, speed_first);
        crate::parallel::ordered(
            (self.index..self.source.chunks.len()).map(Ok),
            admission,
            speed_first,
            |index, cancelled| {
                crate::parallel::check_cancelled(cancelled)?;
                let mesh = self.mesh_at(index).map_err(|error| {
                    format!("该投影文件过于复杂，无法预览：{error}")
                })?;
                crate::parallel::check_cancelled(cancelled)?;
                Ok(mesh)
            },
            consume,
            current,
        )?;
        self.index = self.source.chunks.len();
        Ok(())
    }

    fn worker_admission(&self, count: u8, speed_first: bool) -> usize {
        if speed_first {
            return usize::from(count);
        }
        let max_chunk = self
            .source
            .chunks
            .iter()
            .map(|(_, blocks)| blocks.len())
            .max()
            .unwrap_or(0);
        // Budget the largest core, its 27 neighbors, and Vec growth; an oversize
        // chunk runs alone in memory-first mode.
        let context_bound = max_chunk.saturating_mul(54).max(1);
        usize::from(count).min((128 * 1024 / context_bound).max(1))
    }

    fn mesh_at(&self, index: usize) -> Result<MeshOutput, String> {
        let (coord, blocks) = &self.source.chunks[index];
        let bounds = if let Some(size) = self.chunk_size {
            let (min, max) = chunk_bounds(*coord, size);
            BoundingBox::new(min.map(|value| value as f32), max.map(|value| value as f32))
        } else {
            BoundingBox::from_points(
                blocks
                    .iter()
                    .map(|(pos, _)| [pos.x as f32, pos.y as f32, pos.z as f32]),
            )
            .expect("Only occupied groups are retained")
        };
        let context = self.source.context(*coord, self.chunk_size);
        let mut mesh = builder::mesh(
            self.pack,
            &self.config,
            &self.atlas,
            &self.source.palette,
            &self.atlas_only,
            blocks,
            &context,
            bounds,
        )?;
        mesh.chunk_coord = self.chunk_size.map(|_| *coord);
        Ok(mesh)
    }
}

impl Iterator for ChunkMeshes<'_> {
    type Item = Result<MeshOutput, String>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.index == self.source.chunks.len() {
            return None;
        }
        let result = self.mesh_at(self.index);
        self.index += 1;
        Some(result)
    }

    fn size_hint(&self) -> (usize, Option<usize>) {
        let remaining = self.source.chunks.len() - self.index;
        (remaining, Some(remaining))
    }
}

fn mesher_config(config: &MeshConfig) -> MesherConfig {
    // Keep unspecified mesher settings at library defaults, then apply the
    // optional biome after mapping the public configuration.
    let mut result = MesherConfig {
        cull_hidden_faces: config.cull_hidden_faces,
        ambient_occlusion: config.ambient_occlusion,
        ao_intensity: config.ao_intensity,
        atlas_max_size: config.atlas_max_size,
        cull_occluded_blocks: config.cull_occluded_blocks,
        greedy_meshing: config.greedy_meshing,
        ..MesherConfig::default()
    };
    if let Some(biome) = &config.biome {
        result = result.with_biome(biome);
    }
    result
}

fn is_air(name: &str) -> bool {
    matches!(
        name,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}

fn block_state_to_input_block(state: &BlockState) -> InputBlock {
    let mut input = InputBlock::new(state.name.to_string());
    // A bare id denotes its default state. Explicit schematic properties win.
    if let Some(facts) = nucleation::blockpedia::get_block(&state.name) {
        for (key, value) in facts.default_state {
            input.properties.insert(key.to_string(), value.to_string());
        }
    }
    for (key, value) in &state.properties {
        input.properties.insert(key.to_string(), value.to_string());
    }
    input
}

fn entity_to_input_block(entity: &Entity) -> InputBlock {
    let entity_id = entity.id.strip_prefix("minecraft:").unwrap_or(&entity.id);
    let mesher_id = match entity_id {
        "furnace_minecart"
        | "chest_minecart"
        | "tnt_minecart"
        | "hopper_minecart"
        | "spawner_minecart"
        | "command_block_minecart" => "minecart",
        id if id.ends_with("_chest_boat") || id.ends_with("_chest_raft") => "chest_boat",
        id if id.ends_with("_boat") || id.ends_with("_raft") => "boat",
        id => id,
    };
    let mut input = InputBlock::new(format!("entity:{mesher_id}"));
    if let Some(NbtValue::List(rotation)) = entity.nbt.get("Rotation") {
        if let Some(NbtValue::Float(yaw)) = rotation.first() {
            input
                .properties
                .insert("facing".into(), yaw_to_facing(*yaw).into());
        }
    }
    if let Some(NbtValue::Compound(item)) = entity.nbt.get("Item") {
        if let Some(NbtValue::String(id)) = item.get("id") {
            input.properties.insert("item".into(), id.clone());
        }
    }
    if matches!(entity.nbt.get("IsBaby"), Some(NbtValue::Byte(1)))
        || matches!(entity.nbt.get("Age"), Some(NbtValue::Int(age)) if *age < 0)
    {
        input.properties.insert("is_baby".into(), "true".into());
    }
    if let Some(NbtValue::Byte(color)) = entity.nbt.get("Color") {
        input
            .properties
            .insert("color".into(), dye_color_name(*color as u8).into());
    }
    for pose_key in [
        "RightArmPose",
        "LeftArmPose",
        "RightLegPose",
        "LeftLegPose",
        "HeadPose",
        "BodyPose",
    ] {
        if let Some(NbtValue::List(angles)) = entity.nbt.get(pose_key) {
            use std::fmt::Write;
            let mut pose = String::new();
            for angle in angles {
                if let NbtValue::Float(angle) = angle {
                    if !pose.is_empty() {
                        pose.push(',');
                    }
                    write!(&mut pose, "{angle}").expect("writing to a String cannot fail");
                }
            }
            if !pose.is_empty() {
                input.properties.insert(pose_key.into(), pose);
            }
        }
    }
    if let Some(NbtValue::List(items)) = entity.nbt.get("ArmorItems") {
        for (index, property) in ["boots", "leggings", "chestplate", "helmet"]
            .iter()
            .enumerate()
        {
            if let Some(NbtValue::Compound(item)) = items.get(index) {
                if let Some(NbtValue::String(id)) = item.get("id") {
                    input.properties.insert((*property).into(), id.clone());
                }
            }
        }
    }
    input
}

fn yaw_to_facing(yaw: f32) -> &'static str {
    let normalized = ((yaw % 360.0) + 360.0) % 360.0;
    if !(45.0..315.0).contains(&normalized) {
        "south"
    } else if (45.0..135.0).contains(&normalized) {
        "west"
    } else if (135.0..225.0).contains(&normalized) {
        "north"
    } else {
        "east"
    }
}

fn dye_color_name(color: u8) -> &'static str {
    match color {
        0 => "white",
        1 => "orange",
        2 => "magenta",
        3 => "light_blue",
        4 => "yellow",
        5 => "lime",
        6 => "pink",
        7 => "gray",
        8 => "light_gray",
        9 => "cyan",
        10 => "purple",
        11 => "blue",
        12 => "brown",
        13 => "green",
        14 => "red",
        15 => "black",
        _ => "white",
    }
}

#[cfg(test)]
mod tests;
