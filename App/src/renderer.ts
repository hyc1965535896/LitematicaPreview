import { mat4, vec3 } from "gl-matrix"
import {
  PreparedUploads,
  PreparationWorkerPool,
  validateUpload,
  type PreparationWorker,
} from "./upload-preparation"
import {
  buildUploadLayout,
  UPLOAD_CHUNK,
  type PreviewReadRange,
  type UploadPage,
} from "./upload-layout"

import {
  BUFFER_FORMATS,
  PreviewStream,
  validateMetadata,
  type PreviewMetadata,
  type PreviewStreamEvent,
} from "./preview-stream"
export type { PreviewBatch, PreviewMetadata, PreviewStreamEvent } from "./preview-stream"

type Part = {
  vao: WebGLVertexArrayObject
  indexByteOffset: number
  texture: WebGLTexture
  count: number
  alphaMode: 0 | 1 | 2
}

type Model = {
  textures: WebGLTexture[]
  buffers: WebGLBuffer[]
  parts: Part[]
  gridVao: WebGLVertexArrayObject | null
  gridBuffer: WebGLBuffer | null
  gridCount: number
  released: boolean
}

type Pipeline = {
  scene: WebGLProgram
  output: WebGLProgram
  fullscreen: WebGLVertexArrayObject
  matrix: WebGLUniformLocation
  offset: WebGLUniformLocation
  alphaMode: WebGLUniformLocation
  grid: WebGLUniformLocation
}

type Targets = {
  width: number
  height: number
  color: WebGLTexture | null
  resolve: WebGLFramebuffer | null
  draw: WebGLFramebuffer | null
  depth: WebGLRenderbuffer | null
  multisampleColor: WebGLRenderbuffer | null
}

type UploadBudget = { bytes: number; started: number }
type ReadBuffer = (ranges: readonly PreviewReadRange[]) => Promise<ArrayBuffer>

const FOV = (28 * Math.PI) / 180
const HALF_FOV_TAN = Math.tan(FOV / 2)
const MAX_RENDER_PIXELS = 16 * 1024 * 1024
const UP = new Float32Array([0, 1, 0])

function required<T>(value: T | null, name: string): T {
  if (value === null)
    throw new Error(
      `图形设备无法分配${name}，显卡内存可能已耗尽。`,
    )
  return value
}

function errorOf(error: unknown): Error {
  return error instanceof Error ? error : new Error(String(error))
}

function createPreparationWorker(onError: (error: Error) => void): PreparationWorker {
  const worker = new Worker(new URL("./upload-worker.ts", import.meta.url), { type: "module" })
  let pending: { resolve: (buffer: ArrayBuffer) => void; reject: (error: Error) => void } | null =
    null
  let failure: Error | null = null
  const fail = (error: Error, notify = true) => {
    failure = error
    const task = pending
    pending = null
    task?.reject(error)
    if (notify) onError(error)
  }
  worker.onmessage = (event: MessageEvent<{ buffer?: ArrayBuffer; error?: string }>) => {
    const task = pending
    if (!task) return
    if (event.data?.buffer instanceof ArrayBuffer) {
      pending = null
      task.resolve(event.data.buffer)
    } else {
      fail(new Error(event.data?.error || "预览准备失败。"))
    }
  }
  worker.onerror = (event) => {
    event.preventDefault()
    fail(new Error(event.message || "预览准备失败。"))
  }
  worker.onmessageerror = () => fail(new Error("预览工作进程返回了无法读取的数据。"))
  return {
    prepare: (buffer, page) =>
      new Promise<ArrayBuffer>((resolve, reject) => {
        if (failure) {
          reject(failure)
          return
        }
        pending = { resolve, reject }
        try {
          worker.postMessage({ buffer, page }, [buffer])
        } catch (error) {
          fail(errorOf(error))
        }
      }),
    terminate: () => {
      fail(new Error("Cancelled"), false)
      worker.onmessage = null
      worker.onerror = null
      worker.onmessageerror = null
      worker.terminate()
    },
  }
}

const VERTEX_SOURCE = `#version 300 es
precision highp float;
layout(location=0) in vec3 position;
layout(location=1) in vec3 normal;
layout(location=2) in vec2 uv;
layout(location=3) in vec4 color;
uniform mat4 mvp;
uniform vec3 offset;
out vec3 surfaceNormal;
out vec2 textureUv;
out vec4 tint;
void main() {
  gl_Position = mvp * vec4(position + offset, 1.0);
  surfaceNormal = normal;
  textureUv = uv;
  tint = color;
}`

const FRAGMENT_SOURCE = `#version 300 es
precision highp float;
in vec3 surfaceNormal;
in vec2 textureUv;
in vec4 tint;
uniform sampler2D blockTexture;
uniform int alphaMode;
uniform bool isGrid;
out vec4 pixel;
void main() {
  if (isGrid) {
    pixel = vec4(0.020, 0.030, 0.042, 1.0);
    return;
  }
  // SRGB8_ALPHA8 textures are decoded by the sampler; tint, ambient occlusion,
  // and lighting are applied in linear space.
  vec4 base = texture(blockTexture, textureUv) * tint;
  if (alphaMode == 1 && base.a < 0.5) discard;
  vec3 n = normalize(surfaceNormal) * (gl_FrontFacing ? 1.0 : -1.0);
  float light = 0.68 + 0.32 * max(dot(n, normalize(vec3(1.0, 1.4, 0.8))), 0.0);
  pixel = vec4(base.rgb * light, alphaMode == 2 ? base.a : 1.0);
}`

const OUTPUT_VERTEX_SOURCE = `#version 300 es
precision highp float;
out vec2 textureUv;
void main() {
  vec2 p = vec2(float((gl_VertexID << 1) & 2), float(gl_VertexID & 2));
  textureUv = p;
  gl_Position = vec4(p * 2.0 - 1.0, 0.0, 1.0);
}`

