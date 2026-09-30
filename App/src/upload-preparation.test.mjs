import assert from "node:assert/strict"
import { test } from "vite-plus/test"
import { PreparedUploads, PreparationWorkerPool, validateUpload } from "./upload-preparation.ts"
import { UPLOAD_CHUNK } from "./upload-layout.ts"

function deferred() {
  let resolve
  let reject
  const promise = new Promise((yes, no) => {
    resolve = yes
    reject = no
  })
  return { promise, resolve, reject }
}

const turn = () => new Promise((resolve) => setImmediate(resolve))
const pages = (count, length = UPLOAD_CHUNK) =>
  Array.from({ length: count }, (_, bufferId) => ({
    byteLength: length,
    payloadBytes: length,
    slices: [{ bufferId, offset: 0, length, pageOffset: 0, validation: "none" }],
  }))

function controlledWorkers() {
  const workers = []
  const pending = new Map()
  const create = () => {
    const worker = {
      terminated: false,
      prepare(buffer, page) {
        const work = deferred()
        const transferred = structuredClone(buffer, { transfer: [buffer] })
        assert.equal(buffer.byteLength, 0)
        pending.set(page.slices[0].bufferId, {
          work,
          finish: () => work.resolve(structuredClone(transferred, { transfer: [transferred] })),
        })
        return work.promise
      },
      terminate() {
        worker.terminated = true
      },
    }
    workers.push(worker)
    return worker
  }
  return { create, workers, pending }
}

for (const speedFirst of [false, true]) {
  const mode = speedFirst ? "speed-first" : "memory-first"
  test(`${mode} prefetch remains bounded until ordered uploads release their slots`, async () => {
    const parts = pages(11)
    const workers = controlledWorkers()
    const reads = []
    const limit = speedFirst ? 8 : 4
    const initialReads = Array.from({ length: limit }, (_, id) => id)
    const preparation = new PreparedUploads(
      parts.values(),
      async (page) => {
        reads.push(page.slices[0].bufferId)
        const buffer = new ArrayBuffer(page.byteLength)
        new Uint8Array(buffer)[0] = page.slices[0].bufferId
        return buffer
      },
      8,
      speedFirst,
      () => {},
      workers.create,
    )
    try {
      await turn()
      assert.deepEqual(reads, initialReads)
      assert.equal(workers.workers.length, limit)
      for (let id = limit - 1; id > 0; id--) workers.pending.get(id).finish()
      let ready = false
      const first = preparation.take().then((result) => {
        ready = true
        return result
      })
      await turn()
      assert.equal(ready, false)
      assert.deepEqual(reads, initialReads)
      workers.pending.get(0).finish()
      const firstPage = await first
      assert.equal(firstPage.page, parts[0])
      assert.equal(firstPage.buffer.byteLength, UPLOAD_CHUNK)
      assert.equal(new Uint8Array(firstPage.buffer)[0], 0)
      await turn()
      assert.deepEqual(reads, initialReads)
      await assert.rejects(preparation.take(), /预览页面尚未上传/)
      preparation.release()
      await turn()
      assert.deepEqual(reads, [...initialReads, limit])
      assert.equal(
        firstPage.buffer.byteLength,
        UPLOAD_CHUNK,
        "upload storage is never retransferred",
      )
      for (let id = 1; id < parts.length; id++) {
        if (id >= limit) workers.pending.get(id).finish()
        const result = await preparation.take()
        assert.equal(result.page, parts[id])
        assert.equal(new Uint8Array(result.buffer)[0], id)
        preparation.release()
        await turn()
      }
      assert.deepEqual(
        reads,
        parts.map((page) => page.slices[0].bufferId),
      )
      assert.equal(workers.workers.length, limit)
      assert.equal(await preparation.take(), null)
    } finally {
      preparation.stop()
    }
    assert.ok(workers.workers.every((worker) => worker.terminated))
    await preparation.drained
  })
}

test("cancellation drops late reads and bounds replacement generations", async () => {
  const firstReads = []
  const firstWorkers = controlledWorkers()
  const first = new PreparedUploads(
    pages(5).values(),
    (page) => {
      const work = deferred()
      firstReads.push({ page, work })
      return work.promise
    },
    2,
    true,
    () => {},
    firstWorkers.create,
  )
  await turn()
  const pendingTake = assert.rejects(first.take(), /Cancelled/)
  first.stop()
  await pendingTake
  assert.equal(firstReads.length, 2)
  assert.ok(firstWorkers.workers.every((worker) => worker.terminated))
  const secondReads = []
  const secondWorkers = controlledWorkers()
  const second = new PreparedUploads(
    pages(3).values(),
    async (page) => {
      secondReads.push(page.slices[0].bufferId)
      return new ArrayBuffer(page.byteLength)
    },
    2,
    true,
    () => {},
    secondWorkers.create,
    first.drained,
  )
  const thirdWorkers = controlledWorkers()
  const thirdReads = []
  const secondTake = assert.rejects(second.take(), /Cancelled/)
  second.stop()
  await secondTake
  const third = new PreparedUploads(
    pages(3).values(),
    async (page) => {
      thirdReads.push(page.slices[0].bufferId)
      return new ArrayBuffer(page.byteLength)
    },
    2,
    true,
    () => {},
    thirdWorkers.create,
    second.drained,
  )
  try {
    await turn()
    assert.deepEqual(secondReads, [])
    assert.deepEqual(thirdReads, [])
    firstReads[0].work.resolve(new ArrayBuffer(UPLOAD_CHUNK))
    await turn()
    assert.deepEqual(thirdReads, [])
    firstReads[1].work.reject(new Error("stale IPC failure"))
    await turn()
    assert.deepEqual(thirdReads, [0, 1])
    assert.equal(firstWorkers.pending.size, 0)
    assert.equal(secondWorkers.workers.length, 0)
  } finally {
    third.stop()
  }
  await third.drained
})

