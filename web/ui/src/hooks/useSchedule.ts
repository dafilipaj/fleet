import { useCallback, useMemo, useState } from 'react'

import { ApiError, api, isAbortError } from '@/api/client'
import type { ScheduledEntry } from '@/api/types'
import { useFleet } from '@/hooks/useFleet'
import { usePoller } from '@/hooks/usePoller'
import { pendingEntries } from '@/lib/schedule'

const POLL_MS = 60_000

interface HostSchedule {
  host: string
  entries: ScheduledEntry[]
}

// Last answers, kept across remounts so reopening paints at once.
let last: HostSchedule[] = []

/** Every reachable host's scheduled sessions (GET …/schedule), polled while shown. Hosts too old to have them are left out. */
export function useSchedule(enabled = true) {
  const { fleet } = useFleet()
  const hosts = useMemo(() => (fleet?.hosts ?? []).filter((h) => h.ok !== false).map((h) => h.name), [fleet])
  const [results, setResults] = useState<HostSchedule[]>(last)
  const hostsKey = hosts.join('\n')

  const load = useCallback(
    async (signal: AbortSignal) => {
      if (!hosts.length) return
      const next = await Promise.all(
        hosts.map(async (host): Promise<HostSchedule> => {
          try {
            return { host, entries: (await api.schedule(host, { signal })).entries ?? [] }
          } catch (err) {
            if (isAbortError(err)) throw err
            if (!(err instanceof ApiError)) console.warn(`schedule on ${host}:`, err)
            return { host, entries: [] }
          }
        }),
      )
      last = next
      setResults(next)
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps -- hostsKey stands in for hosts
    [hostsKey],
  )

  const reload = usePoller((signal) => load(signal).catch(() => {}), POLL_MS, { enabled: enabled && hosts.length > 0 })
  const pending = useMemo(() => pendingEntries(results), [results])
  return { pending, reload }
}
