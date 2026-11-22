import { useSyncExternalStore } from 'react'

import { ApiError } from '@/api/client'

/** What a link bridge's status answers (GET /api/github, /api/productive). */
export interface LinkBridgeStatus {
  enabled: boolean
  host: string | null
  links: { host: string | null; session: string }[]
}

/**
 * One bridge's links, shared by every panel: one poll (every 20s while anything is subscribed and
 * the tab is visible). A fleet without the bridge answers `enabled: false` without a host once
 * (an older server a 404), and the page stops asking.
 */
export function createLinkStatusStore<S extends LinkBridgeStatus>(fetchStatus: () => Promise<S>, empty: S) {
  const POLL_MS = 20 * 1000
  let state: S | null = null
  const listeners = new Set<() => void>()
  let timer: ReturnType<typeof setTimeout> | undefined
  let inflight: Promise<void> | null = null

  const emit = () => {
    for (const l of listeners) l()
  }

  function refresh(): Promise<void> {
    if (inflight) return inflight
    inflight = fetchStatus()
      .then((s) => {
        state = s
        emit()
      })
      .catch((err) => {
        if (err instanceof ApiError && err.status === 404) {
          state = empty
          emit()
        }
      })
      .finally(() => {
        inflight = null
      })
    return inflight
  }

  /** No bridge configured: nothing will ever be linked, stop polling. */
  const noBridge = () => state != null && !state.enabled && !state.host

  function loop() {
    clearTimeout(timer)
    if (!listeners.size || noBridge()) return
    const next = () => {
      if (!noBridge()) timer = setTimeout(loop, POLL_MS)
    }
    if (document.visibilityState === 'visible') void refresh().then(next)
    else next()
  }

  function subscribe(fn: () => void) {
    listeners.add(fn)
    if (listeners.size === 1) loop()
    return () => {
      listeners.delete(fn)
      if (!listeners.size) clearTimeout(timer)
    }
  }

  return {
    refresh,
    useStatus: (): S | null => useSyncExternalStore(subscribe, () => state),
  }
}
