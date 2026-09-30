import { MAX_READ_RANGES, UPLOAD_CHUNK, type UploadPage } from "./upload-layout"
const PREPARATION_BYTES = 4 * UPLOAD_CHUNK

export type PreparationWorker = {
  prepare: (buffer: ArrayBuffer, page: UploadPage) => Promise<ArrayBuffer>
  terminate: () => void
}

export type PreparedPage = { page: UploadPage; buffer: ArrayBuffer }

type PooledWorker = {
  worker: PreparationWorker
  onError: ((error: Error) => void) | null
  busy: boolean
  failed: boolean
}

// A batch returns an idle lease; a whole load owns and disposes the actual workers.
// Busy/error leases are never reused, so late replies cannot reach a later batch.
export class PreparationWorkerPool {
  private readonly workers = new Set<PooledWorker>()
  private readonly idle: PooledWorker[] = []
  private disposed = false

  constructor(
    private readonly createWorker: (onError: (error: Error) => void) => PreparationWorker,
  ) {}

  acquire(onError: (error: Error) => void): PreparationWorker {
    if (this.disposed) throw new Error("Cancelled")
    let entry = this.idle.pop()
    while (entry?.failed) {
      this.workers.delete(entry)
      entry.worker.terminate()
      entry = this.idle.pop()
    }
    if (!entry) {
      const created: PooledWorker = {
        worker: this.createWorker((error) => {
          created.failed = true
          created.onError?.(error)
        }),
        onError,
        busy: false,
        failed: false,
      }
      entry = created
      this.workers.add(entry)
    }
    const leased = entry
    leased.onError = onError
    let released = false
    return {
      prepare: async (buffer, page) => {
        if (released || this.disposed) throw new Error("Cancelled")
        leased.busy = true
        try {
          return await leased.worker.prepare(buffer, page)
        } catch (error) {
          leased.failed = true
          throw error
        } finally {
          leased.busy = false
        }
      },
      terminate: () => {
        if (released) return
        released = true
        leased.onError = null
        if (!this.disposed && !leased.busy && !leased.failed) this.idle.push(leased)
        else if (this.workers.delete(leased)) leased.worker.terminate()
      },
    }
  }

  dispose(): void {
    this.disposed = true
    for (const entry of this.workers) {
      entry.onError = null
      entry.worker.terminate()
    }
    this.workers.clear()
    this.idle.length = 0
  }
}

type Result = { buffer: ArrayBuffer } | { error: Error }
type Slot = {
  page: UploadPage
  worker: PreparationWorker
  result: Promise<Result>
  finish: (result: Result) => void
}

const errorOf = (error: unknown): Error =>
  error instanceof Error ? error : new Error(String(error))

// The window includes reads, worker-owned buffers, ready results and the buffer
// currently being uploaded. A slot is not reused until GL has consumed its bytes.
export class PreparedUploads {
  readonly drained: Promise<void>
  private readonly workers: PreparationWorker[] = []
  private readonly slots: Slot[] = []
  private readonly started: Promise<void>
  private readonly finishDrain: () => void
  private readonly cancelled: Promise<void>
  private readonly finishCancellation: () => void
  private reads = 0
  private failure: Error | null = null
  private ended = false
  private held: Slot | null = null
  private readonly pages: Iterator<UploadPage>
  private readonly read: (page: UploadPage) => Promise<ArrayBuffer>
  private readonly guard: () => void
  private readonly createWorker: (onError: (error: Error) => void) => PreparationWorker

  constructor(
    pages: Iterator<UploadPage>,
    read: (page: UploadPage) => Promise<ArrayBuffer>,
    count: number,
    speedFirst: boolean,
    guard: () => void,
    createWorker: (onError: (error: Error) => void) => PreparationWorker,
    preceding: Promise<void> = Promise.resolve(),
  ) {
    if (!Number.isInteger(count) || count < 2 || count > 8)
      throw new Error("预览工作线程数无效。")
    this.pages = pages
    this.read = read
    this.guard = guard
    this.createWorker = createWorker
    let finishDrain!: () => void
    const readsDrained = new Promise<void>((resolve) => {
      finishDrain = resolve
    })
    this.finishDrain = finishDrain
    let finishCancellation!: () => void
    this.cancelled = new Promise<void>((resolve) => {
      finishCancellation = resolve
    })
    this.finishCancellation = finishCancellation
    // A superseded generation cannot let its successor pass earlier live reads.
    this.drained = preceding.then(() => readsDrained)
    this.started = preceding.then(() => {
      if (this.failure) return
      try {
        guard()
        const limit = speedFirst ? count : Math.min(count, PREPARATION_BYTES / UPLOAD_CHUNK)
        for (let index = 0; index < limit && !this.failure; index++) {
          const next = this.pages.next()
          if (next.done) {
            this.ended = true
            break
          }
          const worker = this.createWorker((error) => this.stop(error))
          this.workers.push(worker)
          this.schedule(worker, next.value)
        }
      } catch (error) {
        this.stop(errorOf(error))
      }
    })
  }

