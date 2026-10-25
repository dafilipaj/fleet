import { useMemo, useState } from 'react'

import { useFleet } from '@/hooks/useFleet'
import { usePersistentState } from '@/hooks/usePersistentState'
import { relTime } from '@/lib/format'
import { allSessions, findStatusFilter, keepOrder, listView, statusSummary, type StatusFilterId } from '@/lib/sessions'

export const ALL_HOSTS = '*'

/**
 * The session list's state, shared by the mobile list screen and the desktop sidebar:
 * search, status + host filters (persisted in `fleet.filter`; "Scheduled" is a status filter),
 * the filtered view, per-host counts, the header summary and the "updated Xs ago" note.
 * While `frozen` (the pointer is on the list) the view keeps its order: rows don't move
 * under the mouse; the new order lands when it leaves.
 */
export function useSessionList(now: number) {
  const { fleet, fleetAt, error, refreshing } = useFleet()
  const [query, setQuery] = useState('')
  const [status, setStatus] = usePersistentState<StatusFilterId>('fleet.filter', 'all', (raw) => findStatusFilter(raw).id)
  const [hostFilter, setHostFilter] = usePersistentState<string>('fleet.hostFilter', ALL_HOSTS)

  const hosts = fleet?.hosts ?? []
  const hostNames = hosts.map((h) => h.name)
  // A remembered host that is no longer configured means "all".
  const host = hostFilter !== ALL_HOSTS && hostNames.includes(hostFilter) ? hostFilter : null

  const live = useMemo(() => listView(fleet, { query, host, status }), [fleet, query, host, status])
  // The order the list froze in (session keys); null when it follows the live order.
  const [frozenOrder, setFrozenOrder] = useState<string[] | null>(null)
  const setFrozen = (on: boolean) => setFrozenOrder(on ? live.sessions.map((s) => `${s.host}/${s.session_id}`) : null)
  const view = useMemo(() => (frozenOrder ? { ...live, sessions: keepOrder(live.sessions, frozenOrder) } : live), [live, frozenOrder])
  const hostCounts = useMemo(() => {
    const counts = new Map<string, number>()
    for (const s of allSessions(fleet)) counts.set(s.host, (counts.get(s.host) ?? 0) + 1)
    return counts
  }, [fleet])
  const summary = useMemo(() => statusSummary(fleet), [fleet])

  const unreachable = hosts.filter((h) => h.ok === false && (host == null || h.name === host))

  let note: { text: string; error?: boolean }
  if (!fleet && refreshing) note = { text: 'loading…' }
  else if (error) note = { text: 'offline — retrying', error: true }
  else if (refreshing) note = { text: 'refreshing…' }
  else note = { text: fleetAt ? `updated ${relTime(fleetAt, now)} ago` : '' }

  return {
    fleet,
    error,
    query,
    setQuery,
    status,
    setStatus,
    setHostFilter,
    hosts,
    hostNames,
    host,
    view,
    setFrozen,
    hostCounts,
    summary,
    unreachable,
    note,
  }
}

export type SessionListState = ReturnType<typeof useSessionList>
