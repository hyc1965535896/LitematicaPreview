import { useCallback, useEffect, useMemo, useRef, useState } from "react"
import {
  Badge,
  Button,
  Dialog,
  DialogActions,
  DialogBody,
  DialogContent,
  DialogSurface,
  DialogTitle,
  Field,
  FluentProvider,
  Menu,
  MenuDivider,
  MenuGroup,
  MenuGroupHeader,
  MenuItem,
  MenuItemRadio,
  MenuList,
  MenuPopover,
  MenuTrigger,
  MessageBar,
  MessageBarActions,
  MessageBarBody,
  MessageBarTitle,
  ProgressBar,
  Slider,
  SpinButton,
  Switch,
  ToggleButton,
  Tooltip,
  webDarkTheme,
  webLightTheme,
} from "@fluentui/react-components"
import {
  Add20Regular,
  ArrowExpand20Regular,
  ArrowRight20Regular,
  Cube24Regular,
  Dismiss20Regular,
  Document20Regular,
  FolderOpen20Regular,
  Grid20Regular,
  Home20Regular,
  Info20Regular,
  Keyboard20Regular,
  List20Regular,
  MoreHorizontal20Regular,
  Settings20Regular,
  ShieldCheckmark20Regular,
  Subtract20Regular,
} from "@fluentui/react-icons"
import { listen } from "@tauri-apps/api/event"
import { invoke } from "@tauri-apps/api/core"
import { getCurrentWebview } from "@tauri-apps/api/webview"
import { getCurrentWindow } from "@tauri-apps/api/window"
import icon from "../../Assets/app-ui.png"
import { SchematicRenderer, type PreviewMetadata, type PreviewStreamEvent } from "./renderer"
import type { PreviewMaterial } from "./preview-stream"
import type { PreviewReadRange } from "./upload-layout"
import { blockZhName, matchesBlockQuery, tooltipLabel, type BlockIconEntry } from "./block-picker"

type BlockReplacement = { from: string; to: string }
type ExportOutcome = { destination: string; replaced: number; blockCount: number }

// Icons render once per app run in the Rust host; keep them across dialog opens.
let cachedBlockIcons: BlockIconEntry[] | null = null

type Bootstrap = {
  extensions: string[]
  demos: { name: string; path: string; extension: string }[]
  initialPath: string | null
  version: string
  requestId: number
  maxWorkerThreads: number
}
type ThemePreference = "system" | "light" | "dark"
type PreviewSettings = {
  memoryLimitEnabled: boolean
  memoryLimitMB: number
  chunkingEnabled: boolean
  chunkSize: number
  multithreadingEnabled: boolean
  threadCount: number
  conservativeMemoryScheduling: boolean
}
type Loading = {
  path: string
  requestId: number
  phase: "decode" | "mesh" | "upload" | "stream"
  completed: number
  total: number
  uploadedBytes: number
}
type MeshProgress = { requestId: number; phase: "mesh"; completed: number; total: number }
type Loaded = { path: string; metadata: PreviewMetadata; seconds: number }
type Notice = { intent: "error" | "success" | "info"; message: string }

const appName = "Litematica Preview"
const numbers = new Intl.NumberFormat()
const dimensions = new Intl.NumberFormat(undefined, {
  maximumFractionDigits: 2,
})
const megabytes = new Intl.NumberFormat(undefined, { maximumFractionDigits: 1 })
const formatMB = (bytes: number) => `${megabytes.format(bytes / (1024 * 1024))} MB`
const chunkSizes = [16, 32, 64, 128, 256]
const defaultPreviewSettings: PreviewSettings = {
  memoryLimitEnabled: false,
  memoryLimitMB: 2048,
  chunkingEnabled: true,
  chunkSize: 64,
  multithreadingEnabled: true,
  threadCount: 4,
  conservativeMemoryScheduling: false,
}
const fileName = (path: string) => path.split(/[\\/]/).pop() || path
const errorMessage = (error: unknown) => (error instanceof Error ? error.message : String(error))
const isTheme = (value: unknown): value is ThemePreference =>
  value === "system" || value === "light" || value === "dark"
const fileExtension = (path: string) => {
  const name = fileName(path)
  const dot = name.lastIndexOf(".")
  return dot > 0 ? name.slice(dot + 1).toLowerCase() : ""
}
const exportFormats = [
  { value: "litematic", label: "Litematica（.litematic）" },
  { value: "schem", label: "Sponge（.schem）" },
  { value: "nbt", label: "结构方块（.nbt）" },
  { value: "snbt", label: "结构 SNBT（.snbt）" },
  { value: "mcstructure", label: "基岩版结构（.mcstructure）" },
  { value: "nusn", label: "Nucleation 快照（.nusn）" },
]
const materialStateLabel = (material: PreviewMaterial) =>
  material.properties.length > 0
    ? `${material.name} [${material.properties
        .map(([key, value]) => `${key}=${value}`)
        .join(",")}]`
    : material.name

function savedTheme(): ThemePreference {
  try {
    const value = localStorage.getItem("litematica-preview-theme")
    return isTheme(value) ? value : "system"
  } catch {
    return "system"
  }
}

export function savedPreviewSettings(): PreviewSettings {
  try {
    const value: unknown = JSON.parse(localStorage.getItem("litematica-preview-settings") || "null")
    if (value === null || typeof value !== "object" || Array.isArray(value))
      return defaultPreviewSettings
    const settings = value as Record<string, unknown>
    return {
      memoryLimitEnabled:
        typeof settings.memoryLimitEnabled === "boolean"
          ? settings.memoryLimitEnabled
          : defaultPreviewSettings.memoryLimitEnabled,
      memoryLimitMB:
        typeof settings.memoryLimitMB === "number" &&
        Number.isInteger(settings.memoryLimitMB) &&
        settings.memoryLimitMB >= 2048 &&
        settings.memoryLimitMB <= 8192
          ? settings.memoryLimitMB
          : typeof settings.memoryLimitGiB === "number" &&
              Number.isInteger(settings.memoryLimitGiB) &&
              settings.memoryLimitGiB >= 2 &&
              settings.memoryLimitGiB <= 8
            ? settings.memoryLimitGiB * 1024
            : defaultPreviewSettings.memoryLimitMB,
      chunkingEnabled:
        typeof settings.chunkingEnabled === "boolean"
          ? settings.chunkingEnabled
          : defaultPreviewSettings.chunkingEnabled,
      chunkSize:
        typeof settings.chunkSize === "number" && chunkSizes.includes(settings.chunkSize)
          ? settings.chunkSize
          : defaultPreviewSettings.chunkSize,
      multithreadingEnabled:
        settings.multithreadingEnabled === true && settings.chunkingEnabled !== false,
      threadCount:
        typeof settings.threadCount === "number" &&
        Number.isInteger(settings.threadCount) &&
        settings.threadCount >= 2 &&
        settings.threadCount <= 8
          ? settings.threadCount
          : defaultPreviewSettings.threadCount,
      conservativeMemoryScheduling:
        typeof settings.conservativeMemoryScheduling === "boolean"
          ? settings.conservativeMemoryScheduling
          : typeof settings.speedFirst === "boolean"
            ? !settings.speedFirst
            : defaultPreviewSettings.conservativeMemoryScheduling,
    }
  } catch {
    return defaultPreviewSettings
  }
}

