import { useMemo, useState } from 'react'

import { useFleet } from '@/hooks/useFleet'
import { usePersistentState } from '@/hooks/usePersistentState'
import { useSlack } from '@/hooks/useSlack'
import { relTime } from '@/lib/format'
import { DEFAULT_SELECTION, isShared, parseSelection, resolveSelection, serializeSelection, type HostSelection } from '@/lib/hosts'
import { allSessions, findStatusFilter, keepOrder, listView, slackLinked, statusSummary, type StatusFilterId } from '@/lib/sessions'

/**
 * The session list's state, shared by the mobile list screen and the desktop sidebar:
 * search, status + host filters (persisted in `fleet.filter` / `fleet.hostFilter`; Bots / Slack / Apps
 * are status filters; the host filter is a scope or a multi-select, lib/hosts.ts), the filtered view,
 * per-host counts, the header summary (own hosts only: a shared shed's sessions are not yours to
 * answer) and the "updated Xs ago" note.
 * While `frozen` (the pointer is on the list) the view keeps its order: rows don't move
 * under the mouse; the new order lands when it leaves.
 */
export function useSessionList(now: number) {
  const { fleet, fleetAt, error, refreshing } = useFleet()
  const [query, setQuery] = useState('')
  const [status, setStatus] = usePersistentState<StatusFilterId>('fleet.filter', 'all', (raw) => findStatusFilter(raw).id)
  const [hostFilter, setHostFilter] = usePersistentState<string>('fleet.hostFilter', serializeSelection(DEFAULT_SELECTION))
  const selection = useMemo(() => parseSelection(hostFilter), [hostFilter])
  const setSelection = (sel: HostSelection) => setHostFilter(serializeSelection(sel))

  const hosts = useMemo(() => fleet?.hosts ?? [], [fleet])
  const hostNames = hosts.map((h) => h.name)
  // The hosts shown; a remembered host that is no longer configured drops out (lib/hosts.ts).
  const shown = useMemo(() => resolveSelection(selection, hosts), [selection, hosts])
  // Before the first snapshot there are no hosts to resolve against: show everything.
  const hostSet = hosts.length ? shown : null
  const ownHosts = useMemo(() => new Set(hosts.filter((h) => !isShared(h)).map((h) => h.name)), [hosts])

  const slackStatus = useSlack()
  const slack = useMemo(() => slackLinked(slackStatus?.enabled ? slackStatus.links : null), [slackStatus])
  const live = useMemo(() => listView(fleet, { query, hosts: hostSet, status, slack }), [fleet, query, hostSet, status, slack])
  // The order the list froze in (session keys); null when it follows the live order.
  const [frozenOrder, setFrozenOrder] = useState<string[] | null>(null)
  const setFrozen = (on: boolean) => setFrozenOrder(on ? live.sessions.map((s) => `${s.host}/${s.session_id}`) : null)
  const view = useMemo(() => (frozenOrder ? { ...live, sessions: keepOrder(live.sessions, frozenOrder) } : live), [live, frozenOrder])
  const hostCounts = useMemo(() => {
    const counts = new Map<string, number>()
    for (const s of allSessions(fleet)) counts.set(s.host, (counts.get(s.host) ?? 0) + 1)
    return counts
  }, [fleet])
  const summary = useMemo(() => statusSummary(fleet, ownHosts), [fleet, ownHosts])
  // Sessions waiting on each shared host: the rail's quiet hint that something happens there.
  const sharedWaiting = useMemo(() => {
    const out = new Map<string, number>()
    for (const h of hosts) {
      if (!isShared(h) || h.ok === false) continue
      out.set(h.name, h.sessions.filter((s) => s.status === 'waiting' && !s.operator).length)
    }
    return out
  }, [hosts])

  const unreachable = hosts.filter((h) => h.ok === false && (hostSet == null || hostSet.has(h.name)))

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
    selection,
    setSelection,
    hosts,
    hostNames,
    /** Host names the views show; null = all (no snapshot yet). */
    hostSet,
    sharedWaiting,
    view,
    setFrozen,
    hostCounts,
    summary,
    unreachable,
    note,
  }
}

export type SessionListState = ReturnType<typeof useSessionList>
