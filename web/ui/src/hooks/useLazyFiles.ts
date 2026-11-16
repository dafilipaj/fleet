import { useCallback, useEffect, useRef, useState } from 'react'

import { api } from '@/api/client'
import type { DiffFile, DiffRepo } from '@/api/types'

/** Files per request (a rename asks for its old path too: ≤ 40 paths, the server's cap). */
const BATCH = 20
const PARALLEL = 4
/** Bodies kept at most (by the time a session's review has this many, the oldest are long gone). */
const MAX_CACHE = 3000

/** A file's body is the same for the same entry, base and tree: the cache key. */
const bodyKey = (repo: DiffRepo, file: DiffFile) => `${repo.root}\u0000${repo.base?.sha ?? ''}\u0000${repo.tree ?? ''}\u0000${file.path}`

interface Want {
  key: string
  sig?: string
  root: string
  base: string
  tree: string
  paths: string[]
  path: string
}

/**
 * The lines of a lite diff's files, fetched when a card asks (it scrolled near the screen), a
 * few files per request. `fill(repo, file)`: the file with its lines once loaded, else as
 * listed (`lazy`); `need(repo, file)`: queue it; `failed(…)`: its last fetch failed (need() retries).
 */
export function useLazyFiles(host: string, id: string) {
  const [state, setState] = useState<{ bodies: Map<string, DiffFile>; errors: Set<string> }>(() => ({ bodies: new Map(), errors: new Set() }))
  const loaded = useRef(new Set<string>()) // keys with a body (read in callbacks; `state` is what renders)
  const inflight = useRef(new Set<string>())
  const queue = useRef<Want[]>([])
  const running = useRef(0)
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null)
  const alive = useRef(true)
  const pumpRef = useRef<() => void>(() => {})

  useEffect(() => {
    alive.current = true
    return () => {
      alive.current = false
      if (timer.current) clearTimeout(timer.current)
    }
  }, [])

  const pump = useCallback(() => {
    while (running.current < PARALLEL && queue.current.length) {
      const first = queue.current[0]
      const group = queue.current.filter((w) => w.root === first.root && w.base === first.base && w.tree === first.tree).slice(0, BATCH)
      queue.current = queue.current.filter((w) => !group.includes(w))
      running.current++
      api
        .diffFiles(host, id, { root: first.root, base: first.base, tree: first.tree, paths: [...new Set(group.flatMap((w) => w.paths))] })
        .then(
          (r) => {
            const got = new Map(r.files.map((f) => [f.path, f]))
            if (!alive.current) return
            setState((prev) => {
              const bodies = new Map(prev.bodies)
              const errors = new Set(prev.errors)
              for (const w of group) {
                const f = got.get(w.path)
                // Not in the answer (gone from the diff meanwhile): an empty body, not a retry loop.
                bodies.set(w.key, f ? { ...f, lazy: false, sig: w.sig } : { path: w.path, oldPath: null, status: 'modified', binary: false, additions: 0, deletions: 0, truncated: false, hunks: [], sig: w.sig })
                errors.delete(w.key)
                loaded.current.add(w.key)
              }
              if (bodies.size > MAX_CACHE)
                for (const k of [...bodies.keys()].slice(0, bodies.size - MAX_CACHE)) {
                  bodies.delete(k)
                  loaded.current.delete(k)
                }
              return { bodies, errors }
            })
          },
          () => {
            if (alive.current) setState((prev) => ({ bodies: prev.bodies, errors: new Set([...prev.errors, ...group.map((w) => w.key)]) }))
          },
        )
        .finally(() => {
          group.forEach((w) => inflight.current.delete(w.key))
          running.current--
          if (alive.current) pumpRef.current()
        })
    }
  }, [host, id])

  useEffect(() => {
    pumpRef.current = pump
  }, [pump])

  const need = useCallback(
    (repo: DiffRepo, file: DiffFile) => {
      if (!file.lazy || !repo.base?.sha || !repo.tree) return
      const key = bodyKey(repo, file)
      if (loaded.current.has(key) || inflight.current.has(key)) return
      setState((prev) => (prev.errors.has(key) ? { bodies: prev.bodies, errors: new Set([...prev.errors].filter((k) => k !== key)) } : prev))
      inflight.current.add(key)
      queue.current.push({ key, sig: file.sig, root: repo.root, base: repo.base.sha, tree: repo.tree, path: file.path, paths: file.oldPath && file.oldPath !== file.path ? [file.path, file.oldPath] : [file.path] })
      // Cards that come into view together go in one request.
      if (!timer.current)
        timer.current = setTimeout(() => {
          timer.current = null
          pump()
        }, 30)
    },
    [pump],
  )

  const { bodies, errors } = state
  // The cached body object itself: unchanged files keep their identity (FileDiff is memoised).
  const fill = useCallback((repo: DiffRepo, file: DiffFile): DiffFile => (file.lazy ? (bodies.get(bodyKey(repo, file)) ?? file) : file), [bodies])
  const failed = useCallback((repo: DiffRepo, file: DiffFile) => errors.has(bodyKey(repo, file)), [errors])
  return { fill, need, failed }
}
