import { useSyncExternalStore } from 'react'

import { api, ApiError } from '@/api/client'
import type { VerandaLink, VerandaStatus } from '@/api/types'

/**
 * Veranda links (opt-in), shared by every panel: one poll of /api/veranda (every 20s while anything
 * is subscribed and the tab is visible). A server without a `veranda` section answers
 * `configured: false` once (an older server a 404), and the page stops asking.
 */
const POLL_MS = 20 * 1000
let state: VerandaStatus | null = null
const listeners = new Set<() => void>()
let timer: ReturnType<typeof setTimeout> | undefined
let inflight: Promise<void> | null = null

function emit() {
  for (const l of listeners) l()
}

export function refreshVeranda(): Promise<void> {
  if (inflight) return inflight
  inflight = api
    .veranda()
    .then((s) => {
      state = s
      emit()
    })
    .catch((err) => {
      // A server from before Veranda links: nothing to show, nothing to ask again.
      if (err instanceof ApiError && err.status === 404) {
        state = { configured: false, enabled: false, host: null, links: [], ended: [] }
        emit()
      }
    })
    .finally(() => {
      inflight = null
    })
  return inflight
}

const unconfigured = () => state?.configured === false

function loop() {
  clearTimeout(timer)
  if (!listeners.size || unconfigured()) return
  const next = () => {
    if (!unconfigured()) timer = setTimeout(loop, POLL_MS)
  }
  if (document.visibilityState === 'visible') void refreshVeranda().then(next)
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

export function useVeranda(): VerandaStatus | null {
  return useSyncExternalStore(subscribe, () => state)
}

/** The active link of one session, if any (one room per session). */
export function useSessionVerandaLink(host: string | null | undefined, sessionId: string | null | undefined): VerandaLink | null {
  const s = useVeranda()
  if (!s?.enabled || !host || !sessionId) return null
  return s.links.find((l) => (l.host ?? s.host) === host && l.session === sessionId) ?? null
}
