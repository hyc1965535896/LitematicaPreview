//! Container contents for the material list: the items stored inside the
//! schematic's chests, hoppers, droppers, shulker boxes and the like, read
//! from each region's block-entity NBT.
//!
//! One entry per container *type* (not per container), so "漏斗" lists the
//! items spread across every hopper of the projection.

use std::collections::{BTreeMap, HashMap};
use std::iter;

use nucleation::utils::container_spec::is_container;
use nucleation::utils::{NbtMap, NbtValue};
use nucleation::UniversalSchematic;

pub struct ContainerMaterials {
    /// Container id (with `minecraft:` prefix) and its aggregated item counts.
    pub types: Vec<(String, Vec<(String, i64)>)>,
}

impl ContainerMaterials {
    /// Number of distinct item ids across every container type.
    pub fn distinct_items(&self) -> usize {
        let mut items: Vec<&str> = self
            .types
            .iter()
            .flat_map(|(_, items)| items.iter().map(|(id, _)| id.as_str()))
            .collect();
        items.sort_unstable();
        items.dedup();
        items.len()
    }
}

pub(crate) fn collect(schematic: &UniversalSchematic) -> ContainerMaterials {
    let mut totals: BTreeMap<String, HashMap<String, i64>> = BTreeMap::new();
    let regions = iter::once(&schematic.default_region).chain(schematic.other_regions.values());
    for region in regions {
        for entity in region.block_entities.values() {
            let Some(items) = item_totals(&entity.nbt, &entity.id) else {
                continue;
            };
            let container = totals.entry(entity.id.clone()).or_default();
            for (item, count) in items {
                *container.entry(item).or_default() += count;
            }
        }
    }
    let mut types: Vec<(String, Vec<(String, i64)>, i64)> = totals
        .into_iter()
        .map(|(container, items)| {
            let total: i64 = items.values().sum();
            let mut items: Vec<(String, i64)> = items.into_iter().collect();
            items.sort_by(|left, right| right.1.cmp(&left.1).then_with(|| left.0.cmp(&right.0)));
            (container, items, total)
        })
        .collect();
    // Most-filled container first, then by id for a stable order.
    types.sort_by(|left, right| right.2.cmp(&left.2).then_with(|| left.0.cmp(&right.0)));
    ContainerMaterials {
        types: types
            .into_iter()
            .map(|(container, items, _)| (container, items))
            .collect(),
    }
}

/// Aggregated item stack counts for one block entity, or `None` when it holds
/// no usable inventory (signs, note blocks, an unknown `id`, or no items).
fn item_totals(nbt: &NbtMap, entity_id: &str) -> Option<HashMap<String, i64>> {
    let entity_id = entity_id.strip_prefix("minecraft:").unwrap_or(entity_id);
    if !is_container(entity_id) {
        return None;
    }
    let Some(NbtValue::List(items)) = nbt.get("Items") else {
        return None;
    };
    let mut totals: HashMap<String, i64> = HashMap::new();
    for item in items {
        let NbtValue::Compound(item) = item else {
            continue;
        };
        let Some((id, count)) = item_stack(item) else {
            continue;
        };
        *totals.entry(id).or_default() += count;
    }
    if totals.is_empty() {
        None
    } else {
        Some(totals)
    }
}

/// `(item id, count)` for one entry of a container's `Items` list. The count
/// moved from the `Count` byte (through 1.20.4) to the `count` int (1.20.5+);
/// a stack without either field counts once, as in vanilla.
fn item_stack(item: &NbtMap) -> Option<(String, i64)> {
    let id = match item.get("id")? {
        NbtValue::String(value) => value.clone(),
        // Older or hand-edited NBT stores the id as raw bytes.
        NbtValue::ByteArray(value) => String::from_utf8(
            value.iter().map(|byte| *byte as u8).collect::<Vec<u8>>(),
        )
        .ok()?,
        _ => return None,
    };
    if crate::materials_xlsx::is_air(&id) {
        return None;
    }
    let count = match item.get("count").or_else(|| item.get("Count")) {
        Some(NbtValue::Byte(value)) => i64::from(*value),
        Some(NbtValue::Short(value)) => i64::from(*value),
        Some(NbtValue::Int(value)) => i64::from(*value),
        Some(NbtValue::Long(value)) => *value,
        _ => 1,
    };
    Some((id, count.max(0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, count: i64) -> NbtValue {
        let mut map = NbtMap::new();
        map.insert("id".into(), NbtValue::String(id.into()));
        map.insert("count".into(), NbtValue::Int(count as i32));
        NbtValue::Compound(map)
    }

    fn entity(id: &str, items: Vec<NbtValue>) -> NbtMap {
        let mut map = NbtMap::new();
        map.insert("id".into(), NbtValue::String(id.into()));
        map.insert("Items".into(), NbtValue::List(items));
        map
    }

    #[test]
    fn aggregates_items_of_known_containers_only() {
        let chest = entity("minecraft:chest", vec![item("minecraft:stone", 64)]);
        assert_eq!(
            item_totals(&chest, "minecraft:chest"),
            Some(HashMap::from([("minecraft:stone".to_string(), 64)]))
        );
        // A sign is a block entity but not a container.
        assert_eq!(item_totals(&chest, "minecraft:sign"), None);
        // An empty container contributes nothing.
        assert_eq!(
            item_totals(&entity("minecraft:chest", vec![]), "minecraft:chest"),
            None
        );
    }

    #[test]
    fn reads_both_count_spellings_and_ignores_air() {
        let mut legacy = NbtMap::new();
        legacy.insert("id".into(), NbtValue::String("minecraft:redstone".into()));
        legacy.insert("Count".into(), NbtValue::Byte(16));
        assert_eq!(
            item_stack(&legacy),
            Some(("minecraft:redstone".to_string(), 16))
        );
        assert_eq!(
            item_stack(&NbtMap::new()),
            None,
            "a stack without an id is skipped"
        );
        let mut air = NbtMap::new();
        air.insert("id".into(), NbtValue::String("minecraft:air".into()));
        assert_eq!(item_stack(&air), None);
    }
}