test("worker startup failure stops reads before their results arrive", async () => {
  const reads = []
  const failures = []
  const terminated = []
  const preparation = new PreparedUploads(
    pages(5, 4).values(),
    (page) => {
      const work = deferred()
      reads.push({ page, work })
      return work.promise
    },
    2,
    false,
    () => {},
    (onError) => {
      failures.push(onError)
      return {
        prepare: () => {
          throw new Error("Stale data reached a worker")
        },
        terminate: () => terminated.push(true),
      }
    },
  )
  await turn()
  const taking = assert.rejects(preparation.take(), /worker failed to start/)
  failures[1](new Error("worker failed to start"))
  await taking
  assert.equal(terminated.length, 2)
  reads[0].work.resolve(new ArrayBuffer(4))
  reads[1].work.resolve(new ArrayBuffer(4))
  await preparation.drained
  assert.equal(reads.length, 2)
})

test("an out-of-order validation failure cancels the entire window", async () => {
  const workers = controlledWorkers()
  const reads = []
  const preparation = new PreparedUploads(
    pages(6, 4).values(),
    async (page) => {
      reads.push(page.slices[0].bufferId)
      return new ArrayBuffer(page.byteLength)
    },
    2,
    true,
    () => {},
    workers.create,
  )
  await turn()
  const first = assert.rejects(preparation.take(), /invalid coordinates/)
  workers.pending.get(1).work.reject(new Error("invalid coordinates"))
  await first
  workers.pending.get(0).finish()
  await turn()
  assert.deepEqual(reads, [0, 1])
  assert.ok(workers.workers.every((worker) => worker.terminated))
  await assert.rejects(preparation.take(), /invalid coordinates/)
  await preparation.drained
})

test("page validation checks every aligned slice without treating padding as payload", () => {
  const page = {
    slices: [
      { bufferId: 0, offset: 0, length: 3, pageOffset: 0, validation: "none" },
      { bufferId: 1, offset: 0, length: 8, pageOffset: 4, validation: "float32" },
      { bufferId: 2, offset: 0, length: 8, pageOffset: 12, validation: "uint32", vertexCount: 3 },
    ],
    byteLength: 20,
    payloadBytes: 19,
  }
  const buffer = new ArrayBuffer(20)
  new Uint8Array(buffer)[3] = 255
  new Float32Array(buffer, 4, 2).set([1, 2])
  new Uint32Array(buffer, 12, 2).set([0, 2])
  validateUpload(buffer, page)
  new Float32Array(buffer, 4, 2)[1] = Number.NaN
  assert.throws(() => validateUpload(buffer, page), /invalid coordinates/)
  new Float32Array(buffer, 4, 2)[1] = 2
  new Uint32Array(buffer, 12, 2)[1] = 3
  assert.throws(() => validateUpload(buffer, page), /invalid vertex index/)
  new Uint32Array(buffer, 12, 2)[1] = 2
  const indexPage = {
    slices: [page.slices[0], { ...page.slices[2], pageOffset: 4 }],
    byteLength: 12,
    payloadBytes: 11,
  }
  const indexBuffer = new ArrayBuffer(12)
  new Uint32Array(indexBuffer, 4, 2).set([0, 3])
  assert.throws(() => validateUpload(indexBuffer, indexPage), /invalid vertex index/)
  assert.throws(
    () => validateUpload(buffer, { ...page, payloadBytes: 20 }),
    /预览准备页面无效/,
  )
  assert.throws(() => validateUpload(buffer, { ...page, byteLength: 19 }), /incomplete/)
  assert.throws(
    () =>
      validateUpload(buffer, {
        ...page,
        slices: [page.slices[0], { ...page.slices[1], pageOffset: 3 }, page.slices[2]],
      }),
    /预览准备页面无效/,
  )
  assert.throws(
    () =>
      validateUpload(buffer, {
        ...page,
        slices: [page.slices[0], { ...page.slices[1], length: 7 }, page.slices[2]],
      }),
    /预览准备页面无效/,
  )
  assert.throws(
    () => validateUpload(buffer, { ...page, slices: [] }),
    /预览准备页面无效/,
  )
  assert.throws(
    () => validateUpload(buffer, { ...page, slices: Array(257).fill(page.slices[0]) }),
    /预览准备页面无效/,
  )
})