export default function App({ initialError }: { initialError?: string }) {
  const [bootstrap, setBootstrap] = useState<Bootstrap | null>(null)
  const [loading, setLoading] = useState<Loading | null>(null)
  const [previewMemory, setPreviewMemory] = useState<number | null>(null)
  const [loaded, setLoaded] = useState<Loaded | null>(null)
  const [notice, setNotice] = useState<Notice | null>(null)
  const [dragging, setDragging] = useState(false)
  const [grid, setGrid] = useState(true)
  const [theme, setTheme] = useState<ThemePreference>(savedTheme)
  const [previewSettings, setPreviewSettings] = useState<PreviewSettings>(savedPreviewSettings)
  const [systemDark, setSystemDark] = useState(
    () => matchMedia("(prefers-color-scheme: dark)").matches,
  )
  const [dialog, setDialog] = useState<"controls" | "about" | "settings" | "error" | "materials">(
    initialError ? "error" : "controls",
  )
  const [dialogOpen, setDialogOpen] = useState(Boolean(initialError))
  const [errorDetails, setErrorDetails] = useState(initialError || "")
  const [actionBusy, setActionBusy] = useState(false)
  const [choosing, setChoosing] = useState(false)
  const [replacements, setReplacements] = useState<{ path: string; rules: BlockReplacement[] }>({
    path: "",
    rules: [],
  })
  const [replacingFrom, setReplacingFrom] = useState<string | null>(null)
  const [replacementTo, setReplacementTo] = useState("")
  const [blockIcons, setBlockIcons] = useState<BlockIconEntry[] | null>(null)
  const [exportFormat, setExportFormat] = useState<string>("litematic")
  const [exporting, setExporting] = useState(false)
  const canvasRef = useRef<HTMLCanvasElement>(null)
  const rendererRef = useRef<SchematicRenderer | null>(null)
  const bootstrapRef = useRef<Bootstrap | null>(null)
  const gridRef = useRef(true)
  const previewSettingsRef = useRef(previewSettings)
  const mounted = useRef(false)
  const generation = useRef(0)
  const initialPathConsumed = useRef(Boolean(initialError))
  const choosingRef = useRef(false)
  const actionBusyRef = useRef(false)
  const titleQueue = useRef<Promise<void>>(Promise.resolve())
  const previewReadQueue = useRef<Promise<void>>(Promise.resolve())
  const replacementsRef = useRef(replacements)

  useEffect(() => {
    replacementsRef.current = replacements
  }, [replacements])

  const updatePreviewSettings = useCallback((patch: Partial<PreviewSettings>) => {
    const settings = { ...previewSettingsRef.current, ...patch }
    previewSettingsRef.current = settings
    setPreviewSettings(settings)
  }, [])

  const isCurrent = useCallback((id: number) => mounted.current && generation.current === id, [])

  // Serialize title changes so a delayed native call cannot restore an old filename.
  const updateTitle = useCallback(
    (path: string | null, id: number) => {
      titleQueue.current = titleQueue.current.then(async () => {
        if (!isCurrent(id)) return
        try {
          await getCurrentWindow().setTitle(path ? `${fileName(path)} — ${appName}` : appName)
        } catch (error) {
          if (isCurrent(id))
            setNotice({
              intent: "error",
              message: `无法更新窗口标题：${errorMessage(error)}`,
            })
        }
      })
    },
    [isCurrent],
  )

  const cancelNative = useCallback(
    (id: number) => {
      void invoke("cancel_load", { requestId: id }).catch((error: unknown) => {
        if (isCurrent(id))
          setNotice({
            intent: "error",
            message: `无法取消加载：${errorMessage(error)}`,
          })
      })
    },
    [isCurrent],
  )

  const clearView = useCallback(() => {
    rendererRef.current?.clear()
    setLoaded(null)
    setLoading(null)
    setPreviewMemory(null)
    setDragging(false)
  }, [])

  const home = useCallback(() => {
    const id = ++generation.current
    cancelNative(id)
    clearView()
    setNotice(null)
    updateTitle(null, id)
  }, [cancelNative, clearView, updateTitle])

  const graphicsFailed = useCallback(
    (message: string) => {
      if (!mounted.current) return
      const id = ++generation.current
      cancelNative(id)
      const renderer = rendererRef.current
      rendererRef.current = null
      renderer?.dispose()
      setLoaded(null)
      setLoading(null)
      setPreviewMemory(null)
      setNotice({
        intent: "error",
        message: `无法渲染预览。${message}`,
      })
      updateTitle(null, id)
    },
    [cancelNative, updateTitle],
  )

  useEffect(() => {
    if (notice?.intent !== "error") return
    const id = ++generation.current
    const renderer = rendererRef.current
    rendererRef.current = null
    setLoaded(null)
    setLoading(null)
    setPreviewMemory(null)
    setDragging(false)
    setNotice(null)
    setErrorDetails(notice.message)
    setDialog("error")
    setDialogOpen(true)
    // Recovery failures are included in the same dialog, never recursively reported.
    const recoveryFailed = (error: unknown) => {
      if (isCurrent(id)) setErrorDetails((text) => `${text}\n\n恢复时出错：${errorMessage(error)}`)
    }
    try {
      renderer?.dispose()
    } catch (error) {
      recoveryFailed(error)
    }
    void invoke("cancel_load", { requestId: id }).catch(recoveryFailed)
    void getCurrentWindow().setTitle(appName).catch(recoveryFailed)
  }, [notice, isCurrent])

  useEffect(() => {
    const onError = (event: ErrorEvent) => {
      event.preventDefault()
      setNotice({ intent: "error", message: errorMessage(event.error || event.message) })
    }
    const onRejection = (event: PromiseRejectionEvent) => {
      event.preventDefault()
      setNotice({ intent: "error", message: errorMessage(event.reason) })
    }
    window.addEventListener("error", onError)
    window.addEventListener("unhandledrejection", onRejection)
    return () => {
      window.removeEventListener("error", onError)
      window.removeEventListener("unhandledrejection", onRejection)
    }
  }, [])

  const loadPath = useCallback(
    async (path: string, nextRules?: BlockReplacement[]) => {
      if (!mounted.current) return
      // Read current settings without rebuilding startup and drag-and-drop subscriptions.
      // This request keeps its own snapshot even if settings change during decoding.
      const settings = previewSettingsRef.current
      const speedFirst = settings.multithreadingEnabled && !settings.conservativeMemoryScheduling
      const rules = nextRules ?? (replacementsRef.current.path === path ? replacementsRef.current.rules : [])
      const options = {
        memoryLimitMB: settings.memoryLimitEnabled ? settings.memoryLimitMB : null,
        chunkSize: settings.chunkingEnabled ? settings.chunkSize : null,
        threadCount: settings.multithreadingEnabled ? settings.threadCount : null,
        speedFirst,
        replacements: rules,
      }
      const id = ++generation.current
      clearView()
      setNotice(null)
      updateTitle(null, id)
      const supported = bootstrapRef.current?.extensions.some((extension) =>
        path.toLowerCase().endsWith(extension.toLowerCase()),
      )
      if (!supported) {
        cancelNative(id)
        setNotice({
          intent: "error",
          message: `“${fileName(path)}”不是受支持的投影文件。请从下面列出的格式中选择。`,
        })
        return
      }
      setLoading({ path, requestId: id, phase: "decode", completed: 0, total: 0, uploadedBytes: 0 })
      const started = performance.now()
      let unlistenProgress: (() => void) | undefined
      try {
        unlistenProgress = await listen<MeshProgress>("preview-progress", ({ payload }) => {
          if (!isCurrent(id) || payload.requestId !== id || payload.phase !== "mesh") return
          if (!Number.isSafeInteger(payload.total) || payload.total <= 0) return
          if (
            !Number.isSafeInteger(payload.completed) ||
            payload.completed < 0 ||
            payload.completed > payload.total
          )
            return
          setLoading((previous) =>
            previous?.requestId === id && previous.phase !== "upload"
              ? {
                  ...previous,
                  phase: options.threadCount === null ? "mesh" : "stream",
                  completed: payload.completed,
                  total: payload.total,
                }
              : previous,
          )
        })
        if (!isCurrent(id)) return
        // Serial mode still completes native generation before allocating GPU state.
        const descriptor =
          options.threadCount === null
            ? await invoke<PreviewMetadata>("load_preview", { path, requestId: id, options })
            : null
        if (!isCurrent(id)) return
        let renderer = rendererRef.current
        if (!renderer) {
          if (!canvasRef.current) throw new Error("预览画布不可用。")
          renderer = new SchematicRenderer(canvasRef.current, graphicsFailed)
          if (!isCurrent(id)) {
            renderer.dispose()
            return
          }
          rendererRef.current = renderer
          renderer.setGrid(gridRef.current)
        }
        const readRanges = (batchId: number | null, ranges: readonly PreviewReadRange[]) => {
          if (!isCurrent(id)) return Promise.reject(new Error("Cancelled"))
          return invoke<ArrayBuffer>("read_preview", {
            requestId: id,
            batchId,
            ranges: ranges.map(({ bufferId, offset, length }) => ({ bufferId, offset, length })),
          })
        }
        let lastUploadUpdate = 0
        let metadata: PreviewMetadata
        if (options.threadCount !== null) {
          // The producer starts before the first pull and runs ahead only within its bounded queue.
          await invoke("start_preview", { path, requestId: id, options })
          if (!isCurrent(id)) return
          metadata = await renderer.loadStream(
            async (previousBatchId) => {
              if (!isCurrent(id)) throw new Error("Cancelled")
              const event = await invoke<PreviewStreamEvent>("next_preview", {
                requestId: id,
                previousBatchId,
              })
              if (!isCurrent(id)) throw new Error("Cancelled")
              return event
            },
            readRanges,
            () => isCurrent(id),
            (uploadedBytes) => {
              if (!isCurrent(id)) return
              const now = performance.now()
              if (now - lastUploadUpdate < 150) return
              lastUploadUpdate = now
              setLoading((previous) =>
                previous?.requestId === id
                  ? { ...previous, phase: "stream", uploadedBytes }
                  : previous,
              )
            },
            options.threadCount,
            options.speedFirst,
          )
        } else {
          if (!descriptor) throw new Error("预览元数据不可用。")
          setLoading({
            path,
            requestId: id,
            phase: "upload",
            completed: 0,
            total: descriptor.byteLength,
            uploadedBytes: 0,
          })
          metadata = await renderer.load(
            descriptor,
            (ranges) => {
              const read = previewReadQueue.current.then(() => readRanges(null, ranges))
              previewReadQueue.current = read.then(
                () => {},
                () => {},
              )
              return read
            },
            () => isCurrent(id),
            (completed, total) => {
              if (!isCurrent(id)) return
              const now = performance.now()
              if (completed !== total && now - lastUploadUpdate < 150) return
              lastUploadUpdate = now
              setLoading((previous) =>
                previous?.requestId === id && previous.phase === "upload"
                  ? { ...previous, completed, total, uploadedBytes: completed }
                  : previous,
              )
            },
            null,
            false,
          )
        }
        if (!isCurrent(id)) return
        setLoaded({
          path,
          metadata,
          seconds: (performance.now() - started) / 1000,
        })
        setReplacements({ path, rules })
        setLoading(null)
        setPreviewMemory(null)
        updateTitle(path, id)
        if (metadata.replaced && metadata.replaced > 0) {
          setNotice({
            intent: "success",
            message: `已应用材料替换：共替换了 ${numbers.format(metadata.replaced)} 个方块。`,
          })
        }
        canvasRef.current?.focus({ preventScroll: true })
      } catch (error) {
        if (!isCurrent(id)) return
        rendererRef.current?.clear()
        setLoading(null)
        setPreviewMemory(null)
        if (errorMessage(error) !== "Cancelled")
          setNotice({ intent: "error", message: `${path}\n\n${errorMessage(error)}` })
      } finally {
        unlistenProgress?.()
        try {
          await invoke("release_preview", { requestId: id })
        } catch (error) {
          if (isCurrent(id))
            setNotice({
              intent: "error",
              message: `无法完成预览加载：${errorMessage(error)}`,
            })
        }
      }
    },
    [cancelNative, clearView, graphicsFailed, isCurrent, updateTitle],
  )

  const chooseFile = useCallback(async () => {
    if (!mounted.current || !bootstrapRef.current || choosingRef.current) return
    choosingRef.current = true
    setChoosing(true)
    const id = generation.current
    // Dismissing the picker must leave the current preview or load intact.
    try {
      const path = await invoke<string | null>("choose_file")
      if (path && isCurrent(id)) void loadPath(path)
    } catch (error) {
      if (isCurrent(id)) setNotice({ intent: "error", message: errorMessage(error) })
    } finally {
      choosingRef.current = false
      if (mounted.current) setChoosing(false)
    }
  }, [isCurrent, loadPath])

  const nativeAction = useCallback(
    async (command: "register_associations" | "unregister_associations" | "show_licenses") => {
      if (actionBusyRef.current) return
      actionBusyRef.current = true
      setActionBusy(true)
      const id = generation.current
      try {
        await invoke(command)
        if (isCurrent(id)) {
          setNotice({
            intent: "success",
            message:
              command === "register_associations"
                ? "文件关联已注册。请在 Windows 设置中为投影文件选择 Litematica Preview。"
                : command === "unregister_associations"
                  ? "已移除这份 Litematica Preview 的文件关联。"
                  : "已打开随附的许可证文件夹。",
          })
        }
      } catch (error) {
        if (isCurrent(id)) setNotice({ intent: "error", message: errorMessage(error) })
      } finally {
        actionBusyRef.current = false
        if (mounted.current) setActionBusy(false)
      }
    },
    [isCurrent],
  )

  const ensureBlockIcons = useCallback(() => {
    if (blockIcons !== null) return
    if (cachedBlockIcons !== null) {
      setBlockIcons(cachedBlockIcons)
      return
    }
    void invoke<BlockIconEntry[]>("block_icons")
      .then((icons) => {
        cachedBlockIcons = icons
        setBlockIcons(icons)
      })
      .catch((error: unknown) => {
        setNotice({
          intent: "error",
          message: `无法生成方块图标：${errorMessage(error)}`,
        })
      })
  }, [blockIcons])

  const beginReplacement = useCallback(
    (from: string) => {
      ensureBlockIcons()
      setReplacingFrom(from)
      setReplacementTo("")
    },
    [ensureBlockIcons],
  )

  const confirmReplacement = useCallback(() => {
    const from = replacingFrom
    const to = replacementTo.trim().toLowerCase()
    if (!from || !to || !loaded) return
    if (from === to) {
      setNotice({ intent: "info", message: "替换前后的方块名称相同，无需替换。" })
      setReplacingFrom(null)
      return
    }
    const previous = replacementsRef.current.path === loaded.path ? replacementsRef.current.rules : []
    const rules = [...previous.filter((rule) => rule.from !== from), { from, to }]
    setReplacingFrom(null)
    setReplacementTo("")
    void loadPath(loaded.path, rules)
  }, [loadPath, loaded, replacementTo, replacingFrom])

  const iconsByName = useMemo(() => {
    const map = new Map<string, string>()
    for (const entry of blockIcons ?? []) map.set(entry.name, entry.icon)
    return map
  }, [blockIcons])

  const removeReplacement = useCallback(
    (from: string) => {
      if (!loaded) return
      const previous = replacementsRef.current.path === loaded.path ? replacementsRef.current.rules : []
      const rules = previous.filter((rule) => rule.from !== from)
      void loadPath(loaded.path, rules)
    },
    [loadPath, loaded],
  )

  const runExport = useCallback(async () => {
    if (!loaded || exporting) return
    setExporting(true)
    try {
      const rules = replacementsRef.current.path === loaded.path ? replacementsRef.current.rules : []
      const outcome = await invoke<ExportOutcome>("export_schematic", {
        path: loaded.path,
        format: exportFormat,
        replacements: rules,
      })
      setNotice({
        intent: "success",
        message: `已导出 ${fileName(outcome.destination)}（${numbers.format(outcome.blockCount)} 个方块，替换了 ${numbers.format(outcome.replaced)} 个）。`,
      })
    } catch (error) {
      if (errorMessage(error) !== "Cancelled")
        setNotice({ intent: "error", message: `导出失败：${errorMessage(error)}` })
    } finally {
      setExporting(false)
    }
  }, [exportFormat, exporting, loaded])

  useEffect(() => {
    mounted.current = true
    // Keep restoration opted in even if a lost context makes the renderer
    // dispose itself before the browser dispatches webglcontextlost.
    const canvas = canvasRef.current
    const allowContextRestore = (event: Event) => event.preventDefault()
    canvas?.addEventListener("webglcontextlost", allowContextRestore)
    let active = true
    let unlisten: (() => void) | undefined
    const initialGeneration = generation.current
    void invoke<Bootstrap>("bootstrap")
      .then((result) => {
        if (!active) return
        const openInitialPath = isCurrent(initialGeneration)
        generation.current = Math.max(generation.current, result.requestId)
        bootstrapRef.current = result
        setBootstrap(result)
        const settings = previewSettingsRef.current
        updatePreviewSettings({
          multithreadingEnabled:
            settings.multithreadingEnabled &&
            settings.chunkingEnabled &&
            result.maxWorkerThreads >= 2,
          threadCount: Math.max(2, Math.min(settings.threadCount, result.maxWorkerThreads)),
        })
        if (!initialPathConsumed.current) {
          initialPathConsumed.current = true
          if (result.initialPath && openInitialPath) void loadPath(result.initialPath)
        }
      })
      .catch((error: unknown) => {
        if (active)
          setNotice({
            intent: "error",
            message: `无法初始化应用：${errorMessage(error)}`,
          })
      })
    void getCurrentWebview()
      .onDragDropEvent(({ payload }) => {
        if (!active) return
        if (payload.type === "leave") {
          setDragging(false)
        } else if (payload.type === "enter" || payload.type === "over") {
          setDragging(true)
        } else if (payload.type === "drop") {
          setDragging(false)
          const extensions = bootstrapRef.current?.extensions
          if (!extensions) {
            setNotice({
              intent: "info",
              message:
                "应用仍在启动，请稍候重新拖入文件。",
            })
            return
          }
          const path = payload.paths.find((candidate) =>
            extensions.some((extension) =>
              candidate.toLowerCase().endsWith(extension.toLowerCase()),
            ),
          )
          if (path) void loadPath(path)
          else
            setNotice({
              intent: "error",
              message: `未拖入受支持的投影文件。支持的格式：${extensions.join("、")}。`,
            })
        }
      })
      .then((dispose) => {
        if (active) unlisten = dispose
        else dispose()
      })
      .catch((error: unknown) => {
        if (active)
          setNotice({
            intent: "error",
            message: `拖放功能不可用，你仍然可以使用“打开”按钮。${errorMessage(error)}`,
          })
      })
    return () => {
      active = false
      mounted.current = false
      const id = ++generation.current
      cancelNative(id)
      unlisten?.()
      rendererRef.current?.dispose()
      rendererRef.current = null
      canvas?.removeEventListener("webglcontextlost", allowContextRestore)
    }
  }, [cancelNative, isCurrent, loadPath, updatePreviewSettings])

  useEffect(() => {
    if (!loading) return
    const id = loading.requestId
    let active = true
    let inFlight = false
    const sample = async () => {
      if (inFlight) return
      inFlight = true
      try {
        const memory = await invoke<number | null>("preview_memory", { requestId: id })
        if (active && isCurrent(id)) setPreviewMemory(memory)
      } catch {
        if (active && isCurrent(id)) setPreviewMemory(null)
      } finally {
        inFlight = false
      }
    }
    void sample()
    const timer = window.setInterval(() => void sample(), 500)
    return () => {
      active = false
      window.clearInterval(timer)
    }
  }, [loading?.requestId, isCurrent])

  useEffect(() => {
    const media = matchMedia("(prefers-color-scheme: dark)")
    const change = () => setSystemDark(media.matches)
    media.addEventListener("change", change)
    return () => media.removeEventListener("change", change)
  }, [])

  const dark = theme === "dark" || (theme === "system" && systemDark)
  // Storage failures do not change in-memory preferences.
  useEffect(() => {
    document.documentElement.classList.toggle("dark", dark)
    document.documentElement.style.colorScheme = dark ? "dark" : "light"
    try {
      localStorage.setItem("litematica-preview-theme", theme)
    } catch {
      /* Theme still works when storage is disabled. */
    }
  }, [dark, theme])

  useEffect(() => {
    try {
      localStorage.setItem("litematica-preview-settings", JSON.stringify(previewSettings))
    } catch {
      /* 预览设置在存储被禁用时仍然生效。 */
    }
  }, [previewSettings])

  useEffect(() => {
    if (!loaded) return
    const extension = fileExtension(loaded.path)
    setExportFormat(
      exportFormats.some((format) => format.value === extension) ? extension : "litematic",
    )
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [loaded?.path])

  useEffect(() => {
    const onKeyDown = (event: KeyboardEvent) => {
      if (event.defaultPrevented || event.altKey) return
      if ((event.ctrlKey || event.metaKey) && event.key.toLowerCase() === "o") {
        event.preventDefault()
        if (!dialogOpen) void chooseFile()
        return
      }
      if (dialogOpen || event.ctrlKey || event.metaKey) return
      const target = event.target
      if (
        target instanceof HTMLElement &&
        (target.isContentEditable ||
          target.closest('input, textarea, select, [role="menu"], [role="dialog"]'))
      )
        return
      if (event.key === "Escape" && loading) {
        event.preventDefault()
        home()
      } else if (event.key.toLowerCase() === "f" && loaded && target !== canvasRef.current) {
        event.preventDefault()
        rendererRef.current?.fit()
      }
    }
    window.addEventListener("keydown", onKeyDown)
    return () => window.removeEventListener("keydown", onKeyDown)
  }, [chooseFile, dialogOpen, home, loaded, loading])

  const toggleGrid = () => {
    const visible = !gridRef.current
    gridRef.current = visible
    setGrid(visible)
    rendererRef.current?.setGrid(visible)
  }
  const stageActive = Boolean(loaded || loading)
  const size = loaded?.metadata.max
    .map((maximum, axis) => dimensions.format(maximum - loaded.metadata.min[axis]))
    .join(" × ")

  return (
    <FluentProvider
      theme={dark ? webDarkTheme : webLightTheme}
      // Portals inherit theme tokens, not the full-window app-shell layout.
      applyStylesToPortals={false}
      className={`flex flex-col w-full h-full min-w-[320px] text-text bg-surface-secondary ${dark ? "dark theme-dark" : "theme-light"}`}
    >
      <header className="flex-none flex items-center justify-between gap-4 min-h-14 sm:min-h-16 px-4 py-2.5 sm:px-6 sm:py-3 bg-surface border-b border-border">
        <div className="flex items-center gap-2.5 sm:gap-3 text-sm sm:text-base font-semibold tracking-tight">
          <img className="object-contain flex-none" src={icon} width="30" height="30" alt="" />
          <span>{appName}</span>
          <Badge
            appearance="outline"
            className="ml-1.5! font-normal! text-muted! hidden! sm:inline-flex!"
          >
            桌面版
          </Badge>
        </div>
        <Menu
          checkedValues={{ theme: [theme] }}
          onCheckedValueChange={(_, data) => {
            const value = data.checkedItems[0]
            if (data.name === "theme" && isTheme(value)) setTheme(value)
          }}
        >
          <MenuTrigger disableButtonEnhancement>
            <Button
              appearance="subtle"
              icon={<MoreHorizontal20Regular />}
              aria-label="应用菜单"
              title="应用菜单"
            />
          </MenuTrigger>
          <MenuPopover>
            <MenuList>
              <MenuItem
                icon={<Settings20Regular />}
                onClick={() => {
                  setDialog("settings")
                  setDialogOpen(true)
                }}
              >
                预览设置
              </MenuItem>
              <MenuItem
                icon={<Keyboard20Regular />}
                onClick={() => {
                  setDialog("controls")
                  setDialogOpen(true)
                }}
              >
                操作与快捷键
              </MenuItem>
              <MenuItem
                icon={<Info20Regular />}
                onClick={() => {
                  setDialog("about")
                  setDialogOpen(true)
                }}
              >
                关于与许可证
              </MenuItem>
              <MenuDivider />
              <MenuGroup>
                <MenuGroupHeader>文件关联</MenuGroupHeader>
                <MenuItem
                  disabled={actionBusy}
                  onClick={() => void nativeAction("register_associations")}
                >
                  设为默认应用…
                </MenuItem>
                <MenuItem
                  disabled={actionBusy}
                  onClick={() => void nativeAction("unregister_associations")}
                >
                  移除文件关联
                </MenuItem>
              </MenuGroup>
              <MenuDivider />
              <MenuGroup>
                <MenuGroupHeader>外观</MenuGroupHeader>
                <MenuItemRadio name="theme" value="system">
                  跟随系统
                </MenuItemRadio>
                <MenuItemRadio name="theme" value="light">
                  浅色
                </MenuItemRadio>
                <MenuItemRadio name="theme" value="dark">
                  深色
                </MenuItemRadio>
              </MenuGroup>
            </MenuList>
          </MenuPopover>
        </Menu>
      </header>

      <nav
        className="flex-none flex items-center gap-1.5 sm:gap-2 min-h-14 px-3 py-2 sm:px-5 sm:py-2.5 border-b border-border bg-surface [&>button]:h-9! [&>button]:shrink-0 [&>button:has(span.hidden)]:px-3! [&>button:has(span.hidden)]:gap-2! [&>button:not(:has(span.hidden))]:w-9! [&>button:not(:has(span.hidden))]:min-w-9!"
        aria-label="预览命令"
      >
        <Button
          appearance="primary"
          icon={<FolderOpen20Regular />}
          disabled={!bootstrap || choosing}
          onClick={() => void chooseFile()}
          title="打开投影文件 (Ctrl+O)"
        >
          打开<span className="ml-1 opacity-75 text-xs font-normal hidden sm:inline">Ctrl+O</span>
        </Button>
        <Tooltip content="返回主页" relationship="label">
          <Button appearance="subtle" icon={<Home20Regular />} onClick={home} aria-label="主页" />
        </Tooltip>
        <span className="self-center h-5 w-px mx-0.5 sm:mx-1 bg-border shrink-0" />
        <Button
          appearance="subtle"
          icon={<ArrowExpand20Regular />}
          disabled={!loaded}
          onClick={() => rendererRef.current?.fit()}
          title="适配整个投影 (F)"
        >
          适配<span className="ml-1 opacity-75 text-xs font-normal hidden sm:inline">F</span>
        </Button>
        <Tooltip content="缩小 (−)" relationship="label">
          <Button
            appearance="subtle"
            icon={<Subtract20Regular />}
            disabled={!loaded}
            onClick={() => rendererRef.current?.zoom(1.12)}
            aria-label="缩小"
          />
        </Tooltip>
        <Tooltip content="放大 (+)" relationship="label">
          <Button
            appearance="subtle"
            icon={<Add20Regular />}
            disabled={!loaded}
            onClick={() => rendererRef.current?.zoom(0.88)}
            aria-label="放大"
          />
        </Tooltip>
        <span className="self-center h-5 w-px mx-0.5 sm:mx-1 bg-border shrink-0" />
        <Tooltip content={grid ? "隐藏地面网格" : "显示地面网格"} relationship="label">
          <ToggleButton
            appearance="subtle"
            icon={<Grid20Regular />}
            checked={grid}
            onClick={toggleGrid}
            aria-label="地面网格"
          />
        </Tooltip>
        <Tooltip content="材料清单与导出" relationship="label">
          <Button
            appearance="subtle"
            icon={<List20Regular />}
            disabled={!loaded}
            onClick={() => {
              ensureBlockIcons()
              setDialog("materials")
              setDialogOpen(true)
            }}
            aria-label="材料清单与导出"
          />
        </Tooltip>
        <span
          className="ml-auto pl-4 max-w-[40%] md:max-w-[30%] hidden sm:block text-muted text-xs whitespace-nowrap overflow-hidden text-ellipsis"
          title={loaded?.path || loading?.path}
        >
          {loaded ? fileName(loaded.path) : loading ? fileName(loading.path) : "随时可以打开投影文件"}
        </span>
      </nav>

      {notice && (
        <MessageBar intent={notice.intent} className="flex-none rounded-none! break-words">
          <MessageBarBody>
            <MessageBarTitle>
              {notice.intent === "error"
                ? "出错了"
                : notice.intent === "success"
                  ? "完成"
                  : "请注意"}
            </MessageBarTitle>
            {notice.message}
          </MessageBarBody>
          <MessageBarActions
            containerAction={
              <Button
                appearance="transparent"
                icon={<Dismiss20Regular />}
                aria-label="关闭消息"
                onClick={() => setNotice(null)}
              />
            }
          />
        </MessageBar>
      )}

      <main
        className="relative flex-auto min-h-0 overflow-hidden"
        aria-label={stageActive ? "投影预览" : "欢迎"}
      >
        <section
          className="absolute inset-0 overflow-auto [overscroll-behavior:contain]"
          hidden={stageActive}
        >
          <div className="w-full max-w-5xl mx-auto px-5 py-6 sm:px-8 sm:py-8 md:px-12 md:py-14">
            <div className="flex items-center gap-3 sm:gap-4 min-h-24 p-4 sm:p-5 border border-dashed border-border rounded-xl bg-surface flex-wrap sm:flex-nowrap">
              <div className="flex justify-center items-center w-10 h-10 text-accent bg-accent-soft rounded-lg shrink-0">
                <FolderOpen20Regular />
              </div>
              <div className="flex flex-1 flex-col gap-1">
                <strong className="text-sm font-semibold">将投影文件拖到此处</strong>
                <span className="text-muted text-xs">或从电脑中选择文件</span>
              </div>
              <Button
                appearance="primary"
                icon={<FolderOpen20Regular />}
                disabled={!bootstrap || choosing}
                onClick={() => void chooseFile()}
                className="w-full sm:w-auto!"
              >
                {choosing ? "正在选择文件…" : "打开投影文件"}
              </Button>
            </div>
            <div className="flex items-center flex-wrap gap-2 mt-4" aria-label="支持的格式">
              {bootstrap ? (
                bootstrap.extensions.map((extension) => (
                  <Badge key={extension} appearance="outline" shape="rounded">
                    {extension}
                  </Badge>
                ))
              ) : (
                <span className="font-normal text-muted" role="status">
                  正在准备投影查看器…
                </span>
              )}
            </div>
            {bootstrap && bootstrap.demos.length > 0 && (
              <section className="mt-8" aria-labelledby="demos-heading">
                <div className="flex items-baseline justify-between flex-wrap gap-x-5 gap-y-1.5 mb-3">
                  <h2 id="demos-heading" className="m-0 text-base font-semibold">
                    试试内置示例
                  </h2>
                  <span className="text-muted text-xs">体验各种格式，无需下载</span>
                </div>
                <div className="grid grid-cols-2 sm:grid-cols-3 lg:grid-cols-4 gap-2.5">
                  {bootstrap.demos.map((demo) => (
                    <Button
                      key={demo.path}
                      appearance="outline"
                      className="flex! justify-start! items-center! gap-2.5! w-full! min-w-0! min-h-16! p-3! border-border! rounded-lg! bg-surface! text-left! hover:bg-surface-muted! hover:border-accent!"
                      onClick={() => void loadPath(demo.path)}
                      aria-label={`打开 ${demo.name}（${demo.extension}）`}
                    >
                      <span className="text-muted flex shrink-0">
                        <Document20Regular />
                      </span>
                      <span className="flex flex-1 min-w-0 flex-col gap-1">
                        <strong className="text-xs font-semibold overflow-hidden text-ellipsis whitespace-nowrap">
                          {demo.name}
                        </strong>
                        <span className="text-muted text-xs font-normal">{demo.extension}</span>
                      </span>
                      <ArrowRight20Regular className="text-muted shrink-0 w-4" />
                    </Button>
                  ))}
                </div>
              </section>
            )}
          </div>
        </section>

        <section
          className={`absolute inset-0 bg-surface-secondary ${stageActive ? "visible pointer-events-auto" : "invisible pointer-events-none"}`}
          aria-label="可交互的 3D 模型"
          aria-hidden={!stageActive}
          aria-busy={Boolean(loading)}
        >
          <canvas
            ref={canvasRef}
            className="block w-full h-full [touch-action:none] outline-none focus-visible:outline-2 focus-visible:outline-accent focus-visible:-outline-offset-2"
            tabIndex={loaded ? 0 : -1}
            aria-label={loaded ? `${fileName(loaded.path)} 的 3D 预览` : "3D 预览"}
            aria-describedby="canvas-controls"
          />
          <p id="canvas-controls" className="sr-only">
            拖动以旋转视角。右键或中键拖动以平移。滚动滚轮以缩放。方向键旋转视角；Shift 加方向键平移。加号与减号缩放。按 F 或 Home 键适配模型。
          </p>
          {loaded && (
            <div
              className="absolute bottom-4 left-1/2 -translate-x-1/2 flex items-center gap-2 sm:gap-2.5 px-3 py-1.5 border border-border rounded-md bg-surface text-muted text-xs whitespace-nowrap pointer-events-none max-w-[calc(100%-2rem)] sm:max-w-none"
              aria-hidden="true"
            >
              拖动旋转视角<span>·</span>右键拖动平移<span>·</span>滚轮缩放
            </div>
          )}
          {loading && (
            <div className="absolute inset-0 grid place-items-center bg-surface-secondary p-6 sm:p-8">
              <div
                className="max-w-md w-full p-6 sm:p-8 border border-border rounded-xl bg-surface text-center shadow-lg"
                role="status"
                aria-live="polite"
              >
                <div className="inline-flex justify-center items-center w-12 h-12 mb-4 rounded-xl bg-accent-soft text-accent">
                  <Cube24Regular />
                </div>
                <h2 className="mt-0 mb-2 text-xl font-semibold leading-snug">
                  {loading.phase === "decode" ? "正在准备投影文件" : "正在构建预览"}
                </h2>
                <p
                  className="mt-0 mb-3 text-sm leading-relaxed break-words font-semibold"
                  title={loading.path}
                >
                  {fileName(loading.path)}
                </p>
                <p className="mt-0 mb-3 text-sm leading-relaxed text-muted">
                  {loading.phase === "decode"
                    ? "正在本地读取方块。"
                    : loading.phase === "mesh" || loading.phase === "stream"
                      ? loading.total > 0
                        ? `正在生成几何体：${numbers.format(loading.completed)} / ${numbers.format(loading.total)} 个区块（${Math.round((loading.completed / loading.total) * 100)}%）。`
                        : "正在生成几何体并上传就绪的批次。"
                      : `正在上传几何体与纹理：${formatMB(loading.completed)} / ${formatMB(loading.total)}（${Math.round((loading.completed / loading.total) * 100)}%）。`}
                </p>
                {loading.phase === "stream" && (
                  <p className="mt-0 mb-3 text-sm leading-relaxed text-muted">
                    已上传 {formatMB(loading.uploadedBytes)} 模型数据。生成与上传同时进行；两者都完成后才会显示预览。
                  </p>
                )}
                <ProgressBar
                  value={
                    loading.phase === "decode" || loading.total === 0
                      ? undefined
                      : loading.completed
                  }
                  max={
                    loading.phase === "decode" || loading.total === 0 ? undefined : loading.total
                  }
                  aria-label={
                    loading.phase === "decode"
                      ? "正在读取方块"
                      : loading.phase === "mesh" || loading.phase === "stream"
                        ? "正在生成几何体"
                        : "正在上传模型数据"
                  }
                  className="mb-4"
                />
                <Button appearance="secondary" onClick={home} className="mt-1.5!">
                  取消
                  <span className="ml-2.5 opacity-75 text-xs font-normal hidden sm:inline">
                    Esc
                  </span>
                </Button>
              </div>
            </div>
          )}
        </section>

        {dragging && (
          <div
            className="absolute z-10 inset-3 sm:inset-4 grid place-items-center border-2 border-dashed border-accent rounded-xl bg-surface opacity-95 text-center pointer-events-none"
            role="status"
          >
            <div className="[&>svg]:w-10 [&>svg]:h-10 [&>svg]:mb-4 [&>svg]:text-accent">
              <FolderOpen20Regular />
              <h2 className="mt-0 mb-2 text-2xl font-semibold">松开即可预览</h2>
              <p className="mt-0 px-4 text-muted text-sm">
                将打开第一个受支持的投影文件。
              </p>
            </div>
          </div>
        )}
      </main>

      <footer
        className="flex-none flex items-center flex-wrap gap-x-4 sm:gap-x-6 gap-y-1.5 min-h-9 px-4 sm:px-6 py-2 border-t border-border bg-surface text-muted text-xs leading-normal [&_strong]:text-text [&_strong]:font-semibold"
        aria-label="预览信息"
      >
        {loaded ? (
          <>
            <span>
              <strong>{numbers.format(loaded.metadata.blockCount)}</strong> 个方块
            </span>
            <span title="几何尺寸（单位：方块）">{size} 方块</span>
            <span>{numbers.format(loaded.metadata.triangleCount)} 个三角面</span>
            <span className="ml-auto">
              {formatMB(loaded.metadata.byteLength)} 模型数据加载完成，用时{" "}
              {loaded.seconds.toFixed(2)} 秒。
            </span>
          </>
        ) : (
          <>
            <span role="status" className="flex items-center gap-2">
              {!loading && !choosing && <ShieldCheckmark20Regular className="shrink-0" />}
              {loading
                ? loading.phase === "decode"
                  ? "正在解码…"
                  : loading.phase === "mesh"
                    ? "正在生成几何体…"
                    : loading.phase === "stream"
                      ? "正在生成并上传…"
                      : "正在上传到图形设备…"
                : choosing
                  ? "请在文件对话框中选择投影文件"
                  : "全程离线运行，无需联网。"}
            </span>
            {loading && (
              <div className="ml-auto flex max-w-full flex-wrap justify-end gap-x-3 gap-y-1 text-right">
                {previewMemory !== null ? (
                  <span title="主进程与解码进程的私有工作集（仅计常驻私有页），不包括 WebView2 与 GPU 内存。">
                    进程内存 {formatMB(previewMemory)}
                  </span>
                ) : (
                  <span>正在采样内存…</span>
                )}
                {(loading.phase === "upload" || loading.phase === "stream") && (
                  <span>已上传 {formatMB(loading.uploadedBytes)} 模型数据</span>
                )}
              </div>
            )}
            {!loading && bootstrap && <span className="ml-auto">v{bootstrap.version}</span>}
          </>
        )}
      </footer>

      <Dialog
        open={dialogOpen}
        onOpenChange={(_, data) => {
          setDialogOpen(data.open)
        }}
      >
        <DialogSurface className="w-[min(760px,94vw)]">
          <DialogBody>
            <DialogTitle>
              {dialog === "error"
                ? "无法打开预览"
                : dialog === "controls"
                  ? "操作与快捷键"
                  : dialog === "settings"
                    ? "预览设置"
                    : dialog === "materials"
                      ? "材料清单与导出"
                      : `关于 ${appName}`}
            </DialogTitle>
            <DialogContent>
              {dialog === "materials" ? (
                <div className="flex flex-col gap-4 max-h-[65vh]">
                  <p className="m-0 text-sm leading-relaxed text-muted">
                    材料清单来自当前预览（已应用替换）。替换会按方块名称应用到整个投影，并保留原方块的属性。
                  </p>
                  {replacements.path === loaded?.path && replacements.rules.length > 0 && (
                    <div className="flex flex-wrap gap-2">
                      {replacements.rules.map((rule) => (
                        <span
                          key={rule.from}
                          className="inline-flex items-center gap-1.5 pl-2.5 pr-1.5 py-1 rounded-md bg-surface-muted border border-border text-xs"
                        >
                          <span>{rule.from}</span>
                          <ArrowRight20Regular className="w-3.5 shrink-0" />
                          <span>{rule.to}</span>
                          <button
                            type="button"
                            className="ml-1 p-1 rounded hover:bg-surface border-0 cursor-pointer bg-transparent text-muted"
                            aria-label={`移除替换 ${rule.from}`}
                            onClick={() => removeReplacement(rule.from)}
                          >
                            <Dismiss20Regular className="w-3.5" />
                          </button>
                        </span>
                      ))}
                    </div>
                  )}
                  <div className="flex flex-col divide-y divide-[var(--colorNeutralStroke2,#dddddd)] max-h-[34vh] overflow-y-auto border border-border rounded-lg">
                    {(loaded?.metadata.materials ?? []).map((material) => (
                      <div
                        key={materialStateLabel(material)}
                        className="flex items-center gap-3 px-3 py-2"
                      >
                        {iconsByName.get(material.name) !== undefined && (
                          <img
                            src={iconsByName.get(material.name)}
                            alt=""
                            title={tooltipLabel(material.name)}
                            className="w-6 h-6 shrink-0 [image-rendering:pixelated]"
                            loading="lazy"
                            decoding="async"
                          />
                        )}
                        <span className="flex-1 min-w-0 text-xs break-all">
                          <span className="font-semibold">{material.name}</span>
                          {blockZhName(material.name) !== "" && (
                            <span className="text-muted"> {blockZhName(material.name)}</span>
                          )}
                          {material.properties.length > 0 && (
                            <span className="text-muted">
                              {" "}
                              [{material.properties
                                .map(([key, value]) => `${key}=${value}`)
                                .join(", ")}]
                            </span>
                          )}
                        </span>
                        <span className="text-muted text-xs whitespace-nowrap" title="该方块的方块数量">
                          {numbers.format(material.count)} 个
                        </span>
                        <Button
                          appearance="subtle"
                          size="small"
                          disabled={replacingFrom !== null || loading !== null}
                          onClick={() => beginReplacement(material.name)}
                        >
                          替换
                        </Button>
                      </div>
                    ))}
                    {(loaded?.metadata.materials ?? []).length === 0 && (
                      <span className="px-3 py-3 text-muted text-xs">
                        暂无材料数据，请等待预览加载完成。
                      </span>
                    )}
                  </div>
                  {replacingFrom !== null && (
                    <div className="flex flex-col gap-2 p-3 border border-border rounded-lg bg-surface-muted">
                      <span className="text-xs">
                        将 <span className="font-semibold">{replacingFrom}</span> 替换为：
                        {replacementTo.trim() !== "" && (
                          <span className="ml-2 text-muted">
                            {tooltipLabel(replacementTo.trim().toLowerCase())}
                          </span>
                        )}
                      </span>
                      <input
                        className="h-8 px-2 text-sm rounded-md border border-border bg-surface outline-none focus-visible:outline-2 focus-visible:outline-accent"
                        placeholder="搜索方块：中文名或 minecraft:id"
                        value={replacementTo}
                        autoFocus
                        onChange={(event) => setReplacementTo(event.target.value)}
                        onKeyDown={(event) => {
                          if (event.key === "Enter") confirmReplacement()
                          if (event.key === "Escape") setReplacingFrom(null)
                        }}
                      />
                      <div className="grid gap-1 p-1.5 overflow-y-auto max-h-56 rounded-md border border-border bg-surface [grid-template-columns:repeat(auto-fill,minmax(48px,1fr))]">
                        {blockIcons === null ? (
                          <span className="col-span-full py-6 text-center text-xs text-muted">
                            正在生成方块图标…
                          </span>
                        ) : (blockIcons ?? []).filter((entry) =>
                            matchesBlockQuery(entry.name, replacementTo),
                          ).length === 0 ? (
                          <span className="col-span-full py-6 text-center text-xs text-muted">
                            没有匹配的方块；可直接回车使用输入的 id。
                          </span>
                        ) : (
                          (blockIcons ?? [])
                            .filter((entry) => matchesBlockQuery(entry.name, replacementTo))
                            .map((entry) => {
                              const selected =
                                replacementTo.trim().toLowerCase() === entry.name
                              return (
                                <button
                                  key={entry.name}
                                  type="button"
                                  title={tooltipLabel(entry.name)}
                                  aria-label={tooltipLabel(entry.name)}
                                  className={`flex items-center justify-center h-12 rounded-md border cursor-pointer bg-surface-muted ${
                                    selected
                                      ? "border-accent outline-2 outline-accent"
                                      : "border-transparent hover:border-border"
                                  }`}
                                  onClick={() => setReplacementTo(entry.name)}
                                >
                                  {entry.icon ? (
                                    <img
                                      src={entry.icon}
                                      alt=""
                                      className="w-10 h-10 [image-rendering:pixelated]"
                                      loading="lazy"
                                      decoding="async"
                                    />
                                  ) : (
                                    <span className="px-1 text-[10px] leading-tight text-muted break-all">
                                      {entry.name.split(":")[1] ?? entry.name}
                                    </span>
                                  )}
                                </button>
                              )
                            })
                        )}
                      </div>
                      <div className="flex justify-end gap-2">
                        <Button size="small" onClick={() => setReplacingFrom(null)}>
                          取消
                        </Button>
                        <Button
                          appearance="primary"
                          size="small"
                          disabled={replacementTo.trim() === ""}
                          onClick={confirmReplacement}
                        >
                          替换并重新渲染
                        </Button>
                      </div>
                    </div>
                  )}
                  <div className="flex items-center gap-2 flex-wrap pt-2 border-t border-border">
                    <span className="text-xs text-muted">导出新原理图：</span>
                    <select
                      className="h-8 px-2 text-sm rounded-md border border-border bg-surface"
                      value={exportFormat}
                      onChange={(event) => setExportFormat(event.target.value)}
                    >
                      {exportFormats.map((format) => (
                        <option key={format.value} value={format.value}>
                          {format.label}
                        </option>
                      ))}
                    </select>
                    <Button
                      appearance="primary"
                      size="small"
                      disabled={exporting || loading !== null}
                      onClick={() => void runExport()}
                    >
                      {exporting ? "正在导出…" : "导出…"}
                    </Button>
                    <span className="text-muted text-xs">
                      在保存对话框中选择位置；导出包含已应用的替换。
                    </span>
                  </div>
                </div>
              ) : dialog === "error" ? (
                <>
                  <p>预览已关闭，你已返回主界面。</p>
                  <pre className="whitespace-pre-wrap [overflow-wrap:anywhere] max-h-[45vh] overflow-auto select-text text-xs">
                    {errorDetails}
                  </pre>
                </>
              ) : dialog === "settings" ? (
                <div className="flex flex-col gap-5 max-h-[60vh] overflow-y-auto">
                  <p className="m-0 leading-relaxed">
                    更改会自动保存，并在下次打开投影文件时生效。当前正在进行的预览或加载不会受影响。
                  </p>
                  <div className="flex flex-col gap-3">
                    <div className="flex items-center justify-between gap-4">
                      <Switch
                        label="限制解码器内存"
                        checked={previewSettings.memoryLimitEnabled}
                        aria-describedby="memory-limit-description"
                        onChange={(_, data) =>
                          updatePreviewSettings({ memoryLimitEnabled: data.checked })
                        }
                      />
                      <div className="ml-auto flex items-center gap-2">
                        <SpinButton
                          value={previewSettings.memoryLimitMB}
                          min={2048}
                          max={8192}
                          step={1}
                          disabled={!previewSettings.memoryLimitEnabled}
                          aria-label="解码器内存上限（MB）"
                          aria-describedby="memory-limit-description"
                          className="w-32"
                          onChange={(_, data) => {
                            const value =
                              data.value ??
                              (data.displayValue && /^\d+$/.test(data.displayValue)
                                ? Number(data.displayValue)
                                : null)
                            if (
                              value !== null &&
                              Number.isInteger(value) &&
                              value >= 2048 &&
                              value <= 8192
                            )
                              updatePreviewSettings({ memoryLimitMB: value })
                          }}
                        />
                        <span className="text-sm">MB</span>
                      </div>
                    </div>
                    <p
                      id="memory-limit-description"
                      className="m-0 text-sm leading-relaxed text-muted"
                    >
                      限制的是独立的解码进程，而不是图形或应用总内存。如果启用此限制后解码进程中途停止，加载会返回主页并显示错误；此限制可能有关，但崩溃无法确认确实达到了上限。禁用限制可能耗尽系统内存。
                    </p>
                  </div>
                  <div className="flex flex-col gap-3">
                    <Switch
                      label="将几何体分块"
                      checked={previewSettings.chunkingEnabled}
                      disabled={previewSettings.multithreadingEnabled}
                      aria-describedby="chunk-size-description"
                      onChange={(_, data) =>
                        updatePreviewSettings({ chunkingEnabled: data.checked })
                      }
                    />
                    <Field label={`区块大小（每边方块数）：${previewSettings.chunkSize}`}>
                      <Slider
                        className="chunk-size-slider mb-8"
                        min={0}
                        max={chunkSizes.length - 1}
                        step={1}
                        aria-label="区块大小（每边方块数）"
                        value={chunkSizes.indexOf(previewSettings.chunkSize)}
                        disabled={!previewSettings.chunkingEnabled}
                        rail={{
                          className: "chunk-size-rail",
                          children: chunkSizes.map((size, index) => (
                            <span
                              key={size}
                              aria-hidden="true"
                              className="chunk-size-mark"
                              style={{ left: `${(index / (chunkSizes.length - 1)) * 100}%` }}
                            >
                              <span className="chunk-size-mark-dot" />
                              <span className="chunk-size-mark-label">{size}</span>
                            </span>
                          )),
                        }}
                        onChange={(_, data) =>
                          updatePreviewSettings({ chunkSize: chunkSizes[data.value] })
                        }
                      />
                    </Field>
                    <p
                      id="chunk-size-description"
                      className="m-0 text-sm leading-relaxed text-muted"
                    >
                      较小的区块可以降低网格生成的峰值内存，并允许在区块之间取消。禁用分块会增加峰值内存占用和取消延迟。
                    </p>
                  </div>
                  <div className="flex flex-col gap-3">
                    <Switch
                      label="启用多线程"
                      checked={previewSettings.multithreadingEnabled}
                      disabled={
                        !previewSettings.chunkingEnabled ||
                        !bootstrap ||
                        bootstrap.maxWorkerThreads < 2
                      }
                      aria-describedby="thread-count-description"
                      onChange={(_, data) =>
                        updatePreviewSettings({ multithreadingEnabled: data.checked })
                      }
                    />
                    <Field label="工作线程数">
                      <SpinButton
                        value={previewSettings.threadCount}
                        min={2}
                        max={Math.max(2, bootstrap?.maxWorkerThreads ?? 2)}
                        step={1}
                        disabled={!previewSettings.multithreadingEnabled}
                        aria-label="工作线程数"
                        aria-describedby="thread-count-description"
                        className="w-32"
                        onChange={(_, data) => {
                          const value =
                            data.value ??
                            (data.displayValue && /^\d+$/.test(data.displayValue)
                              ? Number(data.displayValue)
                              : null)
                          if (
                            value !== null &&
                            Number.isInteger(value) &&
                            value >= 2 &&
                            value <= (bootstrap?.maxWorkerThreads ?? 1)
                          )
                            updatePreviewSettings({ threadCount: value })
                        }}
                      />
                    </Field>
                    <Switch
                      label="保守内存调度"
                      checked={previewSettings.conservativeMemoryScheduling}
                      disabled={!previewSettings.multithreadingEnabled}
                      aria-describedby="conservative-memory-description"
                      onChange={(_, data) =>
                        updatePreviewSettings({ conservativeMemoryScheduling: data.checked })
                      }
                    />
                    <p
                      id="conservative-memory-description"
                      className="m-0 text-sm leading-relaxed text-muted"
                    >
                      默认关闭。开启后可以减少并发的网格工作以及排队的批次和上传页数量；这可能会降低解码速度。保持关闭会更充分地利用所选的工作线程数，但可能占用更多内存。无论哪种模式，独立的解码器内存限制都保持有效。
                    </p>
                    <p
                      id="thread-count-description"
                      className="m-0 text-sm leading-relaxed text-muted"
                    >
                      需要启用分块，并且至少有两个可用的逻辑处理器。最多使用 {bootstrap?.maxWorkerThreads ?? 1} 个线程并行进行解码、网格生成和上传准备。内存优先调度可能使用更少的线程；额外的工作缓冲仍可能增加峰值内存。GPU 提交始终在主线程上进行。
                    </p>
                  </div>
                </div>
              ) : dialog === "controls" ? (
                <>
                  <p className="mt-0 mb-5 leading-relaxed">
                    点击预览区域或按 Tab 键聚焦后，即可使用键盘控制。
                  </p>
                  <dl className="flex flex-col gap-0 my-0 mb-5 [&>div]:grid [&>div]:grid-cols-[80px_1fr] sm:[&>div]:grid-cols-[120px_1fr] [&>div]:gap-3 sm:[&>div]:gap-4 [&>div]:py-3 [&>div]:border-b [&>div]:border-[var(--colorNeutralStroke2,#dddddd)] [&_dt]:font-semibold [&_dd]:m-0 [&_dd]:leading-normal">
                    <div>
                      <dt>旋转视角</dt>
                      <dd>左键拖动 / 方向键</dd>
                    </div>
                    <div>
                      <dt>平移</dt>
                      <dd>右键或中键拖动 / Shift + 方向键</dd>
                    </div>
                    <div>
                      <dt>缩放</dt>
                      <dd>
                        滚动滚轮 / <kbd>+</kbd> 或 <kbd>−</kbd>
                      </dd>
                    </div>
                    <div>
                      <dt>适配模型</dt>
                      <dd>
                        预览中按 <kbd>F</kbd> / <kbd>Home</kbd>
                      </dd>
                    </div>
                    <div>
                      <dt>打开投影文件</dt>
                      <dd>
                        <kbd>Ctrl</kbd> + <kbd>O</kbd>
                      </dd>
                    </div>
                    <div>
                      <dt>取消加载</dt>
                      <dd>
                        <kbd>Esc</kbd>
                      </dd>
                    </div>
                  </dl>
                  <p className="text-muted leading-relaxed">
                    使用网格按钮可以显示或隐藏地面网格。命令栏上的“主页”按钮会返回内置示例。
                  </p>
                </>
              ) : (
                <div className="leading-relaxed [&>p]:mb-3">
                  <div className="flex items-center gap-3.5 my-3 mb-6 [&>div]:flex [&>div]:flex-col [&>div]:gap-1 [&_strong]:text-lg [&_strong]:font-semibold [&_span]:text-xs">
                    <img
                      className="object-contain flex-none"
                      src={icon}
                      width="48"
                      height="48"
                      alt=""
                    />
                    <div>
                      <strong>{appName}</strong>
                      <span>版本 {bootstrap?.version || "未知"}</span>
                    </div>
                  </div>
                  <p>
                    一款本地交互式的 Minecraft 投影与结构查看器，灵感来自 LitematicaQL。
                  </p>
                  <p>基于 Nucleation、Tauri、WebGL、React 与 Fluent UI 构建。</p>
                  <p>
                    本程序依据 GNU Affero 通用公共许可证第 3 版发行，不提供任何担保。在随附许可证条款允许的范围内可以重新分发。
                  </p>
                  <p className="text-muted">
                    应用程序许可证与第三方声明已包含在安装目录中。
                  </p>
                </div>
              )}
            </DialogContent>
            <DialogActions>
              {dialog === "about" && (
                <Button
                  disabled={actionBusy}
                  onClick={() => {
                    setDialogOpen(false)
                    void nativeAction("show_licenses")
                  }}
                >
                  打开许可证文件夹
                </Button>
              )}
              <Button appearance="primary" onClick={() => setDialogOpen(false)}>
                关闭
              </Button>
            </DialogActions>
          </DialogBody>
        </DialogSurface>
      </Dialog>
    </FluentProvider>
  )
}
