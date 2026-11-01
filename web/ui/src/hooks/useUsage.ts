import { useCallback, useMemo, useState } from 'react'

import { ApiError, api, isAbortError } from '@/api/client'
import { useFleet } from '@/hooks/useFleet'
import { usePoller } from '@/hooks/usePoller'
import { groupByAccount, usageErrors, type HostUsage } from '@/lib/usage'

// The CLI caches for 60s (the endpoint rate-limits), so polling faster buys nothing.
const POLL_MS = 60_000

// Last answers, kept across remounts so reopening the page paints at once.
let last: HostUsage[] = []

/** Every reachable host's subscription usage, polled while shown, grouped by account. */
export function useUsage() {
  const { fleet } = useFleet()
  const hosts = useMemo(() => (fleet?.hosts ?? []).filter((h) => h.ok).map((h) => h.name), [fleet])
  const [results, setResults] = useState<HostUsage[]>(last)
  const [loading, setLoading] = useState(last.length === 0)
  const hostsKey = hosts.join('\n')

  const load = useCallback(
    async (signal: AbortSignal, refresh = false) => {
      if (!hosts.length) return
      const next = await Promise.all(
        hosts.map(async (host): Promise<HostUsage> => {
          try {
            return { host, usage: await api.usage(host, { signal, refresh }), error: null }
          } catch (err) {
            if (isAbortError(err)) throw err
            // 404/501: that host's server predates the usage route.
            const old = err instanceof ApiError && (err.status === 404 || err.status === 501)
            const error = old ? 'runs an older fleet — update it to see its usage' : (err as Error)?.message || 'could not load usage'
            return { host, usage: null, error }
          }
        }),
      )
      last = next
      setResults(next)
      setLoading(false)
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps -- hostsKey stands in for hosts
    [hostsKey],
  )

  const refreshPoll = usePoller((signal) => load(signal).catch(() => {}), POLL_MS, { enabled: hosts.length > 0 })
  const [refreshing, setRefreshing] = useState(false)
  const refresh = useCallback(async () => {
    setRefreshing(true)
    const c = new AbortController()
    try {
      await load(c.signal, true)
    } catch {
      refreshPoll()
    } finally {
      setRefreshing(false)
    }
  }, [load, refreshPoll])

  const accounts = useMemo(() => groupByAccount(results), [results])
  const errors = useMemo(() => usageErrors(results), [results])
  return { accounts, errors, loading: loading && hosts.length > 0, refresh, refreshing }
}