test("empty page iterator starts no workers and drains normally", async () => {
  const preparation = new PreparedUploads(
    [].values(),
    async () => {
      throw new Error("unexpected read")
    },
    4,
    true,
    () => {},
    () => {
      throw new Error("unexpected worker")
    },
  )
  assert.equal(await preparation.take(), null)
  preparation.stop()
  await preparation.drained
})

test("multi-slice page remains held until all its bytes are consumed", async () => {
  const [first, second] = pages(2, 8)
  first.slices = [
    { bufferId: 0, offset: 0, length: 3, pageOffset: 0, validation: "none" },
    { bufferId: 1, offset: 0, length: 4, pageOffset: 4, validation: "uint32", vertexCount: 3 },
  ]
  first.payloadBytes = 7
  const reads = []
  const preparation = new PreparedUploads(
    [first, second, ...pages(2, 8)].values(),
    async (page) => {
      reads.push(page)
      const buffer = new ArrayBuffer(page.byteLength)
      if (page === first) {
        new Uint8Array(buffer).set([10, 11, 12])
        new Uint32Array(buffer, 4, 1)[0] = 2
      }
      return buffer
    },
    2,
    true,
    () => {},
    () => ({
      prepare: async (buffer, page) => {
        const transferred = structuredClone(buffer, { transfer: [buffer] })
        assert.equal(buffer.byteLength, 0)
        validateUpload(transferred, page)
        return structuredClone(transferred, { transfer: [transferred] })
      },
      terminate: () => {},
    }),
  )
  try {
    const held = await preparation.take()
    assert.equal(held.page, first)
    assert.deepEqual(reads, [first, second])
    assert.equal(held.buffer.byteLength, 8)
    assert.deepEqual([...new Uint8Array(held.buffer, 0, 3)], [10, 11, 12])
    assert.equal(new Uint32Array(held.buffer, 4, 1)[0], 2)
    await assert.rejects(preparation.take(), /预览页面尚未上传/)
    assert.deepEqual(reads, [first, second])
    preparation.release()
    await turn()
    assert.equal(reads.length, 3)
  } finally {
    preparation.stop()
  }
  await preparation.drained
})

for (const speedFirst of [false, true]) {
  const mode = speedFirst ? "speed-first" : "memory-first"
  test(`${mode} batches reuse workers without widening the read window`, async () => {
    let created = 0
    let terminated = 0
    let concurrent = 0
    let maximum = 0
    const limit = speedFirst ? 8 : 4
    const pool = new PreparationWorkerPool(() => {
      created++
      return {
        prepare: async (buffer) => buffer,
        terminate: () => {
          terminated++
        },
      }
    })
    let preceding = Promise.resolve()
    try {
      for (let batch = 0; batch < 12; batch++) {
        const preparation = new PreparedUploads(
          pages(10, 4).values(),
          async (page) => {
            concurrent++
            maximum = Math.max(maximum, concurrent)
            await turn()
            concurrent--
            const buffer = new ArrayBuffer(4)
            new Uint32Array(buffer)[0] = batch * 10 + page.slices[0].bufferId
            return buffer
          },
          8,
          speedFirst,
          () => {},
          (onError) => pool.acquire(onError),
          preceding,
        )
        preceding = preparation.drained
        try {
          for (let id = 0; id < 10; id++) {
            const result = await preparation.take()
            assert.equal(new Uint32Array(result.buffer)[0], batch * 10 + id)
            preparation.release()
          }
        } finally {
          preparation.stop()
        }
        await preparation.drained
      }
      assert.equal(created, limit)
      assert.equal(terminated, 0)
      assert.equal(maximum, limit)
    } finally {
      pool.dispose()
    }
    assert.equal(terminated, created)
  })
}

test("a cancelled busy pool lease cannot deliver stale preparation to a later batch", async () => {
  const pending = Promise.withResolvers()
  let created = 0
  let terminated = 0
  const pool = new PreparationWorkerPool(() => {
    const id = ++created
    return {
      prepare: async (buffer) => (id === 1 ? pending.promise : buffer),
      terminate: () => {
        terminated++
      },
    }
  })
  try {
    const first = pool.acquire(() => {})
    const stale = first.prepare(new ArrayBuffer(4), pages(1, 4)[0])
    first.terminate()
    const second = pool.acquire(() => {})
    const buffer = await second.prepare(new ArrayBuffer(8), pages(1, 8)[0])
    pending.resolve(new ArrayBuffer(4))
    await stale
    assert.equal(buffer.byteLength, 8)
    assert.equal(created, 2)
    assert.equal(terminated, 1)
    second.terminate()
  } finally {
    pool.dispose()
  }
  assert.equal(terminated, 2)
})
