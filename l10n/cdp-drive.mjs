const list = await (await fetch("http://127.0.0.1:9223/json")).json()
const page = list.find((t) => t.type === "page")
const ws = new WebSocket(page.webSocketDebuggerUrl)
await new Promise((resolve, reject) => {
  ws.onopen = resolve
  ws.onerror = reject
})
let seq = 0
const pending = new Map()
ws.onmessage = (event) => {
  const data = JSON.parse(event.data)
  if (data.id && pending.has(data.id)) {
    pending.get(data.id)(data)
    pending.delete(data.id)
  }
}
const call = (method, params = {}) =>
  new Promise((resolve, reject) => {
    const id = ++seq
    pending.set(id, (data) => (data.error ? reject(new Error(JSON.stringify(data.error))) : resolve(data.result)))
    ws.send(JSON.stringify({ id, method, params }))
  })
const evaluate = async (expression) => {
  const result = await call("Runtime.evaluate", { expression, awaitPromise: true, returnByValue: true })
  if (result.exceptionDetails) throw new Error(JSON.stringify(result.exceptionDetails).slice(0, 500))
  return result.result?.value
}
const sleep = (ms) => new Promise((resolve) => setTimeout(resolve, ms))

const state = process.argv[2] ?? "open"
if (state === "open") {
  // Click the materials toolbar button.
  const clicked = await evaluate(`(() => {
    const button = document.querySelector('button[aria-label="材料清单与导出"]')
    if (!button) return "button-missing"
    button.click()
    return "clicked"
  })()`)
  console.log("open:", clicked)
  await sleep(800)
  const text = await evaluate(`document.body.innerText`)
  console.log("--- dialog text ---")
  console.log(text.slice(0, 2500))
} else if (state === "replace") {
  const to = process.argv[3] ?? "minecraft:quartz_block"
  // Click the first row's 替换 button.
  const started = await evaluate(`(() => {
    const rows = [...document.querySelectorAll('div')].filter((d) => d.querySelector('button') && /替换$/.test(d.querySelector('button').textContent.trim()))
    const button = [...document.querySelectorAll('button')].find((b) => b.textContent.trim() === '替换')
    if (!button) return "replace-button-missing"
    button.click()
    return "started"
  })()`)
  console.log("replace click:", started)
  await sleep(400)
  const typed = await evaluate(`(() => {
    const input = document.querySelector('input[list="material-block-catalog"]')
    if (!input) return "input-missing"
    const setter = Object.getOwnPropertyDescriptor(window.HTMLInputElement.prototype, "value").set
    setter.call(input, ${JSON.stringify(to)})
    input.dispatchEvent(new Event("input", { bubbles: true }))
    return "typed"
  })()`)
  console.log("type:", typed)
  await sleep(300)
  const confirmed = await evaluate(`(() => {
    const button = [...document.querySelectorAll('button')].find((b) => b.textContent.trim() === '替换并重新渲染')
    if (!button) return "confirm-missing"
    if (button.disabled) return "confirm-disabled"
    button.click()
    return "confirmed"
  })()`)
  console.log("confirm:", confirmed)
} else if (state === "check") {
  const text = await evaluate(`document.body.innerText`)
  const materials = await evaluate(`window.__lpMaterials ? window.__lpMaterials : "no-hook"`)
  console.log("--- page text ---")
  console.log(text.slice(0, 3000))
}
ws.close()