const OUTPUT_FRAGMENT_SOURCE = `#version 300 es
precision highp float;
in vec2 textureUv;
uniform sampler2D linearFrame;
out vec4 pixel;
void main() {
  vec3 linear = texture(linearFrame, textureUv).rgb;
  vec3 srgb = mix(linear * 12.92, 1.055 * pow(linear, vec3(1.0 / 2.4)) - 0.055,
                  greaterThan(linear, vec3(0.0031308)));
  pixel = vec4(srgb, 1.0);
}`

function compile(gl: WebGL2RenderingContext, type: number, source: string): WebGLShader {
  const shader = required(gl.createShader(type), "a shader")
  gl.shaderSource(shader, source)
  gl.compileShader(shader)
  if (gl.getShaderParameter(shader, gl.COMPILE_STATUS)) return shader
  const message = gl.getShaderInfoLog(shader) || "Unknown shader compilation error."
  gl.deleteShader(shader)
  throw new Error(`WebGL 2 着色器编译失败：${message}`)
}

function program(
  gl: WebGL2RenderingContext,
  vertexSource: string,
  fragmentSource: string,
): WebGLProgram {
  const vertex = compile(gl, gl.VERTEX_SHADER, vertexSource)
  let fragment: WebGLShader | null = null
  let linked: WebGLProgram | null = null
  try {
    fragment = compile(gl, gl.FRAGMENT_SHADER, fragmentSource)
    linked = required(gl.createProgram(), "着色器程序")
    gl.attachShader(linked, vertex)
    gl.attachShader(linked, fragment)
    gl.linkProgram(linked)
    if (!gl.getProgramParameter(linked, gl.LINK_STATUS)) {
      throw new Error(
        `WebGL 2 程序链接失败：${gl.getProgramInfoLog(linked) || "未知链接错误。"}`,
      )
    }
    gl.detachShader(linked, vertex)
    gl.detachShader(linked, fragment)
    return linked
  } catch (error) {
    if (linked) gl.deleteProgram(linked)
    throw error
  } finally {
    gl.deleteShader(vertex)
    if (fragment) gl.deleteShader(fragment)
  }
}

export class SchematicRenderer {
  private readonly gl: WebGL2RenderingContext
  private pipeline: Pipeline | null = null
  private targets: Targets | null = null
  private model: Model | null = null
  private readonly staged = new Set<Model>()
  private preparation: PreparedUploads | null = null
  private stream: PreviewStream | null = null
  private preparationPool: PreparationWorkerPool | null = null
  // Context recovery replaces the renderer, but old IPC reads must drain first.
  private static pendingPreparationReads: Promise<void> = Promise.resolve()
  private readonly yieldChannel = new MessageChannel()
  private readonly pendingYields: (() => void)[] = []
  private readonly observer: ResizeObserver
  private dprQuery: MediaQueryList | null = null
  private generation = 0
  private frame = 0
  private disposed = false
  private contextLost = false
  private failed = false
  private gridVisible = true
  private maxTextureSize = 1
  private maxWidth = 1
  private maxHeight = 1
  private samples = 1
  private aspect = 1
  private cssHeight = 1
  private yaw = Math.PI / 4
  private pitch = Math.atan(1 / Math.sqrt(2))
  private distance = 10
  private fittedDistance = 10
  private readonly centre = new Float32Array(3)
  private readonly minimum = new Float32Array(3)
  private readonly maximum = new Float32Array(3)
  private readonly target = new Float32Array(3)
  private readonly direction = new Float32Array(3)
  private readonly forward = new Float32Array(3)
  private readonly right = new Float32Array(3)
  private readonly up = new Float32Array(3)
  private readonly eye = new Float32Array(3)
  private readonly view = new Float32Array(16)
  private readonly projection = new Float32Array(16)
  private readonly mvp = new Float32Array(16)
  private pointerId: number | null = null
  private pointerButton = 0
  private lastX = 0
  private lastY = 0
  private readonly originalTabIndex: string | null
  private readonly originalTouchAction: string

  constructor(
    private readonly canvas: HTMLCanvasElement,
    private readonly onError: (message: string) => void,
  ) {
    this.originalTabIndex = canvas.getAttribute("tabindex")
    this.originalTouchAction = canvas.style.touchAction
    this.observer = new ResizeObserver(this.onResize)
    const gl = canvas.getContext("webgl2", {
      alpha: false,
      antialias: false,
      depth: false,
      stencil: false,
      premultipliedAlpha: false,
      preserveDrawingBuffer: false,
    })
    if (!gl) {
      this.observer.disconnect()
      this.yieldChannel.port1.close()
      this.yieldChannel.port2.close()
      const message =
        "WebGL 2 不可用。请启用图形加速或更新显卡驱动后再预览投影文件。"
      onError(message)
      throw new Error(message)
    }
    this.gl = gl
    try {
      this.initialize()
    } catch (error) {
      this.observer.disconnect()
      this.yieldChannel.port1.close()
      this.yieldChannel.port2.close()
      onError(errorOf(error).message)
      throw error
    }
    this.yieldChannel.port1.onmessage = () => this.pendingYields.shift()?.()
    if (canvas.tabIndex < 0) canvas.tabIndex = 0
    canvas.style.touchAction = "none"
    canvas.addEventListener("pointerdown", this.onPointerDown)
    canvas.addEventListener("pointermove", this.onPointerMove)
    canvas.addEventListener("pointerup", this.onPointerEnd)
    canvas.addEventListener("pointercancel", this.onPointerEnd)
    canvas.addEventListener("lostpointercapture", this.onPointerEnd)
    canvas.addEventListener("wheel", this.onWheel, { passive: false })
    canvas.addEventListener("keydown", this.onKeyDown)
    canvas.addEventListener("contextmenu", this.onContextMenu)
    canvas.addEventListener("webglcontextlost", this.onContextLost)
    canvas.addEventListener("webglcontextrestored", this.onContextRestored)
    window.addEventListener("resize", this.onResize)
    this.observer.observe(canvas)
    this.watchDpr()
    this.syncSize()
    this.invalidate()
  }

