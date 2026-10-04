//! Material list export to the same `.xlsx` layout as Litematica's
//! schematic-material template: a metadata block followed by the material
//! table (Chinese name, bare item id, total count, shulker-box count).
//!
//! The workbook is written as a minimal OOXML package (inline strings, one
//! stylesheet entry for bold headers) so it opens in Excel and WPS without
//! extra dependencies.

use std::collections::HashMap;
use std::io::{Cursor, Read, Write};

use nucleation::UniversalSchematic;
use zip::write::SimpleFileOptions;
use zip::ZipWriter;

use crate::containers::{self, ContainerMaterials};
use crate::replace::{apply_replacements, BlockReplacement};

/// Cell style indexes into `xl/styles.xml`: bold text, thin borders, text and
/// numbers inside a table, and the grey table header.
const STYLE_BOLD: u8 = 1;
const STYLE_TEXT: u8 = 3;
const STYLE_NUMBER: u8 = 4;
const STYLE_HEADER: u8 = 5;
const STYLE_DECIMAL: u8 = 6;

/// Column widths for the side-by-side tables: A-D the material list, E the
/// gap, F-J the container contents. Kept narrow enough to print on one
/// landscape page.
const COLUMNS: &[(u32, f64)] = &[
    (1, 20.0),
    (2, 21.0),
    (3, 9.0),
    (4, 9.0),
    (5, 3.0),
    (6, 10.0),
    (7, 18.0),
    (8, 20.0),
    (9, 9.0),
    (10, 9.0),
];

pub struct MaterialsExport {
    pub data: Vec<u8>,
    pub material_count: usize,
    /// Distinct item ids stored inside the schematic's containers.
    pub container_item_count: usize,
    pub block_count: i64,
    pub replaced: i64,
}

pub fn export_materials_xlsx(
    bytes: &[u8],
    file_name: &str,
    replacements: &[BlockReplacement],
) -> Result<MaterialsExport, String> {
    let mut schematic = crate::decode::decode(bytes).map_err(|error| match error {
        crate::decode::DecodeFailure::Format(message) | crate::decode::DecodeFailure::Limit(message) => {
            message
        }
    })?;
    let replaced = apply_replacements(&mut schematic, replacements)?;
    let block_count = i64::from(schematic.total_blocks());
    if block_count == 0 {
        return Err("投影文件不包含可见的几何体。".into());
    }
    let materials = aggregate_materials(&schematic);
    let containers = containers::collect(&schematic);
    let bounds = schematic.get_bounding_box();
    let size = (
        bounds.max.0 - bounds.min.0 + 1,
        bounds.max.1 - bounds.min.1 + 1,
        bounds.max.2 - bounds.min.2 + 1,
    );
    let volume = i64::from(size.0) * i64::from(size.1) * i64::from(size.2);

    let metadata = &schematic.metadata;
    let data_version = metadata
        .source_data_version
        .or(metadata.mc_version)
        .unwrap_or_default();
    let lm_version = litematic_format_version(bytes);
    let workbook = build_workbook(
        file_name,
        metadata.author.as_deref().unwrap_or(""),
        metadata.created,
        lm_version,
        data_version,
        block_count,
        size,
        volume,
        &materials,
        &containers,
    );
    Ok(MaterialsExport {
        data: workbook,
        material_count: materials.len(),
        container_item_count: containers.distinct_items(),
        block_count,
        replaced,
    })
}

/// Merges per-state counts into per-block totals, dropping air, sorted by
/// count descending (then by id for stability) like the template. Blocks
/// without an item of their own count as the item that places them, as in
/// Litematica's own material list.
fn aggregate_materials(schematic: &UniversalSchematic) -> Vec<(String, i64)> {
    let mut totals: HashMap<String, i64> = HashMap::new();
    for (state, count) in schematic.count_block_types() {
        if is_air(&state.name) {
            continue;
        }
        *totals.entry(item_for_material(&state.name)).or_default() += count as i64;
    }
    let mut materials: Vec<(String, i64)> = totals.into_iter().collect();
    materials.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
    materials
}

