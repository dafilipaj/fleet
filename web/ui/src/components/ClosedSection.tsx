import { useState } from 'react'
import { ChevronRightIcon, HistoryIcon, Loader2Icon, PlayIcon } from 'lucide-react'

import type { ClosedView } from '@/api/types'
import { ArmButton } from '@/components/DormantSection'
import { HostBadge } from '@/components/HostBadge'
import { Button } from '@/components/ui/button'
import { useDormant } from '@/hooks/useDormant'
import { byActivity, closedMeta, dormantTitles, searchDormant } from '@/lib/dormant'
import { cn } from '@/lib/utils'

/**
 * Sessions closed recently within a boot (Close, kill, /exit), every host in one list, most
 * recently closed first, each with its host badge — collapsed by default.
 * Resume (one at a time: a whole tmux session with its layout, or a Claude session in a new
 * window of its tmux session when that lives on) and Forget. Unlike Dormant: no Resume all, and
 * never on the Board. Nothing when no host has any. `host`: only that host. `query`: the session
 * search — only matching entries, expanded.
 */
export function ClosedSection({ now, host = null, query = '', desktop = false, className }: { now: number; host?: string | null; query?: string; desktop?: boolean; className?: string }) {
  const { hosts, busy, resumeClosed, forgetClosed } = useDormant()
  const [open, setOpen] = useState(false)
  const shown = (host ? hosts.filter((h) => h.host === host) : hosts).map((h) => searchDormant(h, query)).filter((h) => h.closed.length > 0)
  if (!shown.length) return null
  const total = shown.reduce((n, h) => n + h.closed.length, 0)
  const size = desktop ? 'sm' : 'lg'
  // A search shows its matches without having to expand the section.
  const expanded = open || !!query.trim()
  return (
    <section aria-label="Closed sessions" className={cn('flex flex-col gap-2', className)}>
      <button
        type="button"
        aria-expanded={expanded}
        onClick={() => setOpen((o) => !o)}
        className="flex min-h-8 items-center gap-1.5 rounded-md px-1 text-left text-xs font-semibold text-muted-foreground outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
      >
        <ChevronRightIcon className={cn('size-3.5 transition-transform', expanded && 'rotate-90')} />
        <HistoryIcon className="size-3.5" />
        Closed
        <span className="font-normal text-dimmer tabular-nums">· {total}</span>
      </button>
      {expanded
        ? byActivity(shown, (h) => h.closed).map(({ host: h, view: v }) => (
            <ClosedRow
              key={`${h}/${v.kind}/${v.target}`}
              host={h}
              view={v}
              now={now}
              size={size}
              busy={busy.has(`${h}/closed/${v.target}`)}
              onResume={() => void resumeClosed(h, v.target)}
              onForget={() => void forgetClosed(h, v.target)}
            />
          ))
        : null}
    </section>
  )
}

function ClosedRow({
  host,
  view: v,
  now,
  size,
  busy,
  onResume,
  onForget,
}: {
  host: string
  view: ClosedView
  now: number
  size: 'sm' | 'lg'
  busy: boolean
  onResume: () => void
  onForget: () => void
}) {
  const titles = dormantTitles(v)
  const how = v.kind === 'tmux' ? 'Recreate the tmux session and resume its Claude sessions' : v.tmuxSession ? `Resume it in a new window of ${v.tmuxSession}` : 'Resume it in a new tmux session'
  return (
    <div className="rounded-lg border border-dashed border-border/50 bg-card/30 px-3 py-2">
      <div className="min-w-0 opacity-70">
        <div className="flex min-w-0 items-center gap-2">
          <p className="min-w-0 flex-1 truncate text-sm font-semibold" title={v.name}>
            {v.name}
          </p>
          <HostBadge host={host} />
        </div>
        {titles && titles !== v.name ? <p className="line-clamp-2 text-xs break-words text-muted-foreground">{titles}</p> : null}
        <p className="mt-0.5 truncate text-[0.6875rem] text-dimmer tabular-nums">{closedMeta(v, now)}</p>
      </div>
      <div className="mt-1.5 flex items-center justify-end gap-1">
        <ArmButton confirm="Forget?" onConfirm={onForget} size={size} className="text-dimmer" title="Drop it from the list">
          Forget
        </ArmButton>
        <Button type="button" variant="outline" size={size} disabled={busy} onClick={onResume} title={how}>
          {busy ? <Loader2Icon className="animate-spin" /> : <PlayIcon />}
          Resume
        </Button>
      </div>
    </div>
  )
}
