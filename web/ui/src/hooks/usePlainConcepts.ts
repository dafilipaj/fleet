import { useMemo } from 'react'

import { api } from '@/api/client'
import type { PlainModule } from '@/lib/plain'
import { createConceptResolver, localResolver, type ConceptResolver } from '@/lib/plainConcepts'

// Module texts and checkout listings, shared by every spec on screen (a review shows several):
// a term looked up in one file does not re-read the modules another file already read.
const TTL_MS = 30_000
const cache = new Map<string, { at: number; v: Promise<unknown> }>()
function cached<T>(key: string, get: () => Promise<T>): Promise<T> {
  const hit = cache.get(key)
  if (hit && Date.now() - hit.at < TTL_MS) return hit.v as Promise<T>
  const v = get()
  v.catch(() => cache.delete(key))
  cache.set(key, { at: Date.now(), v })
  return v
}

/**
 * Looks up a PlainLang concept for the module at `path` (absolute, on the session's host):
 * here, in what it imports / requires, else anywhere in its checkout. Without a host, only here.
 */
export function usePlainConcepts(host: string | null, id: string | null, path: string | null, mod: PlainModule): ConceptResolver {
  return useMemo(() => {
    if (!host || !id || !path) return localResolver(mod, path ?? '')
    const k = (what: string, p: string) => `${host}\0${id}\0${what}\0${p}`
    return createConceptResolver({
      path,
      mod,
      list: () => cached(k('list', path), () => api.filePlain(host, id, path).then((r) => r.files)),
      load: (p) => cached(k('text', p), () => api.fileText(host, id, p)),
    })
  }, [host, id, path, mod])
}
