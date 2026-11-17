// Voice prompts on this device until they are transcribed: each take's record and its audio
// chunks (a second each) in IndexedDB, written as they arrive. A failed transcription, a closed
// app or a locked phone loses nothing: the take stays here until its text is delivered or it is
// discarded, and the next app start offers it again (providers/VoiceProvider.tsx).

export interface VoiceTake {
  id: string
  startedAt: number
  mimeType: string
  /** Words the transcriber should expect (session names, repo). */
  hint: string
  /** Where it was recorded ("Operator", "New session"): shown when it is offered again. */
  label: string
  /** Set when recording stopped. */
  durationMs?: number
  /** Last sign of life from the tab recording it; a stale take is offered again. */
  heartbeat: number
  /** The last transcription failure. */
  error?: string
}

interface Chunk {
  id: string
  i: number
  blob: Blob
}

const DB = 'fleet-voice'
const TAKES = 'takes'
const CHUNKS = 'chunks'

let dbp: Promise<IDBDatabase> | null = null

function db(): Promise<IDBDatabase> {
  if (!dbp) {
    dbp = new Promise((resolve, reject) => {
      if (typeof indexedDB === 'undefined') return reject(new Error('IndexedDB is unavailable'))
      const req = indexedDB.open(DB, 1)
      req.onupgradeneeded = () => {
        const d = req.result
        d.createObjectStore(TAKES, { keyPath: 'id' })
        d.createObjectStore(CHUNKS, { keyPath: ['id', 'i'] })
      }
      req.onsuccess = () => resolve(req.result)
      req.onerror = () => reject(req.error ?? new Error('IndexedDB open failed'))
    })
    dbp.catch(() => {
      dbp = null
    })
  }
  return dbp
}

function done<T>(req: IDBRequest<T>): Promise<T> {
  return new Promise((resolve, reject) => {
    req.onsuccess = () => resolve(req.result)
    req.onerror = () => reject(req.error)
  })
}

async function tx<T>(stores: string[], mode: IDBTransactionMode, fn: (t: IDBTransaction) => Promise<T> | T): Promise<T> {
  const d = await db()
  const t = d.transaction(stores, mode)
  const finished = new Promise<void>((resolve, reject) => {
    t.oncomplete = () => resolve()
    t.onerror = () => reject(t.error)
    t.onabort = () => reject(t.error ?? new Error('transaction aborted'))
  })
  const out = await fn(t)
  await finished
  return out
}

const range = (id: string) => IDBKeyRange.bound([id, -Infinity], [id, Infinity])

export const voiceStore = {
  put: (t: VoiceTake) => tx([TAKES], 'readwrite', (x) => done(x.objectStore(TAKES).put(t)).then(() => undefined)),
  all: () => tx([TAKES], 'readonly', (x) => done(x.objectStore(TAKES).getAll()) as Promise<VoiceTake[]>),
  /** Merge `patch` into a stored take (no-op when it is gone). */
  update: (id: string, patch: Partial<VoiceTake>) =>
    tx([TAKES], 'readwrite', async (x) => {
      const s = x.objectStore(TAKES)
      const cur = (await done(s.get(id))) as VoiceTake | undefined
      if (cur) await done(s.put({ ...cur, ...patch }))
    }),
  putChunk: (id: string, i: number, blob: Blob) => tx([CHUNKS], 'readwrite', (x) => done(x.objectStore(CHUNKS).put({ id, i, blob } satisfies Chunk)).then(() => undefined)),
  /** The take's audio, chunks in order (an empty Blob when there are none). */
  blob: (id: string, type: string) =>
    tx([CHUNKS], 'readonly', async (x) => {
      const chunks = (await done(x.objectStore(CHUNKS).getAll(range(id)))) as Chunk[]
      return new Blob(
        chunks.sort((a, b) => a.i - b.i).map((c) => c.blob),
        { type },
      )
    }),
  /** The record and all its audio. */
  drop: (id: string) =>
    tx([TAKES, CHUNKS], 'readwrite', async (x) => {
      await done(x.objectStore(CHUNKS).delete(range(id)))
      await done(x.objectStore(TAKES).delete(id))
    }),
}
