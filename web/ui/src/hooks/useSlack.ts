import { useSyncExternalStore } from 'react'

import { api } from '@/api/client'
import type { SlackLink, SlackStatus } from '@/api/types'

/**
 * Slack links, shared by every row and panel: one poll of /api/slack (every 20s while anything
 * is subscribed and the tab is visible), refreshed at once after a link or unlink.
 */
const POLL_MS = 20 * 1000
let state: SlackStatus | null = null
const listeners = new Set<() => void>()
let timer: ReturnType<typeof setTimeout> | undefined
let inflight: Promise<void> | null = null

function emit() {
  for (const l of listeners) l()
}

export function refreshSlack(): Promise<void> {
  if (inflight) return inflight
  inflight = api
    .slack()
    .then((s) => {
      state = s
      emit()
    })
    .catch(() => {})
    .finally(() => {
      inflight = null
    })
  return inflight
}

function loop() {
  clearTimeout(timer)
  if (!listeners.size) return
  if (document.visibilityState === 'visible') void refreshSlack()
  timer = setTimeout(loop, POLL_MS)
}

function subscribe(fn: () => void) {
  listeners.add(fn)
  if (listeners.size === 1) loop()
  return () => {
    listeners.delete(fn)
    if (!listeners.size) clearTimeout(timer)
  }
}

export function useSlack(): SlackStatus | null {
  return useSyncExternalStore(subscribe, () => state)
}

/** The active links of one session. */
export function useSessionSlackLinks(host: string | null | undefined, sessionId: string | null | undefined): SlackLink[] {
  const s = useSlack()
  if (!s?.enabled || !host || !sessionId) return NONE
  const mine = s.links.filter((l) => l.host === host && l.session === sessionId)
  return mine.length ? mine : NONE
}
const NONE: SlackLink[] = []

/** "42m" / "1h 5m" / "1d 3h" left. */
export function slackLeft(until: number, now: number): string {
  const m = Math.max(1, Math.ceil((until - now) / 60000))
  if (m >= 24 * 60) return `${Math.floor(m / 1440)}d${Math.floor((m % 1440) / 60) ? ` ${Math.floor((m % 1440) / 60)}h` : ''}`
  return m < 60 ? `${m}m` : `${Math.floor(m / 60)}h${m % 60 ? ` ${m % 60}m` : ''}`
}
