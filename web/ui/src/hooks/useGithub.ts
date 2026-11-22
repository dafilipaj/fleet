import { useSyncExternalStore } from 'react'

import { api, ApiError } from '@/api/client'
import type { GithubStatus } from '@/api/types'

/**
 * GitHub links, shared by every panel: one poll of /api/github (every 20s while anything is
 * subscribed and the tab is visible). A fleet without a `github.host` answers `enabled: false`
 * without a host once (an older server a 404), and the page stops asking.
 */
const POLL_MS = 20 * 1000
let state: GithubStatus | null = null
const listeners = new Set<() => void>()
let timer: ReturnType<typeof setTimeout> | undefined
let inflight: Promise<void> | null = null

function emit() {
  for (const l of listeners) l()
}

export function refreshGithub(): Promise<void> {
  if (inflight) return inflight
  inflight = api
    .github()
    .then((s) => {
      state = s
      emit()
    })
    .catch((err) => {
      if (err instanceof ApiError && err.status === 404) {
        state = { enabled: false, host: null, links: [], ended: [] }
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
  if (document.visibilityState === 'visible') void refreshGithub().then(next)
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

export function useGithub(): GithubStatus | null {
  return useSyncExternalStore(subscribe, () => state)
}
