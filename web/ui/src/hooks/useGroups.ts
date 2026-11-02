import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { toast } from 'sonner'

import { ApiError, api, isAbortError } from '@/api/client'
import type { FleetResponse, GroupMember, GroupsResponse, Session } from '@/api/types'
import { useDormant } from '@/hooks/useDormant'
import { usePersistentState } from '@/hooks/usePersistentState'
import { usePoller } from '@/hooks/usePoller'
import {
  applyGroupEdit,
  boardColumns,
  effectiveGroups,
  parseIdList,
  parseViewMode,
  regroupToast,
  reorderColumns,
  stickyColumns,
  withStackColumns,
  type BoardColumn,
  type GroupEdit,
  type ViewMode,
} from '@/lib/groups'
import { dormantMember, matchesDormantMember } from '@/lib/dormant'
import { allSessions } from '@/lib/sessions'
import { storage } from '@/lib/storage'

export const GROUPS_POLL_MS = 30_000
/** While the server says a pass is running: check back sooner. */
export const GROUPS_RUNNING_POLL_MS = 4000

/** `fleet.view`: List | Board. */
export function useViewMode(): [ViewMode, (v: ViewMode) => void] {
  return usePersistentState<ViewMode>('fleet.view', 'list', parseViewMode)
}

/**
 * /api/groups, polled every 30s only while `enabled` (the Board is on screen). A 404 or
 * any error (older server, grouping host unreachable) leaves `groups` null — the board then
 * groups by repo client-side. `run()` is "Regroup now" (POST /api/groups/run) with a toast.
 */
export function useGroups(enabled: boolean) {
  const [groups, setGroups] = useState<GroupsResponse | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [loaded, setLoaded] = useState(false)
  const [running, setRunning] = useState(false)

  // Board edits in flight: a poll answered meanwhile is stale — it would undo the optimistic edit.
  const editing = useRef(0)

  const poll = useCallback(async (signal: AbortSignal) => {
    try {
      const res = await api.groups({ signal })
      if (editing.current === 0) setGroups(res)
      setError(null)
    } catch (err) {
      if (isAbortError(err)) throw err
      // An older server has no /api/groups: that is "no grouping", not an error to show.
      if (err instanceof ApiError && (err.status === 404 || err.status === 501)) {
        setGroups(null)
        setError(null)
      } else setError((err as Error)?.message || 'request failed')
    } finally {
      if (!signal.aborted) setLoaded(true)
    }
  }, [])

  const busy = running || !!groups?.running
  const refresh = usePoller(poll, busy ? GROUPS_RUNNING_POLL_MS : GROUPS_POLL_MS, { enabled })

  const run = useCallback(async () => {
    if (running) return
    setRunning(true)
    try {
      const res = await api.runGroups()
      setGroups(res)
      setError(null)
      if (res.lastRun?.ok === false) toast.error(regroupToast(res))
      else toast(regroupToast(res))
    } catch (err) {
      const msg =
        err instanceof ApiError && err.status === 501
          ? 'Smart grouping is off (web.grouping.enabled)'
          : (err as Error)?.message || 'Grouping failed'
      toast.error(msg)
    } finally {
      setRunning(false)
    }
  }, [running])

  /**
   * Rename a group / move a session (POST /api/groups/edit): applied to the board at once,
   * replaced by the server's answer; on failure a toast and a fresh fetch put it back.
   */
  const edit = useCallback(
    async (e: GroupEdit) => {
      editing.current += 1
      setGroups((g) => (g ? applyGroupEdit(g, e) : g))
      let failed = false
      try {
        const res = await api.editGroups(e)
        if (editing.current === 1) setGroups(res)
      } catch (err) {
        failed = true
        toast.error((err as Error)?.message || (e.op === 'rename' ? 'Rename failed' : 'Move failed'))
      } finally {
        editing.current -= 1
        if (failed && editing.current === 0) refresh()
      }
    },
    [refresh],
  )

  /** Only the server's groups can be edited — not the repo fallback drawn client-side. */
  const editable = !!groups?.enabled

  return { groups, error, loaded, running: busy, refresh, run, edit, editable }
}

export type GroupsState = ReturnType<typeof useGroups>

const ORDER_KEY = 'fleet.boardOrder'

/**
 * The board's columns for `sessions` (already filtered), in a sticky order remembered in
 * `fleet.boardOrder`: a column keeps its place while statuses change; new ones are appended.
 * Session stacks become their own columns first (withStackColumns), so their ids are remembered too.
 * `moveColumn` is a drag on the board (per browser, like the rest of that order).
 */
export function useBoardColumns(
  enabled: boolean,
  sessions: Session[],
  groups: GroupsResponse | null,
  fleet: FleetResponse | null,
  /** The host filter: dormant cards of other hosts are left out (null = every host). */
  hosts: ReadonlySet<string> | null = null,
  /** The session search: dormant cards that do not match are left out. */
  query = '',
): { columns: BoardColumn[]; moveColumn: (id: string, before: string | null) => void } {
  const [order, setOrder] = useState<string[]>(() => parseIdList(storage.getJSON(ORDER_KEY)))
  const { index } = useDormant()
  const sticky = useMemo(() => {
    if (!enabled) return null
    const dormantOf = (m: GroupMember) => {
      if (hosts && !hosts.has(m.host)) return null
      const d = dormantMember(m, index)
      return d && matchesDormantMember(d, query) ? d : null
    }
    return stickyColumns(withStackColumns(boardColumns(sessions, effectiveGroups(groups, allSessions(fleet)).groups, dormantOf), sessions), order)
  }, [enabled, sessions, groups, fleet, order, index, hosts, query])
  if (sticky && (sticky.order.length !== order.length || sticky.order.some((id, i) => id !== order[i]))) setOrder(sticky.order)
  useEffect(() => {
    storage.set(ORDER_KEY, JSON.stringify(order))
  }, [order])
  const moveColumn = useCallback((id: string, before: string | null) => setOrder((o) => reorderColumns(o, id, before)), [])
  return { columns: sticky?.columns ?? [], moveColumn }
}
