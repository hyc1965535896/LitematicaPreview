//! Material-list XLSX export against the Litematica template layout.

use litematica_preview_native::{export_materials_xlsx, BlockReplacement};
use nucleation::block_entity::BlockEntity;
use nucleation::block_position::BlockPosition;
use nucleation::UniversalSchematic;
use quartz_nbt::{NbtCompound, NbtList, NbtTag};

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

/// Whether the sheet writes that cell at all (empty rows are omitted).
fn has_cell(sheet: &str, reference: &str) -> bool {
    sheet.contains(&format!(r#"r="{reference}""#))
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
    // Both tables sit side by side: A-D the materials, F-J the containers.
    assert_eq!(cell_value(&sheet, "A13"), "投影材料列表");
    assert_eq!(cell_value(&sheet, "F13"), "投影容器列表");
    assert_eq!(cell_value(&sheet, "A14"), "物品名称");
    assert_eq!(cell_value(&sheet, "C14"), "总数量");
    assert_eq!(cell_value(&sheet, "D14"), "盒数量");
    assert_eq!(cell_value(&sheet, "F14"), "容器名称");
    assert_eq!(cell_value(&sheet, "G14"), "容器物品");
    assert_eq!(cell_value(&sheet, "H14"), "容器物品ID");
    assert_eq!(cell_value(&sheet, "I14"), "总数量");
    assert_eq!(cell_value(&sheet, "J14"), "盒数量");
    // Sorted by count descending: stone first, then grass block.
    assert_eq!(cell_value(&sheet, "A15"), "石头");
    assert_eq!(cell_value(&sheet, "B15"), "stone");
    assert_eq!(cell_value(&sheet, "C15"), "2");
    assert_eq!(cell_value(&sheet, "D15"), "0.1");
    assert_eq!(cell_value(&sheet, "A16"), "草方块");
    assert_eq!(cell_value(&sheet, "B16"), "grass_block");
    assert_eq!(cell_value(&sheet, "C16"), "1");
    // Table headers use the header cell style (bold on a light fill); data
    // cells carry borders and right-aligned numbers.
    let a14 = &sheet[sheet.find(r#"r="A14""#).expect("A14")..];
    assert!(a14.starts_with(r#"r="A14" s="5""#), "A14 header style");
    let a2 = &sheet[sheet.find(r#"r="A2""#).expect("A2")..];
    assert!(a2.starts_with(r#"r="A2" s="3""#), "A2 plain text style");
    let c15 = &sheet[sheet.find(r#"r="C15""#).expect("C15")..];
    assert!(c15.starts_with(r#"r="C15" s="4""#), "C15 number style");
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

/// A stack in the legacy shape (`id` / `Count`).
fn item(id: &str, count: i8) -> NbtTag {
    let mut item = NbtCompound::new();
    item.insert("id", NbtTag::String(id.into()));
    item.insert("Count", NbtTag::Byte(count));
    NbtTag::Compound(item)
}

fn container_schematic(entities: Vec<((i32, i32, i32), &str, Vec<NbtTag>)>) -> UniversalSchematic {
    let mut model = schematic(
        &entities
            .iter()
            .map(|(position, id, _)| (position.0, position.1, position.2, *id))
            .collect::<Vec<_>>(),
    );
    for (position, id, items) in entities {
        let mut nbt = NbtCompound::new();
        nbt.insert("id", NbtTag::String(id.into()));
        nbt.insert("Items", NbtTag::List(NbtList::from(items)));
        let mut entity = BlockEntity::from_nbt(&nbt);
        entity.position = position;
        model.set_block_entity(
            BlockPosition {
                x: position.0,
                y: position.1,
                z: position.2,
            },
            entity,
        );
    }
    model
}

#[test]
fn container_contents_fill_the_container_section() {
    let model = container_schematic(vec![
        // Two chests: their contents are aggregated into one 箱子 group.
        (
            (0, 0, 0),
            "minecraft:chest",
            vec![item("minecraft:stone", 64), item("minecraft:air", 1)],
        ),
        (
            (1, 0, 0),
            "minecraft:chest",
            vec![item("minecraft:stone", 32), item("minecraft:diamond", 5)],
        ),
        // A dropper with fewer items sorts after the chest.
        ((2, 0, 0), "minecraft:dropper", vec![item("minecraft:arrow", 8)]),
        // A block entity that is not a container must not contribute.
        ((3, 0, 0), "minecraft:sign", vec![item("minecraft:oak_planks", 9)]),
    ]);
    let data = nucleation::formats::litematic::to_litematic(&model).unwrap();
    let export = export_materials_xlsx(&data, "容器", &[]).expect("export workbook");

    // The container blocks themselves stay part of the block material list
    // (two chests, one dropper, one sign — the implicit air is dropped).
    let sheet = workbook_sheet(&export.data);
    assert_eq!(export.material_count, 3);
    // stone, diamond, arrow — three distinct items across the containers.
    assert_eq!(export.container_item_count, 3);
    assert_eq!(cell_value(&sheet, "A10"), "投影材料种类");
    assert_eq!(cell_value(&sheet, "B10"), "3");
    assert_eq!(cell_value(&sheet, "A11"), "容器内材料种类");
    assert_eq!(cell_value(&sheet, "B11"), "3");

    // Container rows start at row 15, in columns F-J.
    assert_eq!(cell_value(&sheet, "F15"), "箱子");
    assert_eq!(cell_value(&sheet, "G15"), "石头");
    assert_eq!(cell_value(&sheet, "H15"), "stone");
    assert_eq!(cell_value(&sheet, "I15"), "96");
    assert_eq!(cell_value(&sheet, "J15"), "0.1");
    // The chest name merges over its two items.
    assert!(sheet.contains(r#"<mergeCell ref="F15:F16"/>"#));
    assert_eq!(cell_value(&sheet, "G16"), "钻石");
    assert_eq!(cell_value(&sheet, "I16"), "5");
    assert_eq!(cell_value(&sheet, "F17"), "投掷器");
    assert_eq!(cell_value(&sheet, "G17"), "箭");
    assert_eq!(cell_value(&sheet, "I17"), "8");
    // No fourth row: the sign was skipped.
    assert!(!has_cell(&sheet, "F18"), "container table ends after the dropper");
}

#[test]
fn nested_shulker_box_counts_as_a_single_item() {
    let mut shulker = NbtCompound::new();
    shulker.insert("id", NbtTag::String("minecraft:shulker_box".into()));
    shulker.insert("Count", NbtTag::Byte(1));
    let model = container_schematic(vec![(
        (0, 0, 0),
        "minecraft:hopper",
        vec![NbtTag::Compound(shulker)],
    )]);
    let data = nucleation::formats::litematic::to_litematic(&model).unwrap();
    let export = export_materials_xlsx(&data, "潜影盒", &[]).expect("export workbook");
    assert_eq!(export.container_item_count, 1);
    let sheet = workbook_sheet(&export.data);
    // The hopper is the only block material here: the shulker box only exists
    // as a stack inside it.
    assert_eq!(export.material_count, 1);
    assert_eq!(cell_value(&sheet, "A15"), "漏斗");
    assert_eq!(cell_value(&sheet, "F15"), "漏斗");
    assert_eq!(cell_value(&sheet, "G15"), "潜影盒");
    assert_eq!(cell_value(&sheet, "H15"), "shulker_box");
    assert_eq!(cell_value(&sheet, "I15"), "1");
}

#[test]
fn a_projection_without_containers_leaves_the_section_empty() {
    let data = nucleation::formats::litematic::to_litematic(&schematic(&[(
        0,
        0,
        0,
        "minecraft:stone",
    )]))
    .unwrap();
    let export = export_materials_xlsx(&data, "无容器", &[]).expect("export workbook");
    assert_eq!(export.container_item_count, 0);
    let sheet = workbook_sheet(&export.data);
    assert_eq!(cell_value(&sheet, "B11"), "0");
    // The container headers are still written, with no rows underneath.
    assert_eq!(cell_value(&sheet, "F14"), "容器名称");
    assert!(!has_cell(&sheet, "F15"), "no container rows");
}
