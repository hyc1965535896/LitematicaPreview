//! Material-list XLSX export against the Litematica template layout.

use litematica_preview_native::{export_materials_xlsx, BlockReplacement};

#[path = "../src/meshing/tests/fixtures.rs"]
mod fixtures;
use fixtures::schematic;

fn workbook_sheet(data: &[u8]) -> String {
    let mut archive = zip::ZipArchive::new(std::io::Cursor::new(data)).expect("valid xlsx zip");
    let mut sheet = archive
        .by_name("xl/worksheets/sheet1.xml")
        .expect("sheet part");
    let mut text = String::new();
    std::io::Read::read_to_string(&mut sheet, &mut text).expect("readable sheet");
    text
}

fn cell_value(sheet: &str, reference: &str) -> String {
    let start = sheet.find(&format!(r#"r="{reference}""#)).expect("cell present");
    let tail = &sheet[start..];
    let open = tail.find('>').expect("cell open") + 1;
    let close = tail.find("</c>").expect("cell close");
    let body = &tail[open..close];
    if let Some(text) = body.strip_prefix("<is><t>") {
        text.strip_suffix("</t></is>").unwrap_or(text).to_string()
    } else if let Some(text) = body.strip_prefix("<v>") {
        text.strip_suffix("</v>").unwrap_or(text).to_string()
    } else {
        String::new()
    }
}

#[test]
fn exports_template_layout_with_chinese_names_and_box_counts() {
    let data = nucleation::formats::litematic::to_litematic(&schematic(&[
        (0, 0, 0, "minecraft:stone"),
        (1, 0, 0, "minecraft:stone"),
        (2, 0, 0, "minecraft:grass_block"),
    ]))
    .unwrap();
    let export = export_materials_xlsx(&data, "测试投影", &[]).expect("export workbook");
    assert_eq!(export.block_count, 3);
    assert_eq!(export.material_count, 2);
    let sheet = workbook_sheet(&export.data);

    assert_eq!(cell_value(&sheet, "B1"), "测试投影");
    assert_eq!(cell_value(&sheet, "A1"), "投影文件名");
    assert!(sheet.contains(r#"<mergeCells count="1"><mergeCell ref="B1:J1"/>"#));
    assert_eq!(cell_value(&sheet, "A4"), "方块数");
    assert_eq!(cell_value(&sheet, "B4"), "3");
    assert_eq!(cell_value(&sheet, "B6"), "3 × 1 × 1");
    assert_eq!(cell_value(&sheet, "B5"), "3");
    assert_eq!(cell_value(&sheet, "B7"), "7");
    assert_eq!(cell_value(&sheet, "A10"), "投影材料种类");
    assert_eq!(cell_value(&sheet, "B10"), "2");
    assert_eq!(cell_value(&sheet, "A13"), "投影材料列表");
    assert_eq!(cell_value(&sheet, "F13"), "投影容器列表");
    assert_eq!(cell_value(&sheet, "A14"), "物品名称");
    assert_eq!(cell_value(&sheet, "C14"), "总数量");
    assert_eq!(cell_value(&sheet, "J14"), "盒数量");
    // Sorted by count descending: stone first, then grass block.
    assert_eq!(cell_value(&sheet, "A15"), "石头");
    assert_eq!(cell_value(&sheet, "B15"), "stone");
    assert_eq!(cell_value(&sheet, "C15"), "2");
    assert_eq!(cell_value(&sheet, "D15"), "0.1");
    assert_eq!(cell_value(&sheet, "A16"), "草方块");
    assert_eq!(cell_value(&sheet, "B16"), "grass_block");
    assert_eq!(cell_value(&sheet, "C16"), "1");
    // Bold headers use the bold cell style.
    let a14 = &sheet[sheet.find(r#"r="A14""#).expect("A14")..];
    assert!(a14.starts_with(r#"r="A14" s="1""#), "A14 bold");
    let a2 = &sheet[sheet.find(r#"r="A2""#).expect("A2")..];
    assert!(!a2.starts_with(r#"r="A2" s="1""#), "A2 not bold");
}

#[test]
fn replacements_apply_to_the_material_counts() {
    let data = nucleation::formats::litematic::to_litematic(&schematic(&[
        (0, 0, 0, "minecraft:stone"),
        (1, 0, 0, "minecraft:stone"),
        (2, 0, 0, "minecraft:oak_planks"),
    ]))
    .unwrap();
    let export = export_materials_xlsx(
        &data,
        "替换",
        &[BlockReplacement {
            from: "minecraft:stone".into(),
            to: "minecraft:quartz_block".into(),
        }],
    )
    .expect("export with replacement");
    assert_eq!(export.replaced, 2);
    assert_eq!(export.material_count, 2);
    let sheet = workbook_sheet(&export.data);
    assert_eq!(cell_value(&sheet, "A15"), "石英块");
    assert_eq!(cell_value(&sheet, "B15"), "quartz_block");
    assert_eq!(cell_value(&sheet, "C15"), "2");
    assert_eq!(cell_value(&sheet, "B16"), "oak_planks");
}