/// The item a player stocks for a block: wall-mounted variants have no item
/// of their own, so they join their standing base (`oak_wall_sign` →
/// `oak_sign`, `redstone_wall_torch` → `redstone_torch`, `wall_torch` →
/// `torch`, `wither_skeleton_wall_skull` → `wither_skeleton_skull`), exactly
/// how Litematica's material list reports them.
fn item_for_material(block: &str) -> String {
    let id = block.strip_prefix("minecraft:").unwrap_or(block);
    if let Some(base) = id.strip_prefix("wall_") {
        return format!("minecraft:{base}");
    }
    match id.find("_wall_") {
        Some(index) => {
            let tail = index + "_wall_".len();
            format!("minecraft:{}{}", &id[..index + 1], &id[tail..])
        }
        None => block.to_string(),
    }
}

pub(crate) fn is_air(name: &str) -> bool {
    matches!(
        name,
        "minecraft:air" | "minecraft:cave_air" | "minecraft:void_air"
    )
}

/// The Litematica schematic format version (`Version` in the root compound).
/// Nucleation's decoder does not surface it, so the compressed document is
/// re-read here; anything that is not a Litematica file yields `None`.
fn litematic_format_version(bytes: &[u8]) -> Option<i32> {
    let mut decompressed = Vec::new();
    flate2::read::GzDecoder::new(std::io::Cursor::new(bytes))
        .read_to_end(&mut decompressed)
        .ok()?;
    let (root, _) = quartz_nbt::io::read_nbt(
        &mut std::io::Cursor::new(&decompressed),
        quartz_nbt::io::Flavor::Uncompressed,
    )
    .ok()?;
    root.get::<_, i32>("Version").ok()
}

/// Chinese display name for a block or item id. Container contents are item
/// ids, so the block table is consulted first and the item table second.
pub(crate) fn zh_name(id: &str) -> Option<&'static str> {
    static NAMES: std::sync::OnceLock<HashMap<&'static str, &'static str>> =
        std::sync::OnceLock::new();
    let names = NAMES.get_or_init(|| {
        crate::block_names_zh::BLOCK_NAMES_ZH_CN
            .iter()
            .chain(crate::item_names_zh::ITEM_NAMES_ZH_CN.iter())
            .copied()
            .collect()
    });
    names.get(id).copied()
}

fn game_version(data_version: i32) -> Option<&'static str> {
    crate::dv_map::DATA_VERSIONS
        .iter()
        .find(|(version, _)| *version == data_version)
        .map(|(_, name)| *name)
}

/// Unix epoch milliseconds to local `YYYY-MM-DD HH:MM:SS`.
fn format_created(millis: u64) -> String {
    let offset = local_offset_seconds();
    let seconds = (millis / 1000) as i64 + i64::from(offset);
    let days = seconds.div_euclid(86_400);
    let rem = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02} {:02}:{:02}:{:02}",
        rem / 3600,
        (rem / 60) % 60,
        rem % 60
    )
}

/// Howard Hinnant's civil-from-days algorithm, days since 1970-01-01.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as u32;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as u32;
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

#[cfg(windows)]
fn local_offset_seconds() -> i32 {
    use std::mem::MaybeUninit;
    // Bias counts minutes to add to local time to get UTC; negate to add UTC
    // to local. GetTimeZoneInformation includes the active DST adjustment.
    unsafe {
        let mut info = MaybeUninit::<windows_sys::Win32::System::Time::TIME_ZONE_INFORMATION>::uninit();
        let result = windows_sys::Win32::System::Time::GetTimeZoneInformation(info.as_mut_ptr());
        if result == windows_sys::Win32::System::Time::TIME_ZONE_ID_INVALID {
            return 0;
        }
        -i32::from(info.assume_init().Bias) * 60
    }
}

#[cfg(not(windows))]
fn local_offset_seconds() -> i32 {
    0
}

