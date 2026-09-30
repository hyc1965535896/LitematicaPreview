use std::collections::hash_map::DefaultHasher;
use std::hash::{Hash, Hasher};
use std::io::{self, Read, Write};

use litematica_preview_native::{parts, LoadedPreview, Preview};
use serde::{Deserialize, Serialize};

pub const FRAME_BYTES: usize = 1024 * 1024;
pub const MAX_READ_RANGES: usize = 256;
const END: u8 = 0;
const ERROR: u8 = 1;
const TEXTURE: u8 = 2;
const PART: u8 = 3;
const DATA: u8 = 4;
const CHECKPOINT: u8 = 5;
const PROGRESS: u8 = 6;
const CHUNK: u8 = 7;
const CONTINUE: u8 = 1;
const CANCEL: u8 = 0;
const TOO_LARGE: &str = "预览过大，无法传输到图形设备。";

#[cfg(not(target_endian = "little"))]
compile_error!("预览缓冲区需要小端序目标平台。");

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Metadata {
    pub block_count: i64,
    pub block_entity_count: i64,
    pub triangle_count: u64,
    pub min: [f32; 3],
    pub max: [f32; 3],
    pub byte_length: usize,
    pub textures: Vec<TextureMetadata>,
    pub parts: Vec<PartMetadata>,
    #[serde(default)]
    pub materials: Vec<MaterialRecord>,
    #[serde(default)]
    pub replaced: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MaterialRecord {
    pub name: String,
    pub properties: Vec<(String, String)>,
    pub count: i64,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Batch {
    pub batch_id: u64,
    pub texture_offset: usize,
    pub metadata: Metadata,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum StreamEvent {
    Batch { batch: Batch },
    Complete { metadata: Metadata },
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TextureMetadata {
    pub width: u32,
    pub height: u32,
    pub byte_length: usize,
    pub buffer_id: usize,
    pub repeat: bool,
}

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PartMetadata {
    pub vertex_count: u32,
    pub index_count: u32,
    pub texture_index: u32,
    pub alpha_mode: u32,
    pub buffers: [usize; 5],
}

#[derive(Serialize, Deserialize)]
struct TextureRecord {
    width: u32,
    height: u32,
    byte_length: usize,
    repeat: bool,
}

#[derive(Serialize, Deserialize)]
struct PartRecord {
    vertex_count: u32,
    index_count: u32,
    texture_index: u32,
    alpha_mode: u32,
}

#[derive(Serialize, Deserialize)]
pub struct Summary {
    pub block_count: i64,
    pub block_entity_count: i64,
    pub triangle_count: u64,
    pub min: [f32; 3],
    pub max: [f32; 3],
    #[serde(default)]
    pub materials: Vec<MaterialRecord>,
    #[serde(default)]
    pub replaced: i64,
    /// Distinct items inside containers; only material-list exports fill it.
    #[serde(default)]
    pub container_item_count: i64,
}

/// Stores one logical renderer payload as allocation-sized segments without
/// flattening it into a contiguous model. Packed IPC reads copy at most 1 MiB per page.
pub struct Payload {
    pub metadata: Metadata,
    buffers: Vec<Buffer>,
}

#[derive(Default)]
struct Buffer {
    segments: Vec<Vec<u8>>,
    ends: Vec<usize>,
    length: usize,
}

#[derive(Clone, Copy, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ReadRange {
    pub buffer_id: usize,
    pub offset: usize,
    pub length: usize,
}

fn align4(value: usize) -> Option<usize> {
    value.checked_add(3).map(|value| value & !3)
}

impl Payload {
    pub fn read_ranges(&self, ranges: &[ReadRange]) -> Result<Vec<u8>, String> {
        if ranges.is_empty() || ranges.len() > MAX_READ_RANGES {
            return Err("预览范围无效。".into());
        }
        let mut total = 0usize;
        for range in ranges {
            let buffer = self
                .buffers
                .get(range.buffer_id)
                .ok_or("预览范围无效。")?;
            let end = range
                .offset
                .checked_add(range.length)
                .ok_or("预览范围无效。")?;
            if range.length == 0 || end > buffer.length {
                return Err("预览范围无效。".into());
            }
            total = align4(total)
                .and_then(|total| total.checked_add(range.length))
                .filter(|total| *total <= FRAME_BYTES)
                .ok_or("预览范围无效。")?;
        }

        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(total)
            .map_err(|_| "内存不足，无法上传此预览。")?;
        bytes.resize(total, 0);
        let mut destination = 0usize;
        for range in ranges {
            destination = align4(destination).expect("Validated page offset");
            let buffer = &self.buffers[range.buffer_id];
            let end = range.offset + range.length;
            let mut cursor = range.offset;
            let mut index = buffer
                .ends
                .partition_point(|segment_end| *segment_end <= cursor);
            while cursor < end {
                let base = if index == 0 {
                    0
                } else {
                    buffer.ends[index - 1]
                };
                let segment = &buffer.segments[index];
                let start = cursor - base;
                let count = (segment.len() - start).min(end - cursor);
                bytes[destination..destination + count]
                    .copy_from_slice(&segment[start..start + count]);
                cursor += count;
                destination += count;
                index += 1;
            }
        }
        Ok(bytes)
    }

    fn assemble(
        mut metadata: Metadata,
        mut source: Vec<Buffer>,
        current: impl Fn() -> bool,
    ) -> io::Result<Self> {
        let mut buffers = Vec::new();
        for texture in &mut metadata.textures {
            let buffer = std::mem::take(&mut source[texture.buffer_id]);
            texture.buffer_id = buffers.len();
            buffers.push(buffer);
        }
        let mut merged: Vec<PartMetadata> = Vec::new();
        let mut groups = std::collections::HashMap::new();
        for part in metadata.parts {
            if !current() {
                return Err(io::Error::new(io::ErrorKind::Interrupted, "Cancelled"));
            }
            let key = (part.texture_index, part.alpha_mode);
            let existing = groups.get(&key).copied().filter(|&index| {
                let target: &PartMetadata = &merged[index];
                target
                    .vertex_count
                    .checked_add(part.vertex_count)
                    .is_some_and(|count| count <= i32::MAX as u32)
                    && target
                        .index_count
                        .checked_add(part.index_count)
                        .is_some_and(|count| count <= i32::MAX as u32)
            });
            let group = if let Some(index) = existing {
                index
            } else {
                // Keep each merged draw batch within the renderer's i32::MAX counts.
                let index = merged.len();
                let base = buffers.len();
                buffers.extend((0..5).map(|_| Buffer::default()));
                merged.push(PartMetadata {
                    vertex_count: 0,
                    index_count: 0,
                    texture_index: part.texture_index,
                    alpha_mode: part.alpha_mode,
                    buffers: [base, base + 1, base + 2, base + 3, base + 4],
                });
                groups.insert(key, index);
                index
            };
            let target = &mut merged[group];
            let vertex_offset = target.vertex_count;
            target.vertex_count = target
                .vertex_count
                .checked_add(part.vertex_count)
                .filter(|n| *n <= i32::MAX as u32)
                .ok_or_else(|| invalid(TOO_LARGE))?;
            target.index_count = target
                .index_count
                .checked_add(part.index_count)
                .filter(|n| *n <= i32::MAX as u32)
                .ok_or_else(|| invalid(TOO_LARGE))?;
            for attribute in 0..5 {
                let mut buffer = std::mem::take(&mut source[part.buffers[attribute]]);
                if attribute == 4 && vertex_offset != 0 {
                    for segment in &mut buffer.segments {
                        for bytes in segment.chunks_exact_mut(4) {
                            let index =
                                u32::from_le_bytes(bytes.try_into().unwrap()) + vertex_offset;
                            bytes.copy_from_slice(&index.to_le_bytes());
                        }
                    }
                }
                let destination = &mut buffers[target.buffers[attribute]];
                let base = destination.length;
                destination.length = base
                    .checked_add(buffer.length)
                    .ok_or_else(|| invalid(TOO_LARGE))?;
                destination
                    .ends
                    .extend(buffer.ends.into_iter().map(|end| base + end));
                destination.segments.extend(buffer.segments);
            }
        }
        metadata.parts = merged;
        Ok(Self { metadata, buffers })
    }
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

fn write_packet(stream: &mut impl Write, kind: u8, bytes: &[u8]) -> io::Result<()> {
    if bytes.len() > FRAME_BYTES {
        return Err(invalid("解码器数据包过大。"));
    }
    stream.write_all(&[kind])?;
    stream.write_all(&(bytes.len() as u32).to_le_bytes())?;
    stream.write_all(bytes)
}

fn read_packet(stream: &mut impl Read) -> io::Result<(u8, Vec<u8>)> {
    let mut header = [0; 5];
    stream.read_exact(&mut header)?;
    let length = u32::from_le_bytes(header[1..].try_into().unwrap()) as usize;
    if length > FRAME_BYTES {
        return Err(invalid("解码器数据包过大。"));
    }
    let mut bytes = Vec::new();
    bytes
        .try_reserve_exact(length)
        .map_err(|_| invalid("内存不足，无法接收此预览。"))?;
    bytes.resize(length, 0);
    stream.read_exact(&mut bytes)?;
    Ok((header[0], bytes))
}

pub struct Encoder<'a, S> {
    stream: &'a mut S,
    textures: Vec<(u64, TextureRecord, Vec<u8>)>,
}

impl<'a, S: Read + Write> Encoder<'a, S> {
    pub fn new(stream: &'a mut S) -> Self {
        Self {
            stream,
            textures: Vec::new(),
        }
    }

    pub fn checkpoint(&mut self) -> Result<(), String> {
        self.send(CHECKPOINT, &[])
    }

    pub fn progress(&mut self, completed: u64, total: u64) -> Result<(), String> {
        let mut bytes = [0; 16];
        bytes[..8].copy_from_slice(&completed.to_le_bytes());
        bytes[8..].copy_from_slice(&total.to_le_bytes());
        self.send(PROGRESS, &bytes)
    }

    fn send(&mut self, kind: u8, bytes: &[u8]) -> Result<(), String> {
        write_packet(self.stream, kind, bytes).map_err(|e| e.to_string())?;
        let mut ack = [0];
        self.stream
            .read_exact(&mut ack)
            .map_err(|e| e.to_string())?;
        match ack[0] {
            CONTINUE => Ok(()),
            CANCEL => Err("Cancelled".into()),
            _ => Err("解码器确认无效。".into()),
        }
    }

    fn record(&mut self, kind: u8, value: &impl Serialize) -> Result<(), String> {
        let bytes = serde_json::to_vec(value).map_err(|e| e.to_string())?;
        self.send(kind, &bytes)
    }

    fn bytes(&mut self, bytes: &[u8]) -> Result<(), String> {
        for segment in bytes.chunks(FRAME_BYTES) {
            self.send(DATA, segment)?;
        }
        Ok(())
    }

    fn texture(
        &mut self,
        width: u32,
        height: u32,
        pixels: &[u8],
        repeat: bool,
    ) -> Result<u32, String> {
        let mut hash = DefaultHasher::new();
        (width, height, repeat, pixels).hash(&mut hash);
        let key = hash.finish();
        if let Some(index) = self.textures.iter().position(|(h, record, prior)| {
            *h == key
                && record.width == width
                && record.height == height
                && record.repeat == repeat
                && prior == pixels
        }) {
            return u32::try_from(index).map_err(|_| TOO_LARGE.into());
        }
        let index = u32::try_from(self.textures.len()).map_err(|_| TOO_LARGE)?;
        let record = TextureRecord {
            width,
            height,
            byte_length: pixels.len(),
            repeat,
        };
        self.record(TEXTURE, &record)?;
        self.bytes(pixels)?;
        self.textures.push((key, record, pixels.to_vec()));
        Ok(index)
    }

    pub fn chunk(&mut self, preview: Preview) -> Result<(), String> {
        let atlas = &preview.mesh.atlas;
        let mut textures = Vec::with_capacity(preview.textures.len() + 1);
        textures.push(self.texture(atlas.width, atlas.height, &atlas.pixels, false)?);
        for texture in &preview.textures {
            textures.push(self.texture(texture.width, texture.height, &texture.pixels, true)?);
        }
        for (part, texture, alpha_mode) in parts(&preview.mesh) {
            let record = PartRecord {
                vertex_count: u32::try_from(part.positions.len()).map_err(|_| TOO_LARGE)?,
                index_count: u32::try_from(part.indices.len()).map_err(|_| TOO_LARGE)?,
                texture_index: textures[texture as usize],
                alpha_mode,
            };
            self.record(PART, &record)?;
            self.bytes(bytemuck::cast_slice(&part.positions))?;
            self.quantized(part.normals.iter().flatten().copied(), true)?;
            self.bytes(bytemuck::cast_slice(&part.uvs))?;
            self.quantized(part.colors.iter().flatten().copied(), false)?;
            self.bytes(bytemuck::cast_slice(&part.indices))?;
        }
        self.record(
            CHUNK,
            &Summary {
                block_count: preview.info.block_count,
                block_entity_count: preview.info.block_entity_count,
                triangle_count: preview.info.triangle_count,
                min: preview.info.min,
                max: preview.info.max,
                materials: Vec::new(),
                replaced: 0,
                container_item_count: 0,
            },
        )
    }

    fn quantized(&mut self, values: impl Iterator<Item = f32>, signed: bool) -> Result<(), String> {
        let mut bytes = Vec::with_capacity(FRAME_BYTES);
        for value in values {
            if !value.is_finite() {
                return Err("网格生成器产生了非有限的数值属性。".into());
            }
            bytes.push(if signed {
                (value.clamp(-1.0, 1.0) * 127.0).round() as i8 as u8
            } else {
                (value.clamp(0.0, 1.0) * 255.0).round() as u8
            });
            if bytes.len() == FRAME_BYTES {
                self.send(DATA, &bytes)?;
                bytes.clear();
            }
        }
        if !bytes.is_empty() {
            self.send(DATA, &bytes)?;
        }
        Ok(())
    }
}

pub fn finish(stream: &mut impl Write, result: Result<LoadedPreview, String>) -> io::Result<()> {
    match result {
        Ok(loaded) => {
            let summary = Summary {
                block_count: loaded.info.block_count,
                block_entity_count: loaded.info.block_entity_count,
                triangle_count: loaded.info.triangle_count,
                min: loaded.info.min,
                max: loaded.info.max,
                materials: loaded
                    .materials
                    .into_iter()
                    .map(|entry| MaterialRecord {
                        name: entry.name,
                        properties: entry.properties,
                        count: entry.count,
                    })
                    .collect(),
                replaced: loaded.replaced,
                container_item_count: loaded.container_item_count,
            };
            write_packet(
                stream,
                END,
                &serde_json::to_vec(&summary).map_err(|e| invalid(e.to_string()))?,
            )
        }
        Err(error) => {
            const OVERSIZED_ERROR_FALLBACK: &str = "解码器返回了过大的错误信息。";
            let message = if error.len() <= FRAME_BYTES {
                error.as_bytes()
            } else {
                OVERSIZED_ERROR_FALLBACK.as_bytes()
            };
            write_packet(stream, ERROR, message)
        }
    }
}

/// Receives and assembles one logical renderer payload.
///
/// Every non-terminal packet requires an acknowledgement. Cancellation is observed
/// before the next packet, after which the decoder emits an `ERROR` terminator before
/// accepting another request. A malformed stream invalidates the connection.
pub fn receive(
    stream: &mut (impl Read + Write),
    current: impl Fn() -> bool,
    on_progress: impl FnMut(u64, u64),
) -> io::Result<Result<Payload, String>> {
    let mut buffers = Vec::new();
    let result = receive_stream(stream, &current, on_progress, |_, payload| {
        buffers.extend(payload.buffers);
        Ok(())
    })?;
    match result {
        Ok(metadata) => Payload::assemble(metadata, buffers, current).map(Ok),
        Err(error) => Ok(Err(error)),
    }
}

/// Reads a summary-only terminator exchange used by export requests. Any
/// data packet invalidates the connection; errors preserve the usual text.
pub fn receive_summary(
    stream: &mut (impl Read + Write),
    current: impl Fn() -> bool,
) -> io::Result<Result<Summary, String>> {
    loop {
        let (kind, bytes) = read_packet(stream)?;
        if !current() {
            let _ = stream.write_all(&[CANCEL]);
            return Ok(Err("Cancelled".into()));
        }
        match kind {
            ERROR => {
                let error =
                    String::from_utf8(bytes).map_err(|_| invalid("解码器错误文本无效。"))?;
                return Ok(Err(error));
            }
            END => {
                let summary: Summary = serde_json::from_slice(&bytes)
                    .map_err(|e| invalid(format!("解码器请求无效：{e}")))?;
                return Ok(Ok(summary));
            }
            _ => return Err(invalid("意外的解码器记录。")),
        }
    }
}

pub fn receive_stream(
    stream: &mut (impl Read + Write),
    current: impl Fn() -> bool,
    mut on_progress: impl FnMut(u64, u64),
    mut on_chunk: impl FnMut(usize, Payload) -> Result<(), String>,
) -> io::Result<Result<Metadata, String>> {
    let mut textures = Vec::new();
    let mut parts = Vec::new();
    let mut buffers: Vec<Buffer> = Vec::new();
    let mut pending = std::collections::VecDeque::new();
    let mut total = 0usize;
    let mut cancelled = false;
    let mut progress = None;
    let mut published: Option<Metadata> = None;
    let mut buffer_offset = 0usize;
    let mut texture_offset = 0usize;
    let mut consumer_error = None;
    loop {
        let (kind, bytes) = read_packet(stream)?;
        cancelled |= !current();
        if kind == ERROR {
            let error =
                String::from_utf8(bytes).map_err(|_| invalid("解码器错误文本无效。"))?;
            return Ok(Err(consumer_error.unwrap_or_else(|| {
                if cancelled {
                    "Cancelled".into()
                } else {
                    error
                }
            })));
        }
        if kind == END {
            if cancelled {
                return Ok(Err(consumer_error.unwrap_or_else(|| "Cancelled".into())));
            }
            if !pending.is_empty() || !buffers.is_empty() {
                return Err(invalid("解码器预览数据被截断。"));
            }
            if progress.is_some_and(|(completed, total)| completed != total) {
                return Err(invalid("解码器网格进度不完整。"));
            }
            let mut metadata = published.ok_or_else(|| invalid("解码器未返回任何区块。"))?;
            let summary = parse_summary(&bytes, metadata.triangle_count)?;
            if summary.block_count != metadata.block_count
                || summary.block_entity_count != metadata.block_entity_count
                || summary.min != metadata.min
                || summary.max != metadata.max
            {
                return Err(invalid("解码器汇总与区块不匹配。"));
            }
            metadata.materials = summary.materials;
            metadata.replaced = summary.replaced;
            return Ok(Ok(metadata));
        }
        if cancelled {
            buffers.clear();
            textures.clear();
            parts.clear();
            pending.clear();
            published = None;
            stream.write_all(&[CANCEL])?;
            continue;
        }
        match kind {
            CHUNK if pending.is_empty() => {
                let counted = parts
                    .iter()
                    .try_fold(0u64, |count, part: &PartMetadata| {
                        count.checked_add(u64::from(part.index_count / 3))
                    })
                    .ok_or_else(|| invalid(TOO_LARGE))?;
                let summary = parse_summary(&bytes, counted)?;
                let metadata = Metadata {
                    block_count: summary.block_count,
                    block_entity_count: summary.block_entity_count,
                    triangle_count: counted,
                    min: summary.min,
                    max: summary.max,
                    byte_length: std::mem::take(&mut total),
                    textures: std::mem::take(&mut textures),
                    parts: std::mem::take(&mut parts),
                    materials: Vec::new(),
                    replaced: 0,
                };
                let aggregate = published.get_or_insert_with(|| Metadata {
                    block_count: metadata.block_count,
                    block_entity_count: metadata.block_entity_count,
                    triangle_count: 0,
                    min: metadata.min,
                    max: metadata.max,
                    byte_length: 0,
                    textures: Vec::new(),
                    parts: Vec::new(),
                    materials: Vec::new(),
                    replaced: 0,
                });
                if aggregate.block_count != metadata.block_count
                    || aggregate.block_entity_count != metadata.block_entity_count
                {
                    return Err(invalid("解码器区块来源数量发生了变化。"));
                }
                aggregate.triangle_count = aggregate
                    .triangle_count
                    .checked_add(counted)
                    .ok_or_else(|| invalid(TOO_LARGE))?;
                aggregate.byte_length = aggregate
                    .byte_length
                    .checked_add(metadata.byte_length)
                    .ok_or_else(|| invalid(TOO_LARGE))?;
                for axis in 0..3 {
                    aggregate.min[axis] = aggregate.min[axis].min(metadata.min[axis]);
                    aggregate.max[axis] = aggregate.max[axis].max(metadata.max[axis]);
                }
                let next_buffer_offset = buffer_offset
                    .checked_add(buffers.len())
                    .ok_or_else(|| invalid(TOO_LARGE))?;
                for texture in &metadata.textures {
                    let mut texture = texture.clone();
                    texture.buffer_id += buffer_offset;
                    aggregate.textures.push(texture);
                }
                for part in &metadata.parts {
                    let mut part = part.clone();
                    for id in &mut part.buffers {
                        *id += buffer_offset;
                    }
                    aggregate.parts.push(part);
                }
                buffer_offset = next_buffer_offset;
                let previous_textures = texture_offset;
                texture_offset = aggregate.textures.len();
                let payload = Payload {
                    metadata,
                    buffers: std::mem::take(&mut buffers),
                };
                if let Err(error) = on_chunk(previous_textures, payload) {
                    consumer_error = Some(error);
                    cancelled = true;
                }
            }
            TEXTURE if pending.is_empty() && parts.is_empty() => {
                let record: TextureRecord =
                    serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
                let size = (record.width as usize)
                    .checked_mul(record.height as usize)
                    .and_then(|n| n.checked_mul(4));
                if record.width == 0 || record.height == 0 || size != Some(record.byte_length) {
                    return Err(invalid("解码器返回了无效的纹理。"));
                }
                let id = add_buffer(&mut buffers, &mut total, record.byte_length)?;
                pending.push_back((id, None));
                textures.push(TextureMetadata {
                    width: record.width,
                    height: record.height,
                    byte_length: record.byte_length,
                    buffer_id: id,
                    repeat: record.repeat,
                });
            }
            PART if pending.is_empty() => {
                let record: PartRecord =
                    serde_json::from_slice(&bytes).map_err(|e| invalid(e.to_string()))?;
                if record.vertex_count == 0
                    || record.vertex_count > i32::MAX as u32
                    || record.index_count == 0
                    || record.index_count > i32::MAX as u32
                    || record.index_count % 3 != 0
                    || record.texture_index as usize >= texture_offset + textures.len()
                    || record.alpha_mode > 2
                {
                    return Err(invalid("解码器返回了无效的网格部件。"));
                }
                let v = record.vertex_count as usize;
                let lengths = [
                    v.checked_mul(12),
                    v.checked_mul(3),
                    v.checked_mul(8),
                    v.checked_mul(4),
                    (record.index_count as usize).checked_mul(4),
                ];
                let mut ids = [0; 5];
                for (i, length) in lengths.into_iter().enumerate() {
                    let length = length.ok_or_else(|| invalid(TOO_LARGE))?;
                    ids[i] = add_buffer(&mut buffers, &mut total, length)?;
                    pending.push_back((
                        ids[i],
                        if i == 4 {
                            Some(record.vertex_count)
                        } else {
                            None
                        },
                    ));
                }
                parts.push(PartMetadata {
                    vertex_count: record.vertex_count,
                    index_count: record.index_count,
                    texture_index: record.texture_index,
                    alpha_mode: record.alpha_mode,
                    buffers: ids,
                });
            }
            DATA => {
                let &(id, index_limit) = pending
                    .front()
                    .ok_or_else(|| invalid("意外的预览缓冲区。"))?;
                let buffer = &mut buffers[id];
                let received = buffer.ends.last().copied().unwrap_or(0);
                let expected = buffer
                    .length
                    .checked_sub(received)
                    .ok_or_else(|| invalid("解码器缓冲区过大。"))?
                    .min(FRAME_BYTES);
                if bytes.len() != expected {
                    return Err(invalid("解码器缓冲区被截断或过大。"));
                }
                if let Some(limit) = index_limit {
                    if bytes
                        .chunks_exact(4)
                        .any(|value| u32::from_le_bytes(value.try_into().unwrap()) >= limit)
                    {
                        return Err(invalid("解码器返回了超出范围的网格索引。"));
                    }
                }
                buffer.segments.push(bytes);
                buffer.ends.push(received + expected);
                if received + expected == buffer.length {
                    pending.pop_front();
                }
            }
            PROGRESS if pending.is_empty() && bytes.len() == 16 => {
                let completed = u64::from_le_bytes(bytes[..8].try_into().unwrap());
                let count = u64::from_le_bytes(bytes[8..].try_into().unwrap());
                if count == 0
                    || completed > count
                    || progress.map_or(completed != 0, |(previous, total)| {
                        count != total || completed <= previous
                    })
                {
                    return Err(invalid("解码器返回了无效的网格进度。"));
                }
                progress = Some((completed, count));
                on_progress(completed, count);
            }
            CHECKPOINT if pending.is_empty() && bytes.is_empty() => {}
            _ => return Err(invalid("意外的解码器记录。")),
        }
        stream.write_all(&[if cancelled { CANCEL } else { CONTINUE }])?;
    }
}

fn parse_summary(bytes: &[u8], counted: u64) -> io::Result<Summary> {
    let summary: Summary = serde_json::from_slice(bytes).map_err(|e| invalid(e.to_string()))?;
    if summary.block_count <= 0
        || summary.block_entity_count < 0
        || counted == 0
        || summary.triangle_count != counted
        || !summary
            .min
            .iter()
            .chain(&summary.max)
            .all(|value| value.is_finite())
        || (0..3).any(|axis| summary.min[axis] > summary.max[axis])
    {
        return Err(invalid("解码器返回了无效的预览元数据。"));
    }
    Ok(summary)
}

fn add_buffer(buffers: &mut Vec<Buffer>, total: &mut usize, length: usize) -> io::Result<usize> {
    *total = total
        .checked_add(length)
        .ok_or_else(|| invalid(TOO_LARGE))?;
    if length == 0 {
        return Err(invalid("解码器返回了空缓冲区。"));
    }
    let id = buffers.len();
    buffers.push(Buffer {
        segments: Vec::new(),
        ends: Vec::new(),
        length,
    });
    Ok(id)
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    fn range(buffer_id: usize, offset: usize, length: usize) -> ReadRange {
        ReadRange {
            buffer_id,
            offset,
            length,
        }
    }

    #[test]
    fn native_mesh_encoder_delivers_reusable_textures_before_completion() {
        use litematica_preview_native::{load_chunks, PreviewOptions};
        use nucleation::{BlockState, UniversalSchematic};
        use std::net::{TcpListener, TcpStream};
        use std::sync::atomic::{AtomicBool, Ordering};
        use std::time::Duration;

        let mut schematic = UniversalSchematic::new("pipeline regression".into());
        for x in [0, 16, 32] {
            schematic.set_block(x, 0, 0, &BlockState::new("minecraft:stone"));
            schematic.set_block(x + 1, 0, 0, &BlockState::new("minecraft:glass"));
        }
        let input = nucleation::formats::litematic::to_litematic(&schematic).unwrap();
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let mut sender = TcpStream::connect(listener.local_addr().unwrap()).unwrap();
        let (mut receiver, _) = listener.accept().unwrap();
        sender
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        receiver
            .set_read_timeout(Some(Duration::from_secs(10)))
            .unwrap();
        let ended = AtomicBool::new(false);
        std::thread::scope(|scope| {
            let producer = scope.spawn(|| {
                let pack_bytes = std::fs::read(
                    std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../Assets/pack.zip"),
                )
                .unwrap();
                let pack =
                    nucleation::meshing::ResourcePackSource::from_bytes(&pack_bytes).unwrap();
                let mut encoder = Encoder::new(&mut sender);
                let mut native_alpha = std::collections::BTreeSet::new();
                let info = load_chunks(
                    &input,
                    &pack,
                    PreviewOptions {
                        chunk_size: Some(16),
                        thread_count: (litematica_preview_native::max_worker_threads() >= 2)
                            .then_some(2),
                        ..PreviewOptions::default()
                    },
                    &[],
                    |preview| {
                        native_alpha.extend(parts(&preview.mesh).map(|(_, _, alpha)| alpha));
                        encoder.chunk(preview)
                    },
                    |_, _| Ok(()),
                    || Ok(()),
                );
                drop(encoder);
                finish(&mut sender, info.map(|loaded| loaded)).unwrap();
                ended.store(true, Ordering::Release);
                native_alpha
            });
            let mut chunks = 0;
            let mut textures = 0;
            let mut bytes = 0;
            let mut triangles = 0;
            let mut alpha = std::collections::BTreeSet::new();
            let complete = receive_stream(
                &mut receiver,
                || true,
                |_, _| {},
                |offset, payload| {
                    if chunks == 0 {
                        assert!(!ended.load(Ordering::Acquire));
                    } else {
                        assert!(
                            payload.metadata.textures.is_empty(),
                            "shared textures were sent again"
                        );
                    }
                    assert_eq!(offset, textures);
                    textures += payload.metadata.textures.len();
                    bytes += payload.metadata.byte_length;
                    triangles += payload.metadata.triangle_count;
                    for part in &payload.metadata.parts {
                        assert!((part.texture_index as usize) < textures);
                        alpha.insert(part.alpha_mode);
                        let ranges: Vec<_> = part
                            .buffers
                            .iter()
                            .map(|&id| range(id, 0, payload.buffers[id].length))
                            .collect();
                        let page = payload.read_ranges(&ranges).unwrap();
                        let mut cursor = 0;
                        for descriptor in ranges {
                            cursor = align4(cursor).unwrap();
                            let source = &payload.buffers[descriptor.buffer_id];
                            let expected: Vec<_> =
                                source.segments.iter().flatten().copied().collect();
                            assert_eq!(&page[cursor..cursor + descriptor.length], expected);
                            cursor += descriptor.length;
                        }
                        assert_eq!(page.len(), cursor);
                        let indices = payload
                            .read_ranges(&[range(
                                part.buffers[4],
                                0,
                                part.index_count as usize * 4,
                            )])
                            .unwrap();
                        assert!(indices
                            .chunks_exact(4)
                            .all(|bytes| u32::from_le_bytes(bytes.try_into().unwrap())
                                < part.vertex_count));
                    }
                    chunks += 1;
                    Ok(())
                },
            )
            .unwrap()
            .unwrap();
            let native_alpha = producer.join().unwrap();
            assert_eq!(chunks, 3);
            assert_eq!(complete.block_count, 6);
            assert_eq!(complete.triangle_count, triangles);
            assert_eq!(complete.byte_length, bytes);
            assert_eq!(complete.textures.len(), textures);
            assert_eq!(alpha, native_alpha);
        });
    }

    pub(crate) fn chunk_payload() -> Payload {
        receive(&mut wire(part_packets(0)), || true, |_, _| {})
            .unwrap()
            .unwrap()
    }

    pub(crate) fn stream_fixture(chunks: usize) -> Vec<u8> {
        let mut source = io::Cursor::new(part_packets(0));
        let mut first = Vec::new();
        let mut reused = Vec::new();
        loop {
            let (kind, bytes) = read_packet(&mut source).unwrap();
            if kind == END {
                break;
            }
            write_packet(&mut first, kind, &bytes).unwrap();
            if kind != TEXTURE && !(kind == DATA && reused.is_empty()) {
                write_packet(&mut reused, kind, &bytes).unwrap();
            }
        }
        let mut bytes = first;
        for _ in 1..chunks {
            bytes.extend_from_slice(&reused);
        }
        finish(
            &mut bytes,
            Ok(litematica_preview_native::LoadedPreview {
                info: PreviewInfo {
                    block_count: 1,
                    triangle_count: chunks as u64,
                    max: [1.0; 3],
                    ..PreviewInfo::default()
                },
                materials: Vec::new(),
                replaced: 0,
                container_item_count: 0,
            }),
        )
        .unwrap();
        bytes
    }

    #[test]
    fn streamed_chunks_reuse_textures_and_preserve_global_buffer_descriptors() {
        let mut batches = Vec::new();
        let metadata = receive_stream(
            &mut wire(stream_fixture(2)),
            || true,
            |_, _| {},
            |offset, payload| {
                batches.push((offset, payload));
                Ok(())
            },
        )
        .unwrap()
        .unwrap();
        assert_eq!(batches[0].0, 0);
        assert_eq!(batches[1].0, 1);
        assert_eq!(batches[0].1.metadata.textures.len(), 1);
        assert!(batches[1].1.metadata.textures.is_empty());
        assert_eq!(batches[1].1.metadata.parts[0].texture_index, 0);
        assert_eq!(batches[1].1.metadata.parts[0].buffers, [0, 1, 2, 3, 4]);
        assert_eq!(metadata.parts[1].buffers, [6, 7, 8, 9, 10]);
        assert_eq!(metadata.byte_length, 4 + 2 * 39);
        assert_eq!(metadata.triangle_count, 2);
    }

    #[test]
    fn serial_receiver_still_merges_completed_chunks_and_rebases_indices() {
        let payload = receive(&mut wire(stream_fixture(2)), || true, |_, _| {})
            .unwrap()
            .unwrap();
        assert_eq!(payload.metadata.textures.len(), 1);
        assert_eq!(payload.metadata.parts.len(), 1);
        let part = &payload.metadata.parts[0];
        assert_eq!((part.vertex_count, part.index_count), (2, 6));
        let indices: Vec<_> = payload
            .read_ranges(&[range(part.buffers[4], 0, 24)])
            .unwrap()
            .chunks_exact(4)
            .map(|bytes| u32::from_le_bytes(bytes.try_into().unwrap()))
            .collect();
        assert_eq!(indices, [0, 0, 0, 1, 1, 1]);
    }

    #[test]
    fn rejected_chunk_acknowledges_cancel_and_drains_before_next_request() {
        let mut input = io::Cursor::new(stream_fixture(1));
        let mut bytes = Vec::new();
        loop {
            let (kind, data) = read_packet(&mut input).unwrap();
            write_packet(&mut bytes, kind, &data).unwrap();
            if kind == CHUNK {
                break;
            }
        }
        finish(&mut bytes, Err("Cancelled".into())).unwrap();
        bytes.extend(stream_fixture(1));
        let mut connection = wire(bytes);
        let result = receive_stream(
            &mut connection,
            || true,
            |_, _| {},
            |_, _| Err("Upload failed".into()),
        )
        .unwrap();
        assert!(matches!(result, Err(error) if error == "Upload failed"));
        assert_eq!(connection.outgoing.last(), Some(&CANCEL));
        let next = receive(&mut connection, || true, |_, _| {})
            .unwrap()
            .unwrap();
        assert_eq!(next.read_ranges(&[range(0, 0, 4)]).unwrap(), [255; 4]);
    }

    #[test]
    fn malformed_late_terminator_never_completes_a_stream() {
        let mut bytes = stream_fixture(2);
        bytes.pop();
        let mut batches = 0;
        assert!(receive_stream(
            &mut wire(bytes),
            || true,
            |_, _| {},
            |_, _| {
                batches += 1;
                Ok(())
            }
        )
        .is_err());
        assert_eq!(batches, 2);
    }

    #[test]
    fn encoder_texture_identity_survives_multiple_chunks() {
        let mut connection = wire(vec![CONTINUE; 4]);
        let mut encoder = Encoder::new(&mut connection);
        assert_eq!(encoder.texture(1, 1, &[255; 4], false).unwrap(), 0);
        assert_eq!(encoder.texture(1, 1, &[255; 4], false).unwrap(), 0);
        assert_eq!(encoder.texture(1, 1, &[255; 4], true).unwrap(), 1);
        let mut packets = io::Cursor::new(connection.outgoing);
        assert_eq!(read_packet(&mut packets).unwrap().0, TEXTURE);
        assert_eq!(read_packet(&mut packets).unwrap().0, DATA);
        assert_eq!(read_packet(&mut packets).unwrap().0, TEXTURE);
        assert_eq!(read_packet(&mut packets).unwrap().0, DATA);
        assert_eq!(packets.position() as usize, packets.get_ref().len());
    }

    #[test]
    fn declared_buffers_above_two_gib_wait_for_payload_without_eager_allocation() {
        let mut bytes = Vec::new();
        let texture = TextureRecord {
            width: 1 << 30,
            height: 1,
            byte_length: 4usize << 30,
            repeat: false,
        };
        write_packet(&mut bytes, TEXTURE, &serde_json::to_vec(&texture).unwrap()).unwrap();
        finish(&mut bytes, Err("Stopped before pixel data".into())).unwrap();
        let mut connection = wire(bytes);
        assert!(matches!(
            receive(&mut connection, || true, |_, _| {}).unwrap(),
            Err(error) if error == "Stopped before pixel data"
        ));
        assert_eq!(connection.outgoing, [CONTINUE]);

        let mut buffers = Vec::new();
        let mut total = usize::MAX - 1;
        assert!(add_buffer(&mut buffers, &mut total, 2).is_err());
    }

    #[test]
    fn preview_accepts_more_than_one_hundred_thousand_texture_records() {
        let mut bytes = Vec::new();
        let texture = serde_json::to_vec(&TextureRecord {
            width: 1,
            height: 1,
            byte_length: 4,
            repeat: false,
        })
        .unwrap();
        for _ in 0..100_000 {
            write_packet(&mut bytes, TEXTURE, &texture).unwrap();
            write_packet(&mut bytes, DATA, &[255; 4]).unwrap();
        }
        bytes.extend(part_packets(0));
        let payload = receive(&mut wire(bytes), || true, |_, _| {})
            .unwrap()
            .unwrap();
        assert_eq!(payload.metadata.textures.len(), 100_001);
        let last = payload.metadata.textures.last().unwrap();
        assert_eq!(
            payload.read_ranges(&[range(last.buffer_id, 0, 4)]).unwrap(),
            [255; 4]
        );
        assert_eq!(payload.metadata.triangle_count, 1);
    }

    #[test]
    fn gpu_batch_count_boundary_starts_a_new_draw_instead_of_rejecting_the_model() {
        let max_triangular_indices = i32::MAX as u32 - 1;
        let records = vec![
            PartMetadata {
                vertex_count: 1,
                index_count: max_triangular_indices,
                texture_index: 0,
                alpha_mode: 0,
                buffers: [0, 1, 2, 3, 4],
            },
            PartMetadata {
                vertex_count: 1,
                index_count: 3,
                texture_index: 0,
                alpha_mode: 0,
                buffers: [5, 6, 7, 8, 9],
            },
        ];
        let source = (0..10).map(|_| Buffer::default()).collect();
        let payload = Payload::assemble(
            Metadata {
                block_count: 2,
                block_entity_count: 0,
                triangle_count: u64::from(max_triangular_indices / 3) + 1,
                min: [0.0; 3],
                max: [1.0; 3],
                byte_length: 0,
                textures: vec![],
                parts: records,
                materials: Vec::new(),
                replaced: 0,
            },
            source,
            || true,
        )
        .unwrap();
        assert_eq!(payload.metadata.parts.len(), 2);
        assert_eq!(
            payload.metadata.parts[0].index_count,
            max_triangular_indices
        );
        assert_eq!(payload.metadata.parts[1].index_count, 3);
    }

    #[test]
    fn range_reads_cross_segments_without_flattening_and_reject_overflow() {
        let payload = Payload {
            metadata: Metadata {
                block_count: 1,
                block_entity_count: 0,
                triangle_count: 1,
                min: [0.0; 3],
                max: [1.0; 3],
                byte_length: FRAME_BYTES + 3,
                textures: vec![],
                parts: vec![],
                materials: Vec::new(),
                replaced: 0,
            },
            buffers: vec![Buffer {
                segments: vec![vec![7; FRAME_BYTES], vec![8, 9, 10]],
                ends: vec![FRAME_BYTES, FRAME_BYTES + 3],
                length: FRAME_BYTES + 3,
            }],
        };
        assert_eq!(
            payload
                .read_ranges(&[range(0, FRAME_BYTES - 2, 5)])
                .unwrap(),
            [7, 7, 8, 9, 10]
        );
        assert!(payload.read_ranges(&[range(0, usize::MAX, 1)]).is_err());
        assert!(payload
            .read_ranges(&[range(0, 0, FRAME_BYTES + 1)])
            .is_err());
        assert!(payload.read_ranges(&[range(1, 0, 1)]).is_err());
    }

    #[test]
    fn packed_reads_align_ranges_cross_segments_and_preserve_sources() {
        let payload = Payload {
            metadata: Metadata {
                block_count: 0,
                block_entity_count: 0,
                triangle_count: 0,
                min: [0.0; 3],
                max: [0.0; 3],
                byte_length: 11,
                textures: vec![],
                parts: vec![],
                materials: Vec::new(),
                replaced: 0,
            },
            buffers: vec![
                Buffer {
                    segments: vec![vec![1, 2], vec![3, 4, 5]],
                    ends: vec![2, 5],
                    length: 5,
                },
                Buffer {
                    segments: vec![vec![6, 7, 8, 9]],
                    ends: vec![4],
                    length: 4,
                },
            ],
        };
        assert_eq!(
            payload
                .read_ranges(&[range(0, 0, 3), range(1, 0, 4)])
                .unwrap(),
            [1, 2, 3, 0, 6, 7, 8, 9]
        );
        assert_eq!(
            payload
                .read_ranges(&[range(0, 1, 3), range(0, 1, 3), range(1, 2, 2)])
                .unwrap(),
            [2, 3, 4, 0, 2, 3, 4, 0, 8, 9]
        );
        assert_eq!(
            payload.read_ranges(&[range(0, 0, 5)]).unwrap(),
            [1, 2, 3, 4, 5]
        );
    }

    #[test]
    fn packed_reads_validate_every_range_before_returning_a_page() {
        let payload = Payload {
            metadata: Metadata {
                block_count: 0,
                block_entity_count: 0,
                triangle_count: 0,
                min: [0.0; 3],
                max: [0.0; 3],
                byte_length: FRAME_BYTES,
                textures: vec![],
                parts: vec![],
                materials: Vec::new(),
                replaced: 0,
            },
            buffers: vec![Buffer {
                segments: vec![vec![42; FRAME_BYTES]],
                ends: vec![FRAME_BYTES],
                length: FRAME_BYTES,
            }],
        };
        assert_eq!(
            payload
                .read_ranges(&[range(0, 0, FRAME_BYTES)])
                .unwrap()
                .len(),
            FRAME_BYTES
        );
        assert_eq!(
            payload
                .read_ranges(&[range(0, 0, FRAME_BYTES - 3), range(0, 0, 1)])
                .unwrap_err(),
            "预览范围无效。"
        );
        assert_eq!(
            payload.read_ranges(&[]).unwrap_err(),
            "预览范围无效。"
        );
        assert_eq!(
            payload
                .read_ranges(&vec![range(0, 0, 1); MAX_READ_RANGES + 1])
                .unwrap_err(),
            "预览范围无效。"
        );
        for bad in [
            range(0, 0, 0),
            range(1, 0, 1),
            range(0, FRAME_BYTES, 1),
            range(0, usize::MAX, 2),
        ] {
            assert_eq!(
                payload.read_ranges(&[range(0, 0, 1), bad]).unwrap_err(),
                "预览范围无效。"
            );
        }
        assert_eq!(
            payload
                .read_ranges(&[range(0, 0, 1); MAX_READ_RANGES])
                .unwrap()
                .len(),
            MAX_READ_RANGES * 4 - 3
        );
    }

    #[test]
    fn read_ranges_rejects_invalid_json_fields_and_numeric_types() {
        assert!(serde_json::from_str::<ReadRange>(
            r#"{"bufferId":0,"offset":0,"length":1,"target":0}"#
        )
        .is_err());
        assert!(
            serde_json::from_str::<ReadRange>(r#"{"bufferId":-1,"offset":0,"length":1}"#).is_err()
        );
        assert!(
            serde_json::from_str::<ReadRange>(r#"{"bufferId":0,"offset":0.5,"length":1}"#).is_err()
        );
    }

    #[test]
    fn merging_chunks_rebases_indices_and_preserves_attribute_boundaries() {
        let mut buffers = Vec::new();
        let mut records = Vec::new();
        for value in [10u8, 20] {
            let base = buffers.len();
            for data in [
                vec![value; 12],
                vec![value; 3],
                vec![value; 8],
                vec![value; 4],
                vec![0; 12],
            ] {
                let length = data.len();
                buffers.push(Buffer {
                    segments: vec![data],
                    ends: vec![length],
                    length,
                });
            }
            records.push(PartMetadata {
                vertex_count: 1,
                index_count: 3,
                texture_index: 0,
                alpha_mode: 0,
                buffers: [base, base + 1, base + 2, base + 3, base + 4],
            });
        }
        let metadata = Metadata {
            block_count: 2,
            block_entity_count: 0,
            triangle_count: 2,
            min: [0.0; 3],
            max: [1.0; 3],
            byte_length: 78,
            textures: vec![],
            parts: records,
            materials: Vec::new(),
            replaced: 0,
        };
        let payload = Payload::assemble(metadata, buffers, || true).unwrap();
        let part = &payload.metadata.parts[0];
        assert_eq!(payload.metadata.parts.len(), 1);
        assert_eq!((part.vertex_count, part.index_count), (2, 6));
        assert_eq!(
            payload
                .read_ranges(&[range(part.buffers[1], 2, 3)])
                .unwrap(),
            [10, 20, 20]
        );
        let indices = payload
            .read_ranges(&[range(part.buffers[4], 0, 24)])
            .unwrap();
        let values: Vec<_> = indices
            .chunks_exact(4)
            .map(|v| u32::from_le_bytes(v.try_into().unwrap()))
            .collect();
        assert_eq!(values, [0, 0, 0, 1, 1, 1]);
    }

    struct Wire {
        incoming: io::Cursor<Vec<u8>>,
        outgoing: Vec<u8>,
    }

    impl Read for Wire {
        fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
            self.incoming.read(out)
        }
    }

    impl Write for Wire {
        fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
            self.outgoing.extend_from_slice(bytes);
            Ok(bytes.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    fn wire(bytes: Vec<u8>) -> Wire {
        Wire {
            incoming: io::Cursor::new(bytes),
            outgoing: Vec::new(),
        }
    }

    fn part_packets(index: u32) -> Vec<u8> {
        let mut bytes = Vec::new();
        let texture = TextureRecord {
            width: 1,
            height: 1,
            byte_length: 4,
            repeat: false,
        };
        write_packet(&mut bytes, TEXTURE, &serde_json::to_vec(&texture).unwrap()).unwrap();
        write_packet(&mut bytes, DATA, &[255; 4]).unwrap();
        let part = PartRecord {
            vertex_count: 1,
            index_count: 3,
            texture_index: 0,
            alpha_mode: 0,
        };
        write_packet(&mut bytes, PART, &serde_json::to_vec(&part).unwrap()).unwrap();
        for data in [
            vec![0; 12],
            vec![0; 3],
            vec![0; 8],
            vec![255; 4],
            [index.to_le_bytes(); 3].concat(),
        ] {
            write_packet(&mut bytes, DATA, &data).unwrap();
        }
        write_packet(
            &mut bytes,
            CHUNK,
            &serde_json::to_vec(&Summary {
                block_count: 1,
                block_entity_count: 0,
                triangle_count: 1,
                min: [0.0; 3],
                max: [1.0; 3],
                materials: Vec::new(),
                replaced: 0,
            })
            .unwrap(),
        )
        .unwrap();
        finish(
            &mut bytes,
            Ok(litematica_preview_native::LoadedPreview {
                info: PreviewInfo {
                    block_count: 1,
                    triangle_count: 1,
                    max: [1.0; 3],
                    ..PreviewInfo::default()
                },
                materials: Vec::new(),
                replaced: 0,
                container_item_count: 0,
            }),
        )
        .unwrap();
        bytes
    }

    #[test]
    fn malformed_geometry_is_rejected_before_exposing_buffers() {
        assert!(receive(&mut wire(part_packets(1)), || true, |_, _| {}).is_err());
        let mut truncated = part_packets(0);
        truncated.truncate(truncated.len() - 1);
        assert!(receive(&mut wire(truncated), || true, |_, _| {}).is_err());
        let oversized = [DATA, 1, 0, 16, 0];
        assert!(receive(&mut wire(oversized.to_vec()), || true, |_, _| {}).is_err());
    }

    #[test]
    fn cancellation_drains_terminator_before_the_next_preview() {
        let mut bytes = Vec::new();
        write_packet(&mut bytes, CHECKPOINT, &[]).unwrap();
        finish(&mut bytes, Err("Cancelled".into())).unwrap();
        bytes.extend(part_packets(0));
        let mut connection = wire(bytes);
        assert!(
            matches!(receive(&mut connection, || false, |_, _| {}).unwrap(), Err(e) if e == "Cancelled")
        );
        assert_eq!(connection.outgoing, [CANCEL]);
        let payload = receive(&mut connection, || true, |_, _| {})
            .unwrap()
            .unwrap();
        assert_eq!(
            payload
                .read_ranges(&[range(payload.metadata.parts[0].buffers[4], 0, 12)])
                .unwrap(),
            [0; 12]
        );
    }

    #[test]
    fn mesh_progress_is_acknowledged_and_validated_between_previews() {
        let mut bytes = Vec::new();
        let mut initial = [0; 16];
        initial[8..].copy_from_slice(&1u64.to_le_bytes());
        write_packet(&mut bytes, PROGRESS, &initial).unwrap();
        let mut completed = [0; 16];
        completed[..8].copy_from_slice(&1u64.to_le_bytes());
        completed[8..].copy_from_slice(&1u64.to_le_bytes());
        write_packet(&mut bytes, PROGRESS, &completed).unwrap();
        bytes.extend(part_packets(0));
        let mut connection = wire(bytes);
        let mut observed = Vec::new();
        receive(
            &mut connection,
            || true,
            |done, total| observed.push((done, total)),
        )
        .unwrap()
        .unwrap();
        assert_eq!(observed, [(0, 1), (1, 1)]);
        assert_eq!(&connection.outgoing[..2], &[CONTINUE, CONTINUE]);

        for invalid_bytes in [vec![0; 15], {
            let mut count = [0; 16];
            count[..8].copy_from_slice(&2u64.to_le_bytes());
            count[8..].copy_from_slice(&1u64.to_le_bytes());
            count.to_vec()
        }] {
            let mut bytes = Vec::new();
            write_packet(&mut bytes, PROGRESS, &invalid_bytes).unwrap();
            assert!(receive(&mut wire(bytes), || true, |_, _| {}).is_err());
        }
    }

    #[test]
    fn mesh_progress_rejects_incomplete_or_regressing_results() {
        let mut bytes = Vec::new();
        let mut first = [0; 16];
        first[8..].copy_from_slice(&2u64.to_le_bytes());
        write_packet(&mut bytes, PROGRESS, &first).unwrap();
        bytes.extend(part_packets(0));
        assert!(receive(&mut wire(bytes), || true, |_, _| {}).is_err());

        let mut bytes = Vec::new();
        write_packet(&mut bytes, PROGRESS, &first).unwrap();
        write_packet(&mut bytes, PROGRESS, &first).unwrap();
        assert!(receive(&mut wire(bytes), || true, |_, _| {}).is_err());
    }

    #[test]
    fn old_upload_release_cannot_free_a_new_request() {
        let worker = crate::preview::PreviewWorker::default();
        worker.advance(10);
        let payload = receive(&mut wire(part_packets(0)), || true, |_, _| {})
            .unwrap()
            .unwrap();
        worker.publish(10, payload).unwrap();
        worker.release(9);
        assert_eq!(worker.read(10, None, &[range(0, 0, 4)]).unwrap(), [255; 4]);
        worker.advance(11);
        assert!(worker.read(10, None, &[range(0, 0, 4)]).is_err());
        let payload = receive(&mut wire(part_packets(0)), || true, |_, _| {})
            .unwrap()
            .unwrap();
        worker.publish(11, payload).unwrap();
        worker.release(10);
        assert_eq!(worker.read(11, None, &[range(0, 0, 4)]).unwrap(), [255; 4]);
        worker.release(11);
        assert!(worker.read(11, None, &[range(0, 0, 4)]).is_err());
    }

    #[test]
    fn concurrent_upload_ranges_preserve_bytes_and_release_invalidates_reads() {
        let worker = crate::preview::PreviewWorker::default();
        worker.advance(10);
        let payload = receive(&mut wire(part_packets(0)), || true, |_, _| {})
            .unwrap()
            .unwrap();
        let texture = payload.metadata.textures[0].buffer_id;
        let positions = payload.metadata.parts[0].buffers[0];
        worker.publish(10, payload).unwrap();
        let start = std::sync::Barrier::new(4);
        std::thread::scope(|scope| {
            let readers: Vec<_> = (0..4)
                .map(|offset| {
                    let worker = &worker;
                    let start = &start;
                    scope.spawn(move || {
                        start.wait();
                        assert_eq!(
                            worker
                                .read(
                                    10,
                                    None,
                                    &[range(texture, offset, 1), range(positions, offset * 3, 3)]
                                )
                                .unwrap(),
                            [255, 0, 0, 0, 0, 0, 0]
                        );
                    })
                })
                .collect();
            for reader in readers {
                reader.join().unwrap();
            }
        });
        worker.release(10);
        assert!(worker.read(10, None, &[range(texture, 0, 4)]).is_err());
    }

    #[test]
    fn quantized_attributes_preserve_endpoints_and_rounding_error() {
        let mut connection = wire(vec![CONTINUE; 2]);
        let mut encoder = Encoder::new(&mut connection);
        encoder
            .quantized([-1.0, 0.0, 1.0, 0.5].into_iter(), true)
            .unwrap();
        encoder
            .quantized([0.0, 0.5, 1.0].into_iter(), false)
            .unwrap();
        let mut records = io::Cursor::new(connection.outgoing);
        assert_eq!(
            read_packet(&mut records).unwrap(),
            (DATA, vec![129, 0, 127, 64])
        );
        assert_eq!(
            read_packet(&mut records).unwrap(),
            (DATA, vec![0, 128, 255])
        );
        assert!((64.0f32 / 127.0 - 0.5).abs() <= 0.5 / 127.0 + f32::EPSILON);
        assert!((128.0f32 / 255.0 - 0.5).abs() <= 0.5 / 255.0 + f32::EPSILON);
    }
}
