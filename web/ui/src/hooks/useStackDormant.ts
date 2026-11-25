import { useCallback, useMemo, useState } from 'react'

import { api, isAbortError } from '@/api/client'
import type { StackView } from '@/api/types'
import { useDormant } from '@/hooks/useDormant'
import { usePoller } from '@/hooks/usePoller'
import { stackDormant, type DormantMember } from '@/lib/dormant'

/** How often the stack lists are re-read while some host has inactive sessions. */
export const STACK_DORMANT_POLL_MS = 60_000

/**
 * The inactive members of the stacks homed on `homes`, by stack key (`home/id`), so a stack in
 * the session list shows its whole self. Membership comes from each home's stack list (polled
 * only while any host has inactive sessions); whether a member is inactive comes from the
 * dormant list, so a Resume shows at once.
 */
export function useStackDormant(homes: readonly string[]): Map<string, DormantMember[]> {
  const { index, count } = useDormant()
  const [stacks, setStacks] = useState<Map<string, StackView[]>>(new Map())
  const hostsKey = [...new Set(homes)].sort().join('\n')
  const poll = useCallback(
    async (signal: AbortSignal) => {
      const hosts = hostsKey ? hostsKey.split('\n') : []
      const got = await Promise.all(
        hosts.map(async (h) => {
          try {
            return [h, (await api.stacks(h, { signal })).stacks ?? []] as const
          } catch (err) {
            if (isAbortError(err)) throw err
            return null // older server / unreachable: keep what was loaded
          }
        }),
      )
      setStacks((prev) => {
        const next = new Map<string, StackView[]>()
        for (const h of hosts) next.set(h, prev.get(h) ?? [])
        for (const e of got) if (e) next.set(e[0], e[1])
        return next
      })
    },
    [hostsKey],
  )
  usePoller(poll, STACK_DORMANT_POLL_MS, { enabled: count > 0 && !!hostsKey })
  return useMemo(() => (count > 0 ? stackDormant([...stacks.values()].flat(), index) : new Map<string, DormantMember[]>()), [stacks, index, count])
}
