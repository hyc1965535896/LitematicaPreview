//! Owned schematic mesh buffers shared directly with the Tauri Rust host.

use nucleation::meshing::{MeshConfig, MeshLayer, MeshOutput, ResourcePackSource};
use schematic_mesher::BoundingBox;

mod block_names_zh;
mod decode;
mod dv_map;
mod export;
mod icons;
mod materials_xlsx;
mod meshing;
mod parallel;
mod replace;
pub use decode::{decode, DecodeFailure};
pub use export::{export_schematic, ExportFormat};
pub use icons::{block_icons, BlockIcon};
pub use materials_xlsx::{export_materials_xlsx, MaterialsExport};
pub use replace::{apply_replacements, validate_replacements, BlockReplacement, MAX_REPLACEMENTS};

/// One distinct block state in the loaded schematic with its voxel count.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct MaterialEntry {
    pub name: String,
    pub properties: Vec<(String, String)>,
    pub count: i64,
}

/// Result of a complete preview load: mesh summary plus the material list
/// computed from the same decoded (and replacement-applied) model.
pub struct LoadedPreview {
    pub info: PreviewInfo,
    pub materials: Vec<MaterialEntry>,
    pub replaced: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PreviewOptions {
    pub memory_limit_mb: Option<u16>,
    pub chunk_size: Option<u16>,
    pub thread_count: Option<u8>,
    pub speed_first: bool,
}

impl Default for PreviewOptions {
    fn default() -> Self {
        Self {
            memory_limit_mb: None,
            chunk_size: Some(64),
            thread_count: None,
            speed_first: false,
        }
    }
}

impl PreviewOptions {
    pub fn validate(&self) -> Result<(), String> {
        if self
            .memory_limit_mb
            .is_some_and(|limit| !(2048..=8192).contains(&limit))
        {
            return Err("内存上限必须是 2048 到 8192 MB 之间的整数。".into());
        }
        if self
            .chunk_size
            .is_some_and(|size| !matches!(size, 16 | 32 | 64 | 128 | 256))
        {
            return Err("区块大小必须是 16、32、64、128 或 256 个方块。".into());
        }
        let max = max_worker_threads();
        if let Some(count) = self.thread_count {
            if self.chunk_size.is_none() {
                return Err("并行预览需要启用分块。".into());
            }
            if !(2..=max).contains(&count) {
                return Err(format!("工作线程数必须在 2 到 {max} 之间。"));
            }
        }
        if self.speed_first {
            if self.thread_count.is_none() {
                return Err("速度优先预览需要启用多线程。".into());
            }
        }
        Ok(())
    }
}

/// Maximum supported worker count for opt-in parallel preparation and meshing.
pub fn max_worker_threads() -> u8 {
    std::thread::available_parallelism().map_or(1, |count| count.get().min(8)) as u8
}

pub struct Preview {
    pub mesh: MeshOutput,
    pub textures: Vec<Texture>,
    pub info: PreviewInfo,
}

pub struct Texture {
    pub pixels: Vec<u8>,
    pub width: u32,
    pub height: u32,
}

#[derive(Clone, Copy, Default)]
pub struct PreviewInfo {
    pub block_count: i64,
    pub block_entity_count: i64,
    pub triangle_count: u64,
    pub part_count: u32,
    pub texture_count: u32,
    pub min: [f32; 3],
    pub max: [f32; 3],
}

pub fn mesh_config() -> MeshConfig {
    MeshConfig::new()
        .with_greedy_meshing(true)
        .with_ambient_occlusion(true)
        .with_atlas_max_size(2_048)
}

/// Enumerate non-empty geometry layers with their material and alpha-class indices.
/// Greedy materials use independent repeating textures rather than atlas UVs.
pub fn parts(mesh: &MeshOutput) -> impl Iterator<Item = (&MeshLayer, u32, u32)> {
    [
        (&mesh.opaque, 0, 0),
        (&mesh.cutout, 0, 1),
        (&mesh.transparent, 0, 2),
    ]
    .into_iter()
    .chain(mesh.greedy_materials.iter().enumerate().flat_map(|(i, m)| {
        [
            (&m.opaque, i as u32 + 1, 0),
            (&m.transparent, i as u32 + 1, 2),
        ]
    }))
    .filter(|(layer, _, _)| !layer.indices.is_empty())
}

/// Generate and synchronously hand off each configured spatial chunk, or one
/// complete geometry group when chunk separation is disabled. The caller owns
/// each result and should release it before accepting the next one.
/// Memory enforcement belongs to the isolated host process. Some formats still
/// decode densely, and an unseparated mesh can exhaust memory without that cap.
pub fn load_chunks(
    data: &[u8],
    pack: &ResourcePackSource,
    options: PreviewOptions,
    replacements: &[BlockReplacement],
    mut consume: impl FnMut(Preview) -> Result<(), String>,
    mut on_progress: impl FnMut(usize, usize) -> Result<(), String>,
    current: impl Fn() -> Result<(), String>,
) -> Result<LoadedPreview, String> {
    options.validate()?;
    validate_replacements(replacements)?;
    current()?;
    if data.is_empty() {
        return Err("请选择非空的投影文件。".into());
    }
    let chunk_size = options.chunk_size.map(i32::from);
    let (source, replaced) = decode::decode_preview(
        data,
        chunk_size,
        options.thread_count,
        options.speed_first,
        replacements,
        &current,
    )?;
    current()?;
    let materials = source.materials();
    let block_count = source.block_count();
    let block_entity_count = source.block_entity_count();
    if block_count == 0 {
        return Err("投影文件必须至少包含一个方块。".into());
    }
    let total = source.chunk_count();
    if total == 0 {
        return Err("投影文件不包含可见的几何体。".into());
    }
    on_progress(0, total)?;
    let mut chunks =
        meshing::ChunkMeshes::from_source(source, pack, &mesh_config(), chunk_size, &current)?;
    current()?;
    let mut info = PreviewInfo {
        block_count,
        block_entity_count,
        texture_count: 1,
        min: [f32::INFINITY; 3],
        max: [f32::NEG_INFINITY; 3],
        ..PreviewInfo::default()
    };
    let mut completed = 0;
    chunks.consume(
        options.thread_count,
        options.speed_first,
        |mesh| {
            completed += 1;
            if parts(&mesh).next().is_none() {
                if completed < total {
                    on_progress(completed, total)?;
                }
                return Ok(());
            }
            let preview = prepare(mesh, block_count, block_entity_count)?;
            info.triangle_count = info
                .triangle_count
                .checked_add(preview.info.triangle_count)
                .ok_or("投影文件的三角面过多。")?;
            info.part_count = info
                .part_count
                .checked_add(preview.info.part_count)
                .ok_or("投影文件的网格部件过多。")?;
            info.texture_count = info
                .texture_count
                .checked_add(preview.info.texture_count - 1)
                .ok_or("投影文件的纹理过多。")?;
            for axis in 0..3 {
                info.min[axis] = info.min[axis].min(preview.info.min[axis]);
                info.max[axis] = info.max[axis].max(preview.info.max[axis]);
            }
            consume(preview)?;
            if completed < total {
                on_progress(completed, total)?;
            }
            current()
        },
        &current,
    )?;
    if info.part_count == 0 {
        return Err("投影文件不包含可见的几何体。".into());
    }
    on_progress(completed, total)?;
    Ok(LoadedPreview {
        info,
        materials,
        replaced,
    })
}

pub fn prepare(
    mesh: MeshOutput,
    block_count: i64,
    block_entity_count: i64,
) -> Result<Preview, String> {
    let mut triangle_count = 0u64;
    let mut part_count = 0u32;
    for (layer, _, _) in parts(&mesh) {
        let n = layer.positions.len();
        if n > i32::MAX as usize
            || layer.indices.len() > i32::MAX as usize
            || layer.normals.len() != n
            || layer.uvs.len() != n
            || layer.colors.len() != n
            || layer.indices.len() % 3 != 0
            || layer.indices.iter().any(|&i| i as usize >= n)
        {
            return Err("网格生成器产生了无效或过大的几何体。".into());
        }
        triangle_count += (layer.indices.len() / 3) as u64;
        part_count += 1;
    }
    let bounds =
        BoundingBox::from_points(parts(&mesh).flat_map(|(p, _, _)| p.positions.iter().copied()))
            .ok_or("投影文件不包含可见的几何体。")?;
    if bounds.min.iter().chain(&bounds.max).any(|v| !v.is_finite()) {
        return Err("网格生成器产生了无效的边界。".into());
    }
    let mut textures = Vec::with_capacity(mesh.greedy_materials.len());
    for material in &mesh.greedy_materials {
        let image =
            image::load_from_memory_with_format(&material.texture_png, image::ImageFormat::Png)
                .map_err(|e| format!("无法读取方块纹理：{e}"))?
                .into_rgba8();
        let (width, height) = image.dimensions();
        textures.push(Texture {
            pixels: image.into_raw(),
            width,
            height,
        });
    }
    let info = PreviewInfo {
        block_count,
        block_entity_count,
        triangle_count,
        part_count,
        texture_count: textures.len() as u32 + 1,
        min: bounds.min,
        max: bounds.max,
    };
    Ok(Preview {
        mesh,
        textures,
        info,
    })
}
