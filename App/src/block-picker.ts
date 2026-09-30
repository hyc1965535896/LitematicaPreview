// Pure helpers for the graphical block picker in the materials dialog.

import { BLOCK_NAMES_ZH_CN } from "./block-names.zh-cn"

export type BlockIconEntry = { name: string; icon: string }

export function blockZhName(id: string): string {
  return BLOCK_NAMES_ZH_CN[id] ?? ""
}

/** Matches a block id against the picker query: id substring or Chinese name. */
export function matchesBlockQuery(id: string, query: string): boolean {
  const needle = query.trim().toLowerCase()
  if (needle === "") return true
  if (id.includes(needle)) return true
  const name = BLOCK_NAMES_ZH_CN[id]
  return name !== undefined && name.toLowerCase().includes(needle)
}

/** Hover label the way the game shows it: "刷怪笼 minecraft:spawner". */
export function tooltipLabel(id: string): string {
  const name = BLOCK_NAMES_ZH_CN[id]
  return name === undefined ? id : `${name} ${id}`
}