fn build_workbook(
    file_name: &str,
    author: &str,
    created: Option<u64>,
    lm_version: Option<i32>,
    data_version: i32,
    block_count: i64,
    size: (i32, i32, i32),
    volume: i64,
    materials: &[(String, i64)],
    containers: &ContainerMaterials,
) -> Vec<u8> {
    let mut sheet = SheetXml::new();
    // Metadata block, mirroring the template layout.
    sheet.string_cell("A1", "投影文件名", true);
    sheet.merged_string_cell("B1", "B1:J1", file_name, true);
    sheet.string_cell("A2", "保存者游戏 ID", false);
    sheet.string_cell("B2", author, false);
    sheet.string_cell("A3", "创建时间", false);
    sheet.string_cell("B3", &created.map(format_created).unwrap_or_default(), false);
    sheet.string_cell("A4", "方块数", false);
    sheet.number_cell("B4", block_count as f64, "0");
    sheet.string_cell("A5", "体积", false);
    sheet.number_cell("B5", volume as f64, "0");
    sheet.string_cell("A6", "尺寸", false);
    sheet.string_cell("B6", &format!("{} × {} × {}", size.0, size.1, size.2), false);
    sheet.string_cell("A7", "Litematic 版本", false);
    match lm_version {
        Some(version) => sheet.number_cell("B7", f64::from(version), "0"),
        None => sheet.string_cell("B7", "", false),
    }
    sheet.string_cell("A8", "游戏版本", false);
    match game_version(data_version).and_then(|name| name.parse::<f64>().ok()) {
        Some(version) => sheet.number_cell("B8", version, "0.0"),
        None => sheet.string_cell("B8", game_version(data_version).unwrap_or(""), false),
    }
    sheet.string_cell("A9", "数据版本", false);
    if data_version > 0 {
        sheet.number_cell("B9", f64::from(data_version), "0");
    } else {
        sheet.string_cell("B9", "", false);
    }
    sheet.string_cell("A10", "投影材料种类", false);
    sheet.number_cell("B10", materials.len() as f64, "0");
    sheet.string_cell("A11", "容器内材料种类", false);
    sheet.number_cell("B11", containers.distinct_items() as f64, "0");

    // The material list and the container contents sit side by side — columns
    // A-D and F-J, both starting at row 15 — so neither needs scrolling past
    // the other.
    sheet.string_cell("A13", "投影材料列表", true);
    sheet.string_cell("F13", "投影容器列表", true);
    for (column, header) in [("A", "物品名称"), ("B", "物品ID"), ("C", "总数量"), ("D", "盒数量")] {
        sheet.header_cell(&format!("{column}14"), header);
    }
    for (column, header) in [
        ("F", "容器名称"),
        ("G", "容器物品"),
        ("H", "容器物品ID"),
        ("I", "总数量"),
        ("J", "盒数量"),
    ] {
        sheet.header_cell(&format!("{column}14"), header);
    }
    for (index, (name, count)) in materials.iter().enumerate() {
        let row = index + 15;
        let id = name.strip_prefix("minecraft:").unwrap_or(name);
        sheet.string_cell(&format!("A{row}"), zh_name(name).unwrap_or(id), false);
        sheet.string_cell(&format!("B{row}"), id, false);
        sheet.number_cell(&format!("C{row}"), *count as f64, "0");
        let stack = max_stack_size(name);
        sheet.number_cell(&format!("D{row}"), box_count(*count, stack), "0.0");
    }
    // Container contents: one row per item, grouped by the container it sits
    // in. The container name is merged over its rows so each group reads as
    // one block.
    let mut row = 15;
    for (container, items) in &containers.types {
        let container_id = container.strip_prefix("minecraft:").unwrap_or(container);
        let first = row;
        for (item, count) in items {
            let item_id = item.strip_prefix("minecraft:").unwrap_or(item);
            let name = if row == first {
                zh_name(container).unwrap_or(container_id).to_string()
            } else {
                String::new()
            };
            sheet.string_cell(&format!("F{row}"), &name, false);
            sheet.string_cell(&format!("G{row}"), zh_name(item).unwrap_or(item_id), false);
            sheet.string_cell(&format!("H{row}"), item_id, false);
            sheet.number_cell(&format!("I{row}"), *count as f64, "0");
            sheet.number_cell(&format!("J{row}"), box_count(*count, max_stack_size(item)), "0.0");
            row += 1;
        }
        if row > first + 1 {
            sheet.merge(&format!("F{first}:F{}", row - 1));
        }
    }

    write_package(&sheet).expect("xlsx buffer write")
}

