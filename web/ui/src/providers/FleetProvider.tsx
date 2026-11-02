import { useCallback, useMemo, useRef, useState, type ReactNode } from 'react'

import { api, isAbortError } from '@/api/client'
import type { FleetResponse } from '@/api/types'
import { FleetContext, loadSnapshot, saveSnapshot, type FleetState } from '@/hooks/useFleet'
import { usePoller } from '@/hooks/usePoller'
import { setKnownHosts } from '@/lib/format'
import { closedKey, withoutClosed } from '@/lib/sessions'

export const FLEET_POLL_MS = 5000
/** How long a closed session stays hidden from polls that still list it. */
const CLOSED_HIDE_MS = 20_000

/** Polls /api/fleet every 5s (paused while the page is hidden) for the whole app. */
export function FleetProvider({ children, pollMs = FLEET_POLL_MS }: { children: ReactNode; pollMs?: number }) {
  const [snap, setSnap] = useState(() => {
    const s = loadSnapshot()
    setKnownHosts(s?.fleet?.hosts)
    return { fleet: s?.fleet ?? null, at: s?.at ?? 0 }
  })
  const [error, setError] = useState<string | null>(null)
  const [refreshing, setRefreshing] = useState(false)
  const closed = useRef(new Map<string, number>())

  const applyFleet = useCallback((data: FleetResponse) => {
    const now = Date.now()
    for (const [k, until] of closed.current) if (until <= now) closed.current.delete(k)
    const fleet = withoutClosed(data, closed.current, now)
    const at = data?.snapshotAt || now
    setKnownHosts(fleet?.hosts)
    setSnap({ fleet, at })
    saveSnapshot({ fleet, at })
  }, [])

  const dropSession = useCallback(
    (host: string, id: string) => {
      closed.current.set(closedKey(host, id), Date.now() + CLOSED_HIDE_MS)
      if (snap.fleet) applyFleet(snap.fleet)
    },
    [snap.fleet, applyFleet],
  )

  const poll = useCallback(
    async (signal: AbortSignal) => {
      setRefreshing(true)
      try {
        applyFleet(await api.fleet({ signal }))
        setError(null)
      } catch (err) {
        if (isAbortError(err)) throw err
        setError((err as Error)?.message || 'request failed')
      } finally {
        if (!signal.aborted) setRefreshing(false)
      }
    },
    [applyFleet],
  )

  const refresh = usePoller(poll, pollMs)

  const value = useMemo<FleetState>(
    () => ({ fleet: snap.fleet, fleetAt: snap.at, error, refreshing, refresh, applyFleet, dropSession }),
    [snap, error, refreshing, refresh, applyFleet, dropSession],
  )
  return <FleetContext.Provider value={value}>{children}</FleetContext.Provider>
}
