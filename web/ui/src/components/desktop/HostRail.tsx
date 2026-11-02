import type { MouseEvent } from 'react'
import { ChevronDownIcon, UsersIcon } from 'lucide-react'

import type { Host } from '@/api/types'
import { HostCard } from '@/components/desktop/HostCard'
import { HostDot } from '@/components/HostBadge'
import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover'
import type { SessionListState } from '@/hooks/useSessionList'
import { clickGroup, clickHost, isShared } from '@/lib/hosts'
import { cn } from '@/lib/utils'

const group = 'flex shrink-0 items-center gap-0.5 rounded-lg border border-border bg-card p-0.5'
const label = cn(
  'flex h-7 items-center gap-1 rounded-md px-1.5 text-[0.6875rem] font-medium tracking-wide text-dimmer uppercase outline-none',
  'hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/50 data-[on=true]:text-foreground',
)
const addKey = (e: MouseEvent) => e.metaKey || e.ctrlKey || e.shiftKey

/**
 * The top bar's hosts, which are also the host filter: own hosts ("Mine") and shared sheds
 * (`hosts.<name>.shared`) in two groups. A group label shows that group; a chip shows only that
 * host (again: back to its group); ⌘ / Ctrl / Shift-click adds or removes it (multi-select). Each
 * chip's chevron opens the host's card (state, error, terminal). A shared group carries how many of
 * its sessions wait, since they stay out of the Inbox and the waiting count.
 */
export function HostRail({ list, now }: { list: SessionListState; now: number }) {
  const { fleet, hosts, hostSet, setSelection, hostCounts, sharedWaiting } = list
  if (!hosts.length) return null
  const shown = hostSet ?? new Set(hosts.map((h) => h.name))
  const own = hosts.filter((h) => !isShared(h))
  const shared = hosts.filter(isShared)
  const waiting = [...sharedWaiting.values()].reduce((a, b) => a + b, 0)

  const chips = (list: Host[]) =>
    list.map((h) => (
      <HostChip
        key={h.name}
        host={h}
        on={shown.has(h.name)}
        count={hostCounts.get(h.name) ?? 0}
        waiting={sharedWaiting.get(h.name) ?? 0}
        self={h.name === fleet?.self}
        now={now}
        onPick={(add) => setSelection(clickHost(shown, h.name, hosts, add))}
      />
    ))
  const groupOn = (list: Host[]) => list.every((h) => shown.has(h.name))
  const groupLabel = (scope: 'mine' | 'shared', list: Host[], text: string, title: string) => (
    <button
      type="button"
      data-on={groupOn(list)}
      className={label}
      title={`${title} — ⌘-click to add or remove`}
      aria-pressed={groupOn(list)}
      onClick={(e) => setSelection(clickGroup(shown, scope, hosts, addKey(e)))}
    >
      {scope === 'shared' ? <UsersIcon className="size-3" /> : null}
      {text}
      {scope === 'shared' && waiting > 0 ? (
        <span className="rounded-full bg-status-waiting/15 px-1.5 text-[0.625rem] font-semibold text-status-waiting tabular-nums normal-case" title={`${waiting} waiting on shared sheds`}>
          {waiting}
        </span>
      ) : null}
    </button>
  )

  if (!shared.length) {
    return (
      <nav aria-label="Hosts" className={group}>
        {chips(own)}
      </nav>
    )
  }
  return (
    <div className="flex shrink-0 items-center gap-1.5">
      {own.length ? (
        <nav aria-label="My hosts" className={group}>
          {groupLabel('mine', own, 'Mine', 'Show my hosts')}
          {chips(own)}
        </nav>
      ) : null}
      <nav aria-label="Shared sheds" className={cn(group, 'border-dashed')}>
        {groupLabel('shared', shared, 'Shared', 'Show the shared sheds')}
        {chips(shared)}
      </nav>
    </div>
  )
}

function HostChip({
  host: h,
  on,
  count,
  waiting,
  self,
  now,
  onPick,
}: {
  host: Host
  on: boolean
  count: number
  waiting: number
  self: boolean
  now: number
  onPick: (add: boolean) => void
}) {
  const down = h.ok === false
  const title = `${h.name}: ${down ? 'unreachable' : `${count} session${count === 1 ? '' : 's'}${waiting ? `, ${waiting} waiting` : ''}`} — click to show only it, ⌘-click to add`
  return (
    <div
      className={cn(
        'group/chip flex h-7 items-center rounded-md transition-colors',
        on ? 'bg-accent text-accent-foreground' : 'text-dimmer hover:text-foreground',
        down && 'text-destructive ring-1 ring-destructive/60 ring-inset hover:text-destructive',
      )}
    >
      <button
        type="button"
        aria-pressed={on}
        aria-label={title}
        title={title}
        onClick={(e) => onPick(addKey(e))}
        className="flex h-full items-center gap-1.5 rounded-l-md pr-1 pl-2 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
      >
        <HostDot host={h.name} shared={isShared(h)} className={cn(!on && !down && 'opacity-60')} />
        <span className="hidden max-w-24 truncate 2xl:inline">{h.name}</span>
        <span className={cn('text-[0.6875rem] tabular-nums', on ? 'text-muted-foreground' : 'text-dimmer')}>{down ? '!' : count}</span>
      </button>
      <Popover>
        <PopoverTrigger asChild>
          <button
            type="button"
            aria-label={`${h.name}: host details and terminal`}
            title={`${h.name}: details and terminal`}
            className="grid h-full w-4 place-items-center rounded-r-md text-dimmer opacity-60 outline-none group-hover/chip:opacity-100 hover:text-foreground focus-visible:opacity-100 focus-visible:ring-2 focus-visible:ring-ring/50"
          >
            <ChevronDownIcon className="size-3" />
          </button>
        </PopoverTrigger>
        <PopoverContent align="end" className="w-80">
          <HostCard host={h} self={self} now={now} />
        </PopoverContent>
      </Popover>
    </div>
  )
}