  async load(
    metadata: PreviewMetadata,
    readBuffer: ReadBuffer,
    isCurrent: () => boolean,
    onUploadProgress: (uploadedBytes: number, totalBytes: number) => void,
    threadCount: number | null,
    speedFirst: boolean,
  ): Promise<PreviewMetadata> {
    return this.loadModel(isCurrent, async (model, guard) => {
      validateMetadata(metadata, this.maxTextureSize)
      let uploadedBytes = 0
      onUploadProgress(0, metadata.byteLength)
      await this.appendModel(
        model,
        metadata,
        readBuffer,
        guard,
        (bytes) => {
          uploadedBytes += bytes
          onUploadProgress(uploadedBytes, metadata.byteLength)
        },
        threadCount,
        speedFirst,
      )
      return metadata
    })
  }

  async loadStream(
    nextBatch: (previousBatchId: number | null) => Promise<PreviewStreamEvent>,
    readBuffer: (batchId: number, ranges: readonly PreviewReadRange[]) => Promise<ArrayBuffer>,
    isCurrent: () => boolean,
    onUploadProgress: (uploadedBytes: number) => void,
    threadCount: number,
    speedFirst: boolean,
  ): Promise<PreviewMetadata> {
    return this.loadModel(isCurrent, async (model, guard) => {
      const stream = new PreviewStream()
      this.stream = stream
      const pool = new PreparationWorkerPool(createPreparationWorker)
      this.preparationPool = pool
      let uploadedBytes = 0
      try {
        onUploadProgress(0)
        return await stream.consume(
          nextBatch,
          async (batch) => {
            await this.appendModel(
              model,
              batch.metadata,
              (ranges) => readBuffer(batch.batchId, ranges),
              guard,
              (bytes) => {
                uploadedBytes += bytes
                onUploadProgress(uploadedBytes)
              },
              threadCount,
              speedFirst,
              pool,
            )
          },
          guard,
          this.maxTextureSize,
        )
      } finally {
        stream.cancel()
        pool.dispose()
        if (this.preparationPool === pool) this.preparationPool = null
        if (this.stream === stream) this.stream = null
      }
    })
  }

  private async loadModel(
    isCurrent: () => boolean,
    upload: (model: Model, guard: () => void) => Promise<PreviewMetadata>,
  ): Promise<PreviewMetadata> {
    if (this.disposed || !isCurrent()) throw new Error("Cancelled")
    const generation = ++this.generation
    this.cancelStaged()
    const model: Model = {
      textures: [],
      buffers: [],
      parts: [],
      gridVao: null,
      gridBuffer: null,
      gridCount: 0,
      released: false,
    }
    const guard = () => {
      if (this.disposed || generation !== this.generation || !isCurrent())
        throw new Error("Cancelled")
      if (this.contextLost || this.gl.isContextLost())
        throw new Error(
          "图形上下文已丢失。请在图形设备恢复后重新打开投影文件。",
        )
      if (!this.pipeline)
        throw new Error(
          "图形渲染器不可用。请重新打开应用以初始化图形设备。",
        )
    }
    this.staged.add(model)
    const gl = this.gl
    try {
      guard()
      const metadata = await upload(model, guard)
      guard()
      this.buildGrid(model, metadata)
      this.checkGraphics("上传该投影")
      guard()
      gl.bindVertexArray(null)
      this.staged.delete(model)
      const previous = this.model
      this.model = model
      for (let axis = 0; axis < 3; axis++) {
        this.centre[axis] = (metadata.min[axis] + metadata.max[axis]) * 0.5
        this.minimum[axis] = metadata.min[axis] - this.centre[axis]
        this.maximum[axis] = metadata.max[axis] - this.centre[axis]
      }
      this.releaseModel(previous)
      this.failed = false
      this.syncSize()
      this.fit()
      return metadata
    } catch (error) {
      this.staged.delete(model)
      this.releaseModel(model)
      if (this.disposed || generation !== this.generation || !isCurrent())
        throw new Error("Cancelled")
      const failure = errorOf(error)
      if (failure.message !== "Cancelled") this.onError(failure.message)
      throw failure
    }
  }

