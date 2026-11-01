import { useEffect, useState } from 'react'

import { api } from '@/api/client'
import type { GhIdentity, HarnessesResponse } from '@/api/types'

// One answer per host per page load (the server caches its own for 10 minutes): the list only
// changes when a harness is installed, and `pi --list-models` behind it is slow.
const cache = new Map<string, Promise<HarnessesResponse | null>>()

/** Forget the cached answers (Settings → Agents changed a default), so the next read asks again. */
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

function useHarnessesResponse(host: string | null | undefined): HarnessesResponse | null {
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
  return state && state.host === host ? state.res : null
}

/** What `host` can spawn (GET /api/hosts/:host/harnesses); null while loading or unknown. */
export function useHarnesses(host: string | null | undefined) {
  return useHarnessesResponse(host)?.harnesses ?? null
}

/** The GitHub identities a session spawned on `host` can act as; [] while loading or none. */
export function useGhIdentities(host: string | null | undefined): GhIdentity[] {
  return useHarnessesResponse(host)?.ghIdentities ?? []
}
