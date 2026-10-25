import { LoaderCircleIcon, SparklesIcon } from 'lucide-react'

import { Button } from '@/components/ui/button'
import type { GroupsState } from '@/hooks/useGroups'
import { groupsStatusText } from '@/lib/groups'
import { cn } from '@/lib/utils'

/**
 * Last-run note ("grouped 3m ago · 1 model call" / "fallback: by repo") + "Regroup now".
 * `quiet`: the note only when something failed; otherwise it is the button's tooltip.
 */
export function GroupsStatus({
  state,
  now,
  size = 'sm',
  quiet = false,
  className,
}: {
  state: GroupsState
  now: number
  size?: 'sm' | 'lg'
  quiet?: boolean
  className?: string
}) {
  const { groups, running, run, error } = state
  const enabled = !!groups?.enabled
  const text = error ? 'groups unavailable — by repo' : groupsStatusText(groups, now)
  const failed = !!groups?.lastRun && groups.lastRun.ok === false
  return (
    <span className={cn('flex min-w-0 items-center gap-1.5', className)}>
      {quiet && !failed && !error ? null : (
        <span
          className={cn('min-w-0 truncate text-[0.6875rem] tabular-nums', failed || error ? 'text-destructive' : 'text-dimmer')}
          title={groups?.lastRun?.error || groups?.lastRun?.note || (groups?.host ? `grouping runs on ${groups.host}` : undefined)}
          aria-live="polite"
        >
          {text}
        </span>
      )}
      {enabled ? (
        <Button
          variant="ghost"
          size={size === 'lg' ? 'icon' : 'sm'}
          onClick={() => void run()}
          disabled={running}
          aria-label="Regroup now"
          title={quiet ? `Regroup now — ${text}` : 'Regroup now'}
          className={cn('shrink-0 text-muted-foreground hover:text-foreground', size === 'lg' ? 'size-10' : 'h-7 px-2 text-xs')}
        >
          {running ? <LoaderCircleIcon className="animate-spin" /> : <SparklesIcon />}
          {size === 'lg' ? null : 'Regroup'}
        </Button>
      ) : null}
    </span>
  )
}
