import { useCallback, useEffect, useState } from 'react'

import { api, isAbortError } from '@/api/client'
import type { StackView } from '@/api/types'
import { usePoller } from '@/hooks/usePoller'
import { stackErrorMessage, stacksMissing } from '@/lib/stacks'
import { storage } from '@/lib/storage'

/** The Stack sheet refreshes this often while it is open. */
export const STACK_POLL_MS = 15_000
/** The session screen's stack bar (member counts only). */
export const STACK_BAR_POLL_MS = 60_000

/**
 * One stack (`GET /api/hosts/:host/stacks/:id`) while `enabled`, polled every `intervalMs`.
 * `missing` = that server predates stacks (404 "not found" / 501): callers hide their UI, no
 * error is shown. `apply()` records a fresh StackView (after a save).
 */
export function useStack(host: string | null, id: string | null, enabled: boolean, intervalMs = STACK_POLL_MS) {
  const key = host && id ? `${host}/${id}` : ''
  const [state, setState] = useState<{ key: string; stack: StackView | null; error: string | null; missing: boolean } | null>(null)
  const mine = state?.key === key ? state : null

  const poll = useCallback(
    async (signal: AbortSignal) => {
      if (!host || !id) return
      try {
        const stack = await api.stack(host, id, { signal })
        setState({ key: `${host}/${id}`, stack, error: null, missing: false })
      } catch (err) {
        if (isAbortError(err)) throw err
        const missing = stacksMissing(err)
        setState((prev) => ({
          key: `${host}/${id}`,
          // Keep what was loaded on a transient failure.
          stack: prev?.key === `${host}/${id}` && !missing ? prev.stack : null,
          error: missing ? null : stackErrorMessage(err),
          missing,
        }))
      }
    },
    [host, id],
  )
  const refresh = usePoller(poll, intervalMs, { enabled: enabled && !!key })
  const apply = useCallback((stack: StackView) => setState({ key: `${stack.host}/${stack.id}`, stack, error: null, missing: false }), [])

  return { stack: mine?.stack ?? null, error: mine?.error ?? null, missing: !!mine?.missing, loaded: !!mine, refresh, apply }
}

export type StackState = ReturnType<typeof useStack>

const orderKey = (host: string, id: string) => `fleet.stackOrder.${host}/${id}`

function readOrder(host: string | null, id: string | null | undefined): string[] {
  if (!host || !id) return []
  try {
    const v: unknown = JSON.parse(storage.get(orderKey(host, id)) ?? '[]')
    return Array.isArray(v) ? v.filter((x): x is string => typeof x === 'string') : []
  } catch {
    return []
  }
}

/**
 * A stack's member session ids in StackBrief order, remembered per stack (`fleet.stackOrder.*`)
 * so a switcher that mounts before its StackView loads already has it and never reshuffles.
 */
export function useStackOrder(host: string | null, id: string | null | undefined, stack: StackView | null): string[] {
  const fresh = stack && stack.host === host && stack.id === id ? stack.members.map((m) => m.session) : null
  const joined = fresh?.join(' ')
  useEffect(() => {
    if (host && id && joined != null) storage.set(orderKey(host, id), JSON.stringify(joined ? joined.split(' ') : []))
  }, [host, id, joined])
  return fresh ?? readOrder(host, id)
}