/// Boxes of 27 stacks, rounded up to a tenth and never below 0.1. The stack
/// size follows the item: 64 for most blocks, 16 for snowballs, eggs, signs
/// and the like, 1 for minecarts, shulker boxes, boats, buckets, tools — 481
/// hopper minecarts need 17.9 boxes, not 0.3.
fn box_count(count: i64, stack: u32) -> f64 {
    let capacity = 27.0 * f64::from(stack);
    (((count as f64) * 10.0) / capacity).ceil().max(1.0) / 10.0
}

/// The maximum stack size of an item, as in the vanilla registry.
fn max_stack_size(item_id: &str) -> u32 {
    let id = item_id.strip_prefix("minecraft:").unwrap_or(item_id);
    // Unstackable families (max 1): minecarts, boats, rafts, buckets,
    // shulker boxes, beds, tools, weapons, armor, plus one-off gear.
    if id.ends_with("_minecart")
        || id.ends_with("_boat")
        || id == "bamboo_raft"
        || id == "bamboo_chest_raft"
        || id.ends_with("_bucket")
        || id.ends_with("_shulker_box")
        || id.ends_with("_bed")
        || id.contains("_sword")
        || id.contains("_pickaxe")
        || id.contains("_axe")
        || id.contains("_shovel")
        || id.contains("_hoe")
        || id.contains("_helmet")
        || id.contains("_chestplate")
        || id.contains("_leggings")
        || id.contains("_boots")
        || id.ends_with("_horse_armor")
        || id.starts_with("music_disc_")
        || matches!(
            id,
            "bow"
                | "crossbow"
                | "trident"
                | "mace"
                | "shield"
                | "elytra"
                | "shears"
                | "flint_and_steel"
                | "fishing_rod"
                | "brush"
                | "carrot_on_a_stick"
                | "warped_fungus_on_a_stick"
                | "totem_of_undying"
                | "saddle"
                | "bundle"
                | "goat_horn"
                | "potion"
                | "mushroom_stew"
                | "rabbit_stew"
                | "beetroot_soup"
                | "suspicious_stew"
        )
    {
        return 1;
    }
    // Sixteen-stack items: snowballs, eggs, ender pearls, signs (also
    // hanging signs), banners, armor stands, bottles.
    if id.ends_with("_sign") || id.ends_with("_banner") {
        return 16;
    }
    if matches!(
        id,
        "snowball" | "egg" | "blue_egg" | "brown_egg" | "ender_pearl" | "armor_stand"
            | "experience_bottle" | "honey_bottle" | "ominous_bottle"
    ) {
        return 16;
    }
    64
}

/// Minimal OOXML package writer. Cells are collected and emitted grouped
/// into `<row>` elements sorted by row number, cells sorted by column —
/// Excel requires strictly ascending unique rows and silently drops rows
/// otherwise (openpyxl and WPS tolerate the disorder, which is why the bug
/// only showed as an Excel repair dialog).
struct SheetXml {
    /// `(row, column, cell xml)` in write order; sorted on output.
    cells: Vec<(u32, u32, String)>,
    merges: Vec<String>,
}

/// Splits a cell reference like `AB15` into `(15, 28)`.
fn row_and_column(reference: &str) -> (u32, u32) {
    let column = reference
        .find(|c: char| !c.is_ascii_uppercase())
        .expect("cell column letters");
    let (letters, row) = reference.split_at(column);
    let number = letters
        .bytes()
        .fold(0u32, |acc, letter| acc * 26 + u32::from(letter - b'A' + 1));
    (row.parse().expect("cell row number"), number)
}