  async take(): Promise<PreparedPage | null> {
    await Promise.race([this.started, this.cancelled])
    if (this.failure) throw this.failure
    this.guard()
    if (this.held) throw new Error("上一个预览页面尚未上传。")
    const slot = this.slots[0]
    if (!slot) {
      if (this.ended) return null
      throw new Error("预览上传顺序不一致。")
    }
    const result = await slot.result
    if (this.failure) throw this.failure
    this.guard()
    if ("error" in result) throw result.error
    this.held = slot
    return { page: slot.page, buffer: result.buffer }
  }

  release(): void {
    if (this.failure) throw this.failure
    this.guard()
    const slot = this.held
    if (!slot) throw new Error("当前没有正在上传的预览页面。")
    this.held = null
    this.slots.shift()
    if (!this.ended) {
      try {
        const next = this.pages.next()
        if (next.done) this.ended = true
        else this.schedule(slot.worker, next.value)
      } catch (error) {
        this.stop(errorOf(error))
        throw this.failure
      }
    }
  }

  stop(error = new Error("Cancelled")): void {
    if (this.failure) return
    this.failure = error
    this.finishCancellation()
    for (const slot of this.slots) slot.finish({ error })
    this.slots.length = 0
    this.held = null
    for (const worker of this.workers) worker.terminate()
    this.workers.length = 0
    if (this.reads === 0) this.finishDrain()
  }

  private schedule(worker: PreparationWorker, page: UploadPage): void {
    this.guard()
    if (
      !Number.isSafeInteger(page.byteLength) ||
      page.byteLength <= 0 ||
      page.byteLength > UPLOAD_CHUNK
    )
      throw new Error("预览准备页面无效。")
    let finish!: (result: Result) => void
    const result = new Promise<Result>((resolve) => {
      finish = resolve
    })
    const slot = { page, worker, result, finish }
    this.slots.push(slot)
    this.reads++
    void this.prepare(slot)
  }

  private async prepare(slot: Slot): Promise<void> {
    try {
      let buffer: ArrayBuffer
      try {
        buffer = await this.read(slot.page)
      } finally {
        this.reads--
        if (this.failure && this.reads === 0) this.finishDrain()
      }
      if (this.failure) return
      this.guard()
      if (!(buffer instanceof ArrayBuffer) || buffer.byteLength !== slot.page.byteLength)
        throw new Error("The preview contains an incomplete data page.")
      const prepared = await slot.worker.prepare(buffer, slot.page)
      if (this.failure) return
      this.guard()
      if (!(prepared instanceof ArrayBuffer) || prepared.byteLength !== slot.page.byteLength)
        throw new Error("The preview worker returned an incomplete data page.")
      slot.finish({ buffer: prepared })
    } catch (error) {
      this.stop(errorOf(error))
    }
  }
}

export function validateUpload(buffer: ArrayBuffer, page: UploadPage): void {
  if (!(buffer instanceof ArrayBuffer) || buffer.byteLength !== page.byteLength)
    throw new Error("The preview contains an incomplete data page.")
  if (
    !Array.isArray(page.slices) ||
    page.slices.length === 0 ||
    page.slices.length > MAX_READ_RANGES ||
    !Number.isSafeInteger(page.byteLength) ||
    page.byteLength <= 0 ||
    page.byteLength > UPLOAD_CHUNK ||
    !Number.isSafeInteger(page.payloadBytes)
  )
    throw new Error("预览准备页面无效。")
  let cursor = 0
  let payloadBytes = 0
  for (const slice of page.slices) {
    const expectedOffset = cursor + ((4 - (cursor % 4)) % 4)
    if (
      !Number.isSafeInteger(slice.pageOffset) ||
      slice.pageOffset !== expectedOffset ||
      !Number.isSafeInteger(slice.length) ||
      slice.length <= 0 ||
      slice.length > UPLOAD_CHUNK ||
      slice.pageOffset + slice.length > page.byteLength
    )
      throw new Error("预览准备页面无效。")
    cursor = slice.pageOffset + slice.length
    payloadBytes += slice.length
    if (slice.validation === "float32") {
      if (slice.length % 4 !== 0 || slice.pageOffset % 4 !== 0)
        throw new Error("预览准备页面无效。")
      for (const value of new Float32Array(buffer, slice.pageOffset, slice.length / 4)) {
        if (!Number.isFinite(value)) throw new Error("A preview mesh contains invalid coordinates.")
      }
    } else if (slice.validation === "uint32") {
      if (slice.length % 4 !== 0 || slice.pageOffset % 4 !== 0)
        throw new Error("预览准备页面无效。")
      const vertexCount = slice.vertexCount
      if (vertexCount === undefined || !Number.isSafeInteger(vertexCount) || vertexCount < 0)
        throw new Error("A preview mesh has an invalid vertex count.")
      for (const index of new Uint32Array(buffer, slice.pageOffset, slice.length / 4)) {
        if (index >= vertexCount)
          throw new Error("A preview mesh contains an invalid vertex index.")
      }
    } else if (slice.validation !== "none") {
      throw new Error("预览准备页面无效。")
    }
  }
  if (cursor !== page.byteLength || payloadBytes !== page.payloadBytes)
    throw new Error("预览准备页面无效。")
}
