//! Palette-level material replacement ported from the SchematicPreview mod:
//! every block state whose block name matches a rule is rewritten to the
//! replacement block, copying the original state's properties verbatim.
//! Loaders skip properties the replacement block does not define, which
//! matches the mod's shared-property copying without a block registry.

use nucleation::{BlockState, Region, UniversalSchematic};

/// Maximum number of active replacement rules per load or export.
pub const MAX_REPLACEMENTS: usize = 64;

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BlockReplacement {
    pub from: String,
    pub to: String,
}

fn valid_block_name(name: &str) -> bool {
    let Some((namespace, block)) = name.split_once(':') else {
        return false;
    };
    if namespace.is_empty() || block.is_empty() || block.contains(':') {
        return false;
    }
    name.chars()
        .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '_' | '-' | '.' | '/' | ':'))
}

pub fn validate_replacements(replacements: &[BlockReplacement]) -> Result<(), String> {
    if replacements.len() > MAX_REPLACEMENTS {
        return Err(format!("材料替换规则不能超过 {} 条。", MAX_REPLACEMENTS));
    }
    for replacement in replacements {
        if !valid_block_name(&replacement.from) {
            return Err(format!("无效的方块名称：{}", replacement.from));
        }
        if !valid_block_name(&replacement.to) {
            return Err(format!("无效的方块名称：{}", replacement.to));
        }
        if replacement.from == replacement.to {
            return Err("替换前后的方块名称相同。".into());
        }
    }
    Ok(())
}

/// Rewrite every region so that block states whose name equals a rule's
/// `from` become the rule's `to` block with the original properties copied.
/// Returns the number of replaced block positions.
pub fn apply_replacements(
    schematic: &mut UniversalSchematic,
    replacements: &[BlockReplacement],
) -> Result<i64, String> {
    validate_replacements(replacements)?;
    if replacements.is_empty() {
        return Ok(0);
    }
    let mut replaced = 0i64;
    let default_region_name = schematic.default_region_name.clone();
    let mut names: Vec<String> = schematic.other_regions.keys().cloned().collect();
    names.sort_unstable();
    // Process the default region first for deterministic order, then the
    // remaining regions by name.
    for name in std::iter::once(default_region_name).chain(names) {
        let region = if schematic.default_region.name == name {
            &mut schematic.default_region
        } else {
            schematic
                .other_regions
                .get_mut(&name)
                .ok_or_else(|| format!("投影区域 {name} 不可用。"))?
        };
        replaced += replace_in_region(region, replacements)?;
    }
    Ok(replaced)
}

fn replace_in_region(
    region: &mut Region,
    replacements: &[BlockReplacement],
) -> Result<i64, String> {
    let palette = region.get_palette();
    let targets: Vec<Option<BlockState>> = palette
        .iter()
        .map(|state| {
            replacements
                .iter()
                .find(|rule| rule.from == state.name)
                .map(|rule| {
                    BlockState::new(rule.to.clone()).with_properties(state.properties.clone())
                })
        })
        .collect();
    if targets.iter().all(Option::is_none) {
        return Ok(0);
    }
    let mut replaced = 0i64;
    for index in 0..region.blocks.len() {
        let palette_index = region.blocks[index];
        let Some(new_state) = targets.get(palette_index).and_then(Option::as_ref) else {
            continue;
        };
        let (x, y, z) = region.index_to_coords(index);
        if region.set_block(x, y, z, new_state) {
            replaced += 1;
        }
    }
    Ok(replaced)
}
