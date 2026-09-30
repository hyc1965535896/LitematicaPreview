import assert from "node:assert/strict"
import { test } from "vite-plus/test"
import { blockZhName, matchesBlockQuery, tooltipLabel } from "./block-picker.ts"

test("the picker query matches ids and Chinese names", () => {
  assert.equal(matchesBlockQuery("minecraft:stone", ""), true)
  assert.equal(matchesBlockQuery("minecraft:stone", "stone"), true)
  assert.equal(matchesBlockQuery("minecraft:cobblestone", "stone"), true)
  assert.equal(matchesBlockQuery("minecraft:stone", "MINECRAFT:ST"), true)
  assert.equal(matchesBlockQuery("minecraft:stone", "石头"), true)
  assert.equal(matchesBlockQuery("minecraft:cobblestone", "圆石"), true)
  assert.equal(matchesBlockQuery("minecraft:stone", "圆石"), false)
  assert.equal(matchesBlockQuery("minecraft:spawner", "刷怪笼"), true)
  assert.equal(matchesBlockQuery("minecraft:stone", " 刷怪笼 "), false)
})

test("tooltips pair the Chinese name with the id", () => {
  assert.equal(tooltipLabel("minecraft:spawner"), "刷怪笼 minecraft:spawner")
  assert.equal(tooltipLabel("minecraft:not_in_pack"), "minecraft:not_in_pack")
  assert.equal(blockZhName("minecraft:grass_block"), "草方块")
})
