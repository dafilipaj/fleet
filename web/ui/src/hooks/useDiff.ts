import { useCallback, useRef, useState } from 'react'

import { ApiError, api, sessionErrorMessage } from '@/api/client'
import type { DiffsResponse } from '@/api/types'
import { usePoller } from '@/hooks/usePoller'

/** The diff follows the session live: a poll is one `git add` into a throwaway index, and an unchanged tree answers in a few bytes. */
export const DIFF_POLL_MS = 2_000

function diffError(err: unknown): string {
  if (err instanceof ApiError && err.status === 404 && err.message === 'not found') return 'This host’s fleet web server predates the diff reviewer — update and restart it'
  if (err instanceof ApiError && err.status === 409) return err.message === 'not a git repository' ? 'The session’s folder is not a git checkout' : err.message
  return sessionErrorMessage(err)
}

/**
 * A session's diffs — every checkout it works in — polled with `since=<fingerprint>` so nothing
 * re-renders while nothing moved. `changedAt`: when the files last changed (the "changed" hint).
 */
export function useDiff(host: string, id: string, root: string | null = null) {
  const [data, setData] = useState<{ key: string; diff: DiffsResponse } | null>(null)
  const [error, setError] = useState<string | null>(null)
  const [changedAt, setChangedAt] = useState<number | null>(null)
  const last = useRef<{ key: string; fingerprint: string } | null>(null)
  const key = `${host}/${id}/${root ?? ''}`

  const poll = useCallback(
    async (signal: AbortSignal) => {
      const since = last.current?.key === key ? last.current.fingerprint : null
      try {
        const r = await api.diffs(host, id, since, { signal }, root, true)
        setError(null)
        if (r.unchanged && since) return
        last.current = { key, fingerprint: r.fingerprint }
        setData({ key, diff: r })
        setChangedAt(Date.now())
      } catch (err) {
        if (signal.aborted) throw err
        setError(diffError(err))
      }
    },
    [host, id, key, root],
  )
  const refresh = usePoller(poll, DIFF_POLL_MS)
  return { data: data?.key === key ? data.diff : null, error, changedAt, refresh }
}