impl SheetXml {
    fn new() -> Self {
        SheetXml {
            cells: Vec::new(),
            merges: Vec::new(),
        }
    }

    fn place(&mut self, reference: &str, body: String) {
        let (row, column) = row_and_column(reference);
        self.cells.push((row, column, body));
    }

    fn cell(&mut self, reference: &str, content: &str, style: u8, value_type: &str) {
        let style = format!(r#" s="{style}""#);
        let escaped = escape_xml(content);
        let body = if value_type == "inlineStr" {
            format!(
                r#"<c r="{reference}"{style} t="inlineStr"><is><t>{escaped}</t></is></c>"#
            )
        } else {
            format!(r#"<c r="{reference}"{style} t="{value_type}">{escaped}</c>"#)
        };
        self.place(reference, body);
    }

    fn string_cell(&mut self, reference: &str, content: &str, bold: bool) {
        let style = if bold { STYLE_BOLD } else { STYLE_TEXT };
        self.cell(reference, content, style, "inlineStr");
    }

    /// A table header cell: bold on a light fill, centred.
    fn header_cell(&mut self, reference: &str, content: &str) {
        self.cell(reference, content, STYLE_HEADER, "inlineStr");
    }

    fn merged_string_cell(&mut self, reference: &str, range: &str, content: &str, bold: bool) {
        let style = if bold { STYLE_BOLD } else { STYLE_TEXT };
        self.cell(reference, content, style, "inlineStr");
        self.merges.push(format!(r#"<mergeCell ref="{range}"/>"#));
    }

    fn style_number_cell(&mut self, reference: &str, value: f64, format: &str, style: u8) {
        let rendered = if format == "0" {
            format!("{}", value as i64)
        } else {
            format!("{value:.1}")
        };
        self.place(reference, format!(r#"<c r="{reference}" s="{style}"><v>{rendered}</v></c>"#));
    }

    fn number_cell(&mut self, reference: &str, value: f64, format: &str) {
        let style = if format == "0" { STYLE_NUMBER } else { STYLE_DECIMAL };
        self.style_number_cell(reference, value, format, style);
    }

    fn merge(&mut self, range: &str) {
        self.merges.push(format!(r#"<mergeCell ref="{range}"/>"#));
    }

    fn to_xml(&self, columns: &[(u32, f64)]) -> String {
        let mut body = String::from(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<worksheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><sheetViews><sheetView workbookViewId="0"/></sheetViews><sheetFormatPr defaultRowHeight="15"/><cols>"#,
        );
        for (index, width) in columns {
            body.push_str(&format!(
                r#"<col min="{index}" max="{index}" width="{width}" customWidth="1"/>"#
            ));
        }
        body.push_str("</cols><sheetData>");
        // Both tables write into the same rows side by side, so sort by row
        // then column and emit one <row> per row number.
        let mut cells = self.cells.clone();
        cells.sort_unstable_by_key(|(row, column, _)| (*row, *column));
        let mut current = 0u32;
        for (index, (row, _, cell)) in cells.iter().enumerate() {
            if *row != current {
                if index > 0 {
                    body.push_str("</row>");
                }
                current = *row;
                body.push_str(&format!(r#"<row r="{current}">"#));
            }
            body.push_str(cell);
        }
        if !cells.is_empty() {
            body.push_str("</row>");
        }
        body.push_str("</sheetData>");
        if !self.merges.is_empty() {
            body.push_str(&format!(
                r#"<mergeCells count="{}">{}</mergeCells>"#,
                self.merges.len(),
                self.merges.join("")
            ));
        }
        body.push_str("</worksheet>");
        body
    }
}

fn escape_xml(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
}

fn write_package(sheet: &SheetXml) -> Result<Vec<u8>, String> {
    let mut cursor = Cursor::new(Vec::new());
    let options = SimpleFileOptions::default().compression_method(zip::CompressionMethod::Deflated);
    let mut zip = ZipWriter::new(&mut cursor);
    let write = |zip: &mut ZipWriter<&mut Cursor<Vec<u8>>>, name: &str, content: &str| -> Result<(), String> {
        zip.start_file(name, options)
            .map_err(|e| format!("无法写入工作簿条目：{e}"))?;
        zip.write_all(content.as_bytes())
            .map_err(|e| format!("无法写入工作簿条目：{e}"))
    };

    write(
        &mut zip,
        "[Content_Types].xml",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="rels" ContentType="application/vnd.openxmlformats-package.relationships+xml"/><Default Extension="xml" ContentType="application/xml"/><Override PartName="/xl/workbook.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml"/><Override PartName="/xl/worksheets/sheet1.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.worksheet+xml"/><Override PartName="/xl/styles.xml" ContentType="application/vnd.openxmlformats-officedocument.spreadsheetml.styles+xml"/></Types>"#,
    )?;
    write(
        &mut zip,
        "_rels/.rels",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="xl/workbook.xml"/></Relationships>"#,
    )?;
    write(
        &mut zip,
        "xl/workbook.xml",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<workbook xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main" xmlns:r="http://schemas.openxmlformats.org/officeDocument/2006/relationships"><sheets><sheet name="材料清单" sheetId="1" r:id="rId1"/></sheets></workbook>"#,
    )?;
    write(
        &mut zip,
        "xl/_rels/workbook.xml.rels",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/worksheet" Target="worksheets/sheet1.xml"/><Relationship Id="rId2" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/styles" Target="styles.xml"/></Relationships>"#,
    )?;
    write(
        &mut zip,
        "xl/styles.xml",
        r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<styleSheet xmlns="http://schemas.openxmlformats.org/spreadsheetml/2006/main"><numFmts count="1"><numFmt numFmtId="164" formatCode="0.0"/></numFmts><fonts count="2"><font><name val="宋体"/><sz val="11"/></font><font><name val="宋体"/><sz val="11"/><b/></font></fonts><fills count="3"><fill><patternFill patternType="none"/></fill><fill><patternFill patternType="gray125"/></fill><fill><patternFill patternType="solid"><fgColor rgb="FFF2F2F2"/><bgColor indexed="64"/></patternFill></fill></fills><borders count="2"><border><left/><right/><top/><bottom/><diagonal/></border><border><left style="thin"><color rgb="FFBFBFBF"/></left><right style="thin"><color rgb="FFBFBFBF"/></right><top style="thin"><color rgb="FFBFBFBF"/></top><bottom style="thin"><color rgb="FFBFBFBF"/></bottom><diagonal/></border></borders><cellStyleXfs count="1"><xf numFmtId="0" fontId="0" fillId="0" borderId="0"/></cellStyleXfs><cellXfs count="7"><xf numFmtId="0" fontId="0" fillId="0" borderId="0" xfId="0"/><xf numFmtId="0" fontId="1" fillId="0" borderId="0" xfId="0" applyFont="1"/><xf numFmtId="0" fontId="0" fillId="0" borderId="1" xfId="0" applyBorder="1" applyAlignment="1"><alignment vertical="center"/></xf><xf numFmtId="0" fontId="0" fillId="0" borderId="1" xfId="0" applyBorder="1" applyAlignment="1"><alignment horizontal="left" vertical="center"/></xf><xf numFmtId="0" fontId="0" fillId="0" borderId="1" xfId="0" applyBorder="1" applyAlignment="1"><alignment horizontal="right" vertical="center"/></xf><xf numFmtId="0" fontId="1" fillId="2" borderId="1" xfId="0" applyFont="1" applyFill="1" applyBorder="1" applyAlignment="1"><alignment horizontal="center" vertical="center"/></xf><xf numFmtId="164" fontId="0" fillId="0" borderId="1" xfId="0" applyNumberFormat="1" applyBorder="1" applyAlignment="1"><alignment horizontal="right" vertical="center"/></xf></cellXfs></styleSheet>"#,
    )?;
    write(&mut zip, "xl/worksheets/sheet1.xml", &sheet.to_xml(COLUMNS))?;
    zip.finish().map_err(|e| format!("无法完成工作簿：{e}"))?;
    Ok(cursor.into_inner())
}
