import { useEffect, useState } from 'react'

import { api } from '@/api/client'
import type { HarnessesResponse } from '@/api/types'

// One answer per host per page load (the server caches its own for 10 minutes): the list only
// changes when a harness is installed, and `pi --list-models` behind it is slow.
const cache = new Map<string, Promise<HarnessesResponse | null>>()

/** Forget the cached answers (Settings → Models changed a default), so the next read asks again. */
export function invalidateHarnesses() {
  cache.clear()
}

function load(host: string): Promise<HarnessesResponse | null> {
  let p = cache.get(host)
  if (!p) {
    // An older server (404) or a host that can't answer: Claude alone, from the caller's fallback.
    p = api.harnesses(host).then(
      (r) => (Array.isArray(r.harnesses) ? r : null),
      () => {
        cache.delete(host)
        return null
      },
    )
    cache.set(host, p)
  }
  return p
}

/** Ask every host up front (the New session form), so switching hosts shows its answer at once. */
export function prefetchHarnesses(hosts: string[]) {
  for (const h of hosts) if (h) void load(h)
}

/**
 * What `host` can spawn (GET /api/hosts/:host/harnesses): `harnesses` null when unknown (an older
 * server, an unreachable host); `loading` until its answer is in.
 */
export function useHarnesses(host: string | null | undefined): { harnesses: HarnessesResponse['harnesses'] | null; loading: boolean } {
  const [state, setState] = useState<{ host: string; res: HarnessesResponse | null } | null>(null)
  useEffect(() => {
    if (!host) return
    let live = true
    void load(host).then((res) => {
      if (live) setState({ host, res })
    })
    return () => {
      live = false
    }
  }, [host])
  const here = state && state.host === host ? state : null
  return { harnesses: here?.res?.harnesses ?? null, loading: !!host && !here }
}