  private async appendModel(
    model: Model,
    metadata: PreviewMetadata,
    readBuffer: ReadBuffer,
    guard: () => void,
    reportUploaded: (bytes: number) => void,
    threadCount: number | null,
    speedFirst: boolean,
    pool?: PreparationWorkerPool,
  ): Promise<void> {
    const layout = buildUploadLayout(metadata, threadCount !== null)
    const pages = layout.pages[Symbol.iterator]()
    const arenaBuffers: (WebGLBuffer[] | undefined)[] = []
    const firstTexture = model.textures.length
    const gl = this.gl
    const budget: UploadBudget = { bytes: 0, started: performance.now() }
    let preparation: PreparedUploads | null = null
    try {
      guard()
      if (threadCount !== null) {
        preparation = new PreparedUploads(
          pages,
          (page) => readBuffer(page.slices),
          threadCount,
          speedFirst,
          guard,
          pool ? (onError) => pool.acquire(onError) : createPreparationWorker,
          SchematicRenderer.pendingPreparationReads,
        )
        this.preparation = preparation
        SchematicRenderer.pendingPreparationReads = preparation.drained
      } else {
        await SchematicRenderer.pendingPreparationReads
        guard()
      }
      while (true) {
        let page: UploadPage
        let buffer: ArrayBuffer
        if (preparation) {
          const prepared = await preparation.take()
          if (!prepared) break
          ;({ page, buffer } = prepared)
        } else {
          const next = pages.next()
          if (next.done) break
          page = next.value
          const pendingRead = readBuffer(page.slices)
          SchematicRenderer.pendingPreparationReads = pendingRead.then(
            () => {},
            () => {},
          )
          buffer = await pendingRead
          guard()
          validateUpload(buffer, page)
        }
        for (const slice of page.slices) {
          guard()
          const target = slice.target
          if (target.kind === "texture") {
            const source = metadata.textures[target.textureIndex]
            let texture = model.textures[firstTexture + target.textureIndex]
            if (!texture) {
              texture = required(gl.createTexture(), "方块纹理")
              model.textures.push(texture)
              gl.activeTexture(gl.TEXTURE0)
              gl.bindTexture(gl.TEXTURE_2D, texture)
              gl.texStorage2D(gl.TEXTURE_2D, 1, gl.SRGB8_ALPHA8, source.width, source.height)
              gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST)
              gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST)
              const wrap = source.repeat ? gl.REPEAT : gl.CLAMP_TO_EDGE
              gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, wrap)
              gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, wrap)
              gl.pixelStorei(gl.UNPACK_ALIGNMENT, 4)
              gl.pixelStorei(gl.UNPACK_FLIP_Y_WEBGL, false)
              gl.pixelStorei(gl.UNPACK_PREMULTIPLY_ALPHA_WEBGL, false)
              gl.pixelStorei(gl.UNPACK_COLORSPACE_CONVERSION_WEBGL, gl.NONE)
              this.checkGraphics("allocate a block texture")
              await this.checkpoint(budget, 0, guard)
            }
            const pixels = new Uint8Array(buffer, slice.pageOffset, slice.length)
            gl.activeTexture(gl.TEXTURE0)
            gl.bindTexture(gl.TEXTURE_2D, texture)
            gl.texSubImage2D(
              gl.TEXTURE_2D,
              0,
              target.x,
              target.y,
              target.width,
              target.height,
              gl.RGBA,
              gl.UNSIGNED_BYTE,
              pixels,
            )
            this.checkGraphics("upload a block texture")
          } else {
            const partLayout = layout.parts[target.partIndex]
            let buffers = arenaBuffers[partLayout.arenaIndex]
            if (!buffers) {
              buffers = []
              arenaBuffers[partLayout.arenaIndex] = buffers
              for (let attribute = 0; attribute < 5; attribute++) {
                gl.bindVertexArray(null)
                guard()
                const gpu = required(gl.createBuffer(), "网格缓冲区")
                buffers.push(gpu)
                model.buffers.push(gpu)
                const bindTarget = attribute === 4 ? gl.ELEMENT_ARRAY_BUFFER : gl.ARRAY_BUFFER
                gl.bindBuffer(bindTarget, gpu)
                gl.bufferData(
                  bindTarget,
                  layout.arenas[partLayout.arenaIndex].byteLengths[attribute],
                  gl.STATIC_DRAW,
                )
                this.checkGraphics("allocate a mesh buffer")
                await this.checkpoint(budget, 0, guard)
              }
            }
            let part = model.parts[model.parts.length - 1]
            const expected = metadata.parts[target.partIndex]
            if (target.attribute === 0 && slice.offset === 0) {
              part = {
                vao: required(gl.createVertexArray(), "网格顶点数组"),
                indexByteOffset: partLayout.offsets[4],
                texture: model.textures[expected.textureIndex],
                count: expected.indexCount,
                alphaMode: expected.alphaMode,
              }
              model.parts.push(part)
              gl.bindVertexArray(part.vao)
              for (let attribute = 0; attribute < 4; attribute++) {
                const format = BUFFER_FORMATS[attribute]
                const type =
                  attribute === 1 ? gl.BYTE : attribute === 3 ? gl.UNSIGNED_BYTE : gl.FLOAT
                gl.bindBuffer(gl.ARRAY_BUFFER, buffers[attribute])
                gl.enableVertexAttribArray(attribute)
                gl.vertexAttribPointer(
                  attribute,
                  format.size,
                  type,
                  format.normalized,
                  0,
                  partLayout.offsets[attribute],
                )
              }
              gl.bindBuffer(gl.ELEMENT_ARRAY_BUFFER, buffers[4])
              this.checkGraphics("configure a mesh vertex array")
            }
            const attribute = target.attribute
            const bindTarget = attribute === 4 ? gl.ELEMENT_ARRAY_BUFFER : gl.ARRAY_BUFFER
            const format = BUFFER_FORMATS[attribute]
            const values = new format.array(
              buffer,
              slice.pageOffset,
              slice.length / format.array.BYTES_PER_ELEMENT,
            )
            gl.bindVertexArray(part.vao)
            gl.bindBuffer(bindTarget, buffers[attribute])
            gl.bufferSubData(bindTarget, target.gpuOffset, values)
            this.checkGraphics("upload a mesh buffer")
          }
          reportUploaded(slice.length)
          await this.checkpoint(budget, slice.length, guard)
        }
        preparation?.release()
      }
      guard()
    } finally {
      preparation?.stop()
      if (this.preparation === preparation) this.preparation = null
    }
    if (preparation) await preparation.drained
    guard()
  }

  clear(): void {
    if (this.disposed) return
    this.generation++
    this.cancelStaged()
    this.releaseModel(this.model)
    this.model = null
    this.failed = false
    this.invalidate()
  }

  fit(): void {
    if (!this.model || this.disposed) return
    this.yaw = Math.PI / 4
    this.pitch = Math.atan(1 / Math.sqrt(2))
    this.target.fill(0)
    this.fittedDistance = this.distance = this.fittingDistance()
    this.invalidate()
  }

  zoom(factor: number): void {
    if (!this.model || this.disposed || !Number.isFinite(factor) || factor <= 0) return
    this.distance = Math.min(
      this.fittedDistance * 8,
      Math.max(this.fittedDistance * 0.05, this.distance * factor),
    )
    this.invalidate()
  }

  setGrid(visible: boolean): void {
    if (this.gridVisible === visible || this.disposed) return
    this.gridVisible = visible
    this.invalidate()
  }

  dispose(): void {
    if (this.disposed) return
    this.disposed = true
    this.generation++
    this.cancelFrame()
    this.endPointer()
    this.observer.disconnect()
    this.dprQuery?.removeEventListener("change", this.onDprChange)
    window.removeEventListener("resize", this.onResize)
    const canvas = this.canvas
    canvas.removeEventListener("pointerdown", this.onPointerDown)
    canvas.removeEventListener("pointermove", this.onPointerMove)
    canvas.removeEventListener("pointerup", this.onPointerEnd)
    canvas.removeEventListener("pointercancel", this.onPointerEnd)
    canvas.removeEventListener("lostpointercapture", this.onPointerEnd)
    canvas.removeEventListener("wheel", this.onWheel)
    canvas.removeEventListener("keydown", this.onKeyDown)
    canvas.removeEventListener("contextmenu", this.onContextMenu)
    canvas.removeEventListener("webglcontextlost", this.onContextLost)
    canvas.removeEventListener("webglcontextrestored", this.onContextRestored)
    canvas.style.touchAction = this.originalTouchAction
    if (this.originalTabIndex === null) canvas.removeAttribute("tabindex")
    else canvas.setAttribute("tabindex", this.originalTabIndex)
    this.cancelStaged()
    this.releaseModel(this.model)
    this.model = null
    this.releaseTargets(this.targets)
    this.targets = null
    this.releasePipeline()
    this.yieldChannel.port1.onmessage = null
    this.yieldChannel.port1.close()
    this.yieldChannel.port2.close()
  }

  private initialize(): void {
    const gl = this.gl
    let scene: WebGLProgram | null = null
    let output: WebGLProgram | null = null
    let fullscreen: WebGLVertexArrayObject | null = null
    try {
      scene = program(gl, VERTEX_SOURCE, FRAGMENT_SOURCE)
      output = program(gl, OUTPUT_VERTEX_SOURCE, OUTPUT_FRAGMENT_SOURCE)
      fullscreen = required(gl.createVertexArray(), "输出顶点数组")
      const uniform = (name: string) => {
        const location = gl.getUniformLocation(scene!, name)
        if (location === null)
          throw new Error(`图形着色器缺少 ${name} uniform。`)
        return location
      }
      const pipeline: Pipeline = {
        scene,
        output,
        fullscreen,
        matrix: uniform("mvp"),
        offset: uniform("offset"),
        alphaMode: uniform("alphaMode"),
        grid: uniform("isGrid"),
      }
      gl.useProgram(scene)
      gl.uniform1i(gl.getUniformLocation(scene, "blockTexture"), 0)
      gl.useProgram(output)
      gl.uniform1i(gl.getUniformLocation(output, "linearFrame"), 0)
      gl.frontFace(gl.CCW)
      gl.cullFace(gl.BACK)
      gl.depthFunc(gl.LESS)
      this.maxTextureSize = gl.getParameter(gl.MAX_TEXTURE_SIZE) as number
      const renderSize = gl.getParameter(gl.MAX_RENDERBUFFER_SIZE) as number
      const viewport = gl.getParameter(gl.MAX_VIEWPORT_DIMS) as Int32Array
      this.maxWidth = Math.min(this.maxTextureSize, renderSize, viewport[0])
      this.maxHeight = Math.min(this.maxTextureSize, renderSize, viewport[1])
      const colorSamples = gl.getInternalformatParameter(
        gl.RENDERBUFFER,
        gl.SRGB8_ALPHA8,
        gl.SAMPLES,
      ) as Int32Array
      const depthSamples = gl.getInternalformatParameter(
        gl.RENDERBUFFER,
        gl.DEPTH_COMPONENT24,
        gl.SAMPLES,
      ) as Int32Array
      this.samples = 1
      for (const count of colorSamples) {
        if (count <= 4 && count > this.samples && depthSamples.includes(count)) this.samples = count
      }
      this.checkGraphics("初始化 WebGL 2")
      this.pipeline = pipeline
    } catch (error) {
      if (scene) gl.deleteProgram(scene)
      if (output) gl.deleteProgram(output)
      if (fullscreen) gl.deleteVertexArray(fullscreen)
      throw error
    }
  }

  private async checkpoint(budget: UploadBudget, bytes: number, guard: () => void): Promise<void> {
    guard()
    budget.bytes += bytes
    if (budget.bytes < 4 * UPLOAD_CHUNK && performance.now() - budget.started < 6) return
    this.checkGraphics("上传该投影")
    this.gl.bindVertexArray(null)
    await new Promise<void>((resolve) => {
      this.pendingYields.push(resolve)
      this.yieldChannel.port2.postMessage(null)
    })
    guard()
    budget.bytes = 0
    budget.started = performance.now()
  }

  private buildGrid(model: Model, metadata: PreviewMetadata): void {
    const extent = Math.max(
      Math.max(metadata.max[0] - metadata.min[0], metadata.max[2] - metadata.min[2]) * 1.15,
      16,
    )
    const step = Math.max(1, Math.ceil(extent / 128))
    const halfSteps = Math.ceil(extent / (2 * step))
    const edge = halfSteps * step
    const y = (metadata.min[1] - metadata.max[1]) * 0.5 - 0.02
    const lines = new Float32Array((halfSteps * 2 + 1) * 12)
    let at = 0
    for (let line = -halfSteps; line <= halfSteps; line++) {
      const offset = line * step
      lines[at++] = offset
      lines[at++] = y
      lines[at++] = -edge
      lines[at++] = offset
      lines[at++] = y
      lines[at++] = edge
      lines[at++] = -edge
      lines[at++] = y
      lines[at++] = offset
      lines[at++] = edge
      lines[at++] = y
      lines[at++] = offset
    }
    const gl = this.gl
    model.gridCount = lines.length / 3
    model.gridVao = required(gl.createVertexArray(), "网格线顶点数组")
    model.gridBuffer = required(gl.createBuffer(), "网格线缓冲区")
    gl.bindVertexArray(model.gridVao)
    gl.bindBuffer(gl.ARRAY_BUFFER, model.gridBuffer)
    gl.bufferData(gl.ARRAY_BUFFER, lines, gl.STATIC_DRAW)
    gl.enableVertexAttribArray(0)
    gl.vertexAttribPointer(0, 3, gl.FLOAT, false, 0, 0)
  }

  private basis(): void {
    const cp = Math.cos(this.pitch)
    vec3.set(this.direction, cp * Math.sin(this.yaw), Math.sin(this.pitch), cp * Math.cos(this.yaw))
    vec3.negate(this.forward, this.direction)
    vec3.cross(this.right, this.forward, UP)
    vec3.normalize(this.right, this.right)
    vec3.cross(this.up, this.right, this.forward)
  }

  private fittingDistance(): number {
    this.basis()
    const vertical = HALF_FOV_TAN / 1.18
    const horizontal = vertical * this.aspect
    let fit = 0.1
    for (let corner = 0; corner < 8; corner++) {
      const x = (corner & 1) === 0 ? this.minimum[0] : this.maximum[0]
      const y = (corner & 2) === 0 ? this.minimum[1] : this.maximum[1]
      const z = (corner & 4) === 0 ? this.minimum[2] : this.maximum[2]
      const depth = x * this.forward[0] + y * this.forward[1] + z * this.forward[2]
      const across = x * this.right[0] + y * this.right[1] + z * this.right[2]
      const above = x * this.up[0] + y * this.up[1] + z * this.up[2]
      fit = Math.max(fit, Math.abs(above) / vertical - depth, Math.abs(across) / horizontal - depth)
    }
    return fit
  }

  private pan(dx: number, dy: number): void {
    this.basis()
    const scale = (2 * this.distance * HALF_FOV_TAN) / this.cssHeight
    vec3.scaleAndAdd(this.target, this.target, this.right, -dx * scale)
    vec3.scaleAndAdd(this.target, this.target, this.up, dy * scale)
    this.invalidate()
  }

  private orbit(dx: number, dy: number): void {
    this.yaw = (this.yaw - dx * 0.008) % (Math.PI * 2)
    this.pitch = Math.max(-1.5, Math.min(1.5, this.pitch + dy * 0.008))
    this.invalidate()
  }

  private syncSize(): boolean {
    const width = this.canvas.clientWidth
    const height = this.canvas.clientHeight
    if (width <= 0 || height <= 0) return false
    this.cssHeight = height
    const dpr = Math.max(0.25, Math.min(window.devicePixelRatio || 1, 2))
    const scale = Math.min(
      dpr,
      this.maxWidth / width,
      this.maxHeight / height,
      Math.sqrt(MAX_RENDER_PIXELS / width / height),
    )
    const pixelsX = Math.max(1, Math.floor(width * scale))
    const pixelsY = Math.max(1, Math.floor(height * scale))
    const aspect = pixelsX / pixelsY
    if (aspect !== this.aspect) {
      this.aspect = aspect
      if (this.model) {
        const ratio = this.distance / this.fittedDistance
        this.fittedDistance = this.fittingDistance()
        this.distance = this.fittedDistance * ratio
      }
    }
    if (this.canvas.width !== pixelsX) this.canvas.width = pixelsX
    if (this.canvas.height !== pixelsY) this.canvas.height = pixelsY
    return true
  }

  // An sRGB attachment blends in linear space while preserving dark-color
  // precision. Sampling it decodes to linear before the output shader encodes
  // for the canvas; encoding in the scene shader would blend in the wrong space.
  private createTargets(width: number, height: number): Targets {
    const gl = this.gl
    const targets: Targets = {
      width,
      height,
      color: null,
      resolve: null,
      draw: null,
      depth: null,
      multisampleColor: null,
    }
    try {
      targets.color = required(gl.createTexture(), "线性颜色纹理")
      gl.activeTexture(gl.TEXTURE0)
      gl.bindTexture(gl.TEXTURE_2D, targets.color)
      gl.texStorage2D(gl.TEXTURE_2D, 1, gl.SRGB8_ALPHA8, width, height)
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MIN_FILTER, gl.NEAREST)
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_MAG_FILTER, gl.NEAREST)
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_S, gl.CLAMP_TO_EDGE)
      gl.texParameteri(gl.TEXTURE_2D, gl.TEXTURE_WRAP_T, gl.CLAMP_TO_EDGE)
      targets.resolve = required(gl.createFramebuffer(), "颜色帧缓冲")
      gl.bindFramebuffer(gl.FRAMEBUFFER, targets.resolve)
      gl.framebufferTexture2D(gl.FRAMEBUFFER, gl.COLOR_ATTACHMENT0, gl.TEXTURE_2D, targets.color, 0)
      if (gl.checkFramebufferStatus(gl.FRAMEBUFFER) !== gl.FRAMEBUFFER_COMPLETE)
        throw new Error("图形设备无法创建线性颜色帧缓冲。")
      targets.draw = targets.resolve
      targets.depth = required(gl.createRenderbuffer(), "深度缓冲")
      gl.bindRenderbuffer(gl.RENDERBUFFER, targets.depth)
      if (this.samples > 1) {
        gl.renderbufferStorageMultisample(
          gl.RENDERBUFFER,
          this.samples,
          gl.DEPTH_COMPONENT24,
          width,
          height,
        )
        targets.draw = required(gl.createFramebuffer(), "多重采样帧缓冲")
        gl.bindFramebuffer(gl.FRAMEBUFFER, targets.draw)
        targets.multisampleColor = required(gl.createRenderbuffer(), "多重采样颜色纹理")
        gl.bindRenderbuffer(gl.RENDERBUFFER, targets.multisampleColor)
        gl.renderbufferStorageMultisample(
          gl.RENDERBUFFER,
          this.samples,
          gl.SRGB8_ALPHA8,
          width,
          height,
        )
        gl.framebufferRenderbuffer(
          gl.FRAMEBUFFER,
          gl.COLOR_ATTACHMENT0,
          gl.RENDERBUFFER,
          targets.multisampleColor,
        )
      } else {
        gl.renderbufferStorage(gl.RENDERBUFFER, gl.DEPTH_COMPONENT24, width, height)
      }
      gl.framebufferRenderbuffer(
        gl.FRAMEBUFFER,
        gl.DEPTH_ATTACHMENT,
        gl.RENDERBUFFER,
        targets.depth,
      )
      this.checkGraphics("分配绘制表面")
      if (gl.checkFramebufferStatus(gl.FRAMEBUFFER) !== gl.FRAMEBUFFER_COMPLETE)
        throw new Error("图形设备无法创建投影绘制表面。")
      return targets
    } catch (error) {
      this.releaseTargets(targets)
      throw error
    }
  }

  private readonly drawFrame = (): void => {
    this.frame = 0
    if (this.disposed || this.contextLost || this.failed || !this.pipeline) return
    const gl = this.gl
    try {
      if (!this.syncSize()) return
      const width = this.canvas.width
      const height = this.canvas.height
      if (!this.targets || this.targets.width !== width || this.targets.height !== height) {
        this.releaseTargets(this.targets)
        this.targets = null
        this.targets = this.createTargets(width, height)
      }
      const targets = this.targets
      const pipeline = this.pipeline
      gl.bindFramebuffer(gl.FRAMEBUFFER, targets.draw)
      gl.viewport(0, 0, width, height)
      gl.enable(gl.DEPTH_TEST)
      gl.depthMask(true)
      gl.disable(gl.BLEND)
      gl.disable(gl.CULL_FACE)
      gl.clearColor(0.00335, 0.00518, 0.00802, 1)
      gl.clear(gl.COLOR_BUFFER_BIT | gl.DEPTH_BUFFER_BIT)
      if (this.model) {
        this.basis()
        vec3.scaleAndAdd(this.eye, this.target, this.direction, this.distance)
        mat4.lookAt(this.view, this.eye, this.target, UP)
        mat4.perspective(
          this.projection,
          FOV,
          this.aspect,
          Math.max(this.fittedDistance / 1000, 0.01),
          this.fittedDistance * 24,
        )
        mat4.multiply(this.mvp, this.projection, this.view)
        gl.useProgram(pipeline.scene)
        gl.uniformMatrix4fv(pipeline.matrix, false, this.mvp)
        gl.activeTexture(gl.TEXTURE0)
        // The grid shader does not sample, but a non-feedback sampler binding is
        // still needed when the resolved color texture was bound by the last frame.
        gl.bindTexture(gl.TEXTURE_2D, this.model.textures[0])
        if (this.gridVisible) {
          gl.uniform1i(pipeline.grid, 1)
          gl.uniform3f(pipeline.offset, 0, 0, 0)
          gl.bindVertexArray(this.model.gridVao)
          gl.drawArrays(gl.LINES, 0, this.model.gridCount)
        }
        gl.uniform1i(pipeline.grid, 0)
        gl.uniform3f(pipeline.offset, -this.centre[0], -this.centre[1], -this.centre[2])
        for (const part of this.model.parts) if (part.alphaMode !== 2) this.drawPart(part, pipeline)
        gl.enable(gl.BLEND)
        gl.blendFunc(gl.SRC_ALPHA, gl.ONE_MINUS_SRC_ALPHA)
        gl.depthMask(false)
        for (const part of this.model.parts) if (part.alphaMode === 2) this.drawPart(part, pipeline)
        gl.depthMask(true)
        gl.disable(gl.BLEND)
      }
      if (targets.draw !== targets.resolve) {
        gl.bindFramebuffer(gl.READ_FRAMEBUFFER, targets.draw)
        gl.bindFramebuffer(gl.DRAW_FRAMEBUFFER, targets.resolve)
        gl.blitFramebuffer(
          0,
          0,
          width,
          height,
          0,
          0,
          width,
          height,
          gl.COLOR_BUFFER_BIT,
          gl.NEAREST,
        )
      }
      gl.bindFramebuffer(gl.FRAMEBUFFER, null)
      gl.disable(gl.DEPTH_TEST)
      gl.disable(gl.CULL_FACE)
      gl.useProgram(pipeline.output)
      gl.activeTexture(gl.TEXTURE0)
      gl.bindTexture(gl.TEXTURE_2D, targets.color)
      gl.bindVertexArray(pipeline.fullscreen)
      gl.drawArrays(gl.TRIANGLES, 0, 3)
      gl.bindVertexArray(null)
      this.checkGraphics("绘制该投影")
    } catch (error) {
      this.failed = true
      this.onError(errorOf(error).message)
    }
  }

  private drawPart(part: Part, pipeline: Pipeline): void {
    const gl = this.gl
    if (part.alphaMode === 0) gl.enable(gl.CULL_FACE)
    else gl.disable(gl.CULL_FACE)
    gl.uniform1i(pipeline.alphaMode, part.alphaMode)
    gl.bindTexture(gl.TEXTURE_2D, part.texture)
    gl.bindVertexArray(part.vao)
    gl.drawElements(gl.TRIANGLES, part.count, gl.UNSIGNED_INT, part.indexByteOffset)
  }

  private invalidate(): void {
    if (!this.frame && !this.disposed && !this.contextLost && !this.failed)
      this.frame = requestAnimationFrame(this.drawFrame)
  }

  private cancelFrame(): void {
    if (this.frame) cancelAnimationFrame(this.frame)
    this.frame = 0
  }

  private checkGraphics(action: string): void {
    const gl = this.gl
    const code = gl.getError()
    if (code === gl.NO_ERROR) return
    // Drain the finite error flags so a failed upload cannot poison a later load.
    for (let i = 0; i < 8 && gl.getError() !== gl.NO_ERROR; i++) {
      /* drain */
    }
    if (code === gl.CONTEXT_LOST_WEBGL)
      throw new Error(
        "图形上下文已丢失。请在图形设备恢复后重新打开投影文件。",
      )
    throw new Error(
      `图形设备无法${action}（WebGL 错误 0x${code.toString(16)}）。模型或窗口可能超出了可用的显卡内存。`,
    )
  }

  private releaseModel(model: Model | null): void {
    if (!model || model.released) return
    model.released = true
    const gl = this.gl
    for (const part of model.parts) gl.deleteVertexArray(part.vao)
    for (const buffer of model.buffers) gl.deleteBuffer(buffer)
    model.buffers.length = 0
    for (const texture of model.textures) gl.deleteTexture(texture)
    gl.deleteVertexArray(model.gridVao)
    gl.deleteBuffer(model.gridBuffer)
    model.parts.length = 0
    model.textures.length = 0
    model.gridVao = null
    model.gridBuffer = null
  }

  private cancelStaged(): void {
    this.stream?.cancel()
    this.stream = null
    this.preparation?.stop()
    this.preparation = null
    this.preparationPool?.dispose()
    this.preparationPool = null
    for (const model of this.staged) this.releaseModel(model)
    this.staged.clear()
    while (this.pendingYields.length) this.pendingYields.shift()!()
  }

  private releaseTargets(targets: Targets | null): void {
    if (!targets) return
    const gl = this.gl
    if (targets.draw !== targets.resolve) gl.deleteFramebuffer(targets.draw)
    gl.deleteFramebuffer(targets.resolve)
    gl.deleteTexture(targets.color)
    gl.deleteRenderbuffer(targets.depth)
    gl.deleteRenderbuffer(targets.multisampleColor)
  }

  private releasePipeline(): void {
    if (!this.pipeline) return
    this.gl.useProgram(null)
    this.gl.deleteProgram(this.pipeline.scene)
    this.gl.deleteProgram(this.pipeline.output)
    this.gl.deleteVertexArray(this.pipeline.fullscreen)
    this.pipeline = null
  }

  private watchDpr(): void {
    this.dprQuery?.removeEventListener("change", this.onDprChange)
    this.dprQuery = window.matchMedia(`(resolution: ${window.devicePixelRatio || 1}dppx)`)
    this.dprQuery.addEventListener("change", this.onDprChange)
  }

  private readonly onResize = (): void => this.invalidate()
  private readonly onDprChange = (): void => {
    this.watchDpr()
    this.invalidate()
  }
  private readonly onContextMenu = (event: Event): void => event.preventDefault()

  private readonly onPointerDown = (event: PointerEvent): void => {
    if (this.pointerId !== null || event.button < 0 || event.button > 2) return
    this.canvas.focus({ preventScroll: true })
    this.pointerId = event.pointerId
    this.pointerButton = event.button
    this.lastX = event.clientX
    this.lastY = event.clientY
    this.canvas.setPointerCapture(event.pointerId)
    event.preventDefault()
  }

  private readonly onPointerMove = (event: PointerEvent): void => {
    if (event.pointerId !== this.pointerId) return
    const dx = event.clientX - this.lastX
    const dy = event.clientY - this.lastY
    this.lastX = event.clientX
    this.lastY = event.clientY
    if (!this.model || (dx === 0 && dy === 0)) return
    if (this.pointerButton === 0) this.orbit(dx, dy)
    else this.pan(dx, dy)
  }

  private endPointer(): void {
    const pointer = this.pointerId
    this.pointerId = null
    if (pointer !== null && this.canvas.hasPointerCapture(pointer))
      this.canvas.releasePointerCapture(pointer)
  }

  private readonly onPointerEnd = (event: PointerEvent): void => {
    if (event.pointerId === this.pointerId) this.endPointer()
  }

  private readonly onWheel = (event: WheelEvent): void => {
    if (!this.model) return
    event.preventDefault()
    const unit = event.deltaMode === 1 ? 16 : event.deltaMode === 2 ? this.cssHeight : 1
    this.zoom(Math.exp(Math.max(-1200, Math.min(1200, event.deltaY * unit)) * 0.001))
  }

  private readonly onKeyDown = (event: KeyboardEvent): void => {
    if (!this.model || event.ctrlKey || event.altKey || event.metaKey) return
    let dx = 0
    let dy = 0
    switch (event.key) {
      case "ArrowLeft":
        dx = -18
        break
      case "ArrowRight":
        dx = 18
        break
      case "ArrowUp":
        dy = -18
        break
      case "ArrowDown":
        dy = 18
        break
      case "+":
      case "=":
        this.zoom(0.88)
        break
      case "-":
      case "_":
        this.zoom(1.12)
        break
      case "f":
      case "F":
      case "Home":
        this.fit()
        break
      default:
        return
    }
    event.preventDefault()
    if (dx !== 0 || dy !== 0) {
      if (event.shiftKey) this.pan(dx, dy)
      else this.orbit(dx, dy)
    }
  }

  private readonly onContextLost = (event: Event): void => {
    event.preventDefault()
    this.contextLost = true
    this.generation++
    this.cancelFrame()
    this.cancelStaged()
    this.releaseModel(this.model)
    this.model = null
    this.releaseTargets(this.targets)
    this.targets = null
    this.releasePipeline()
    this.onError(
      "图形上下文已丢失。请在图形设备恢复后重新打开投影文件。",
    )
  }

  private readonly onContextRestored = (): void => {
    if (this.disposed) return
    this.contextLost = false
    this.failed = false
    try {
      this.initialize()
      this.invalidate()
      this.onError("图形设备已恢复。重新打开投影文件即可恢复预览。")
    } catch (error) {
      this.failed = true
      this.onError(errorOf(error).message)
    }
  }
}
