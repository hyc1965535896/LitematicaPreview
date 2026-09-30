import { readFileSync, writeFileSync } from "node:fs"
import { resolve } from "node:path"

const [, , specPath, baseArg] = process.argv
const base = resolve(baseArg)
const spec = JSON.parse(readFileSync(specPath, "utf8"))
const errors = []
const changes = new Map()
for (const { file, old: oldStr, new: newStr, count = 1 } of spec) {
  const path = resolve(base, file)
  if (!changes.has(path)) changes.set(path, readFileSync(path, "utf8"))
  let text = changes.get(path)
  const actual = text.split(oldStr).length - 1
  if (actual !== count) {
    errors.push(
      `${file}: expected ${count} occurrence(s) of ${JSON.stringify(oldStr.slice(0, 70))}..., found ${actual}`,
    )
    continue
  }
  text = text.split(oldStr).join(newStr)
  changes.set(path, text)
}
if (errors.length) {
  console.error(errors.join("\n"))
  process.exit(1)
}
for (const [path, text] of changes) writeFileSync(path, text)
console.log(`Applied ${spec.length} replacements across ${changes.size} files`)
