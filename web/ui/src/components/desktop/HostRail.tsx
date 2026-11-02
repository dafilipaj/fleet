import { useState, type MouseEvent } from 'react'

import type { Host } from '@/api/types'
import { HostCard } from '@/components/desktop/HostCard'
import { HostDot } from '@/components/HostBadge'
import { Popover, PopoverAnchor, PopoverContent } from '@/components/ui/popover'
import type { SessionListState } from '@/hooks/useSessionList'
import { clickHost, isShared } from '@/lib/hosts'
import { cn } from '@/lib/utils'

const group = 'flex shrink-0 items-center gap-0.5 rounded-lg border border-border bg-card p-0.5'
const addKey = (e: MouseEvent) => e.metaKey || e.ctrlKey || e.shiftKey

/**
 * The top bar's hosts, which are also the host filter: a compact chip per host (colour mark +
 * session count), own hosts in one group, shared sheds (`hosts.<name>.shared`: a hollow diamond)
 * in a dashed one. Click: only that host (again: back to its group, so Mine / Shared are a double
 * click away); ⌘ / Ctrl / Shift-click adds or removes it (multi-select). Right-click or ⌥-click
 * opens the host card (state, error, terminal). A shared chip carries how many of its sessions
 * wait, since they stay out of the Inbox and the waiting count.
 */
export function HostRail({ list, now }: { list: SessionListState; now: number }) {
  const { fleet, hosts, hostSet, setSelection, hostCounts, sharedWaiting } = list
  if (!hosts.length) return null
  const shown = hostSet ?? new Set(hosts.map((h) => h.name))
  const own = hosts.filter((h) => !isShared(h))
  const shared = hosts.filter(isShared)

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

  return (
    <div className="flex shrink-0 items-center gap-1">
      {own.length ? (
        <nav aria-label={shared.length ? 'My hosts' : 'Hosts'} className={group}>
          {chips(own)}
        </nav>
      ) : null}
      {shared.length ? (
        <nav aria-label="Shared sheds" className={cn(group, 'border-dashed')}>
          {chips(shared)}
        </nav>
      ) : null}
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
  const [open, setOpen] = useState(false)
  const down = h.ok === false
  const state = down ? 'unreachable' : `${count} session${count === 1 ? '' : 's'}${waiting ? `, ${waiting} waiting` : ''}`
  const title = `${h.name}${isShared(h) ? ' (shared)' : ''}: ${state}\nClick: only this host · ⌘-click: add · right-click: details & terminal`
  return (
    <Popover open={open} onOpenChange={setOpen}>
      <PopoverAnchor asChild>
        <button
          type="button"
          aria-pressed={on}
          aria-label={`${h.name}: ${state}`}
          title={title}
          onClick={(e) => (e.altKey ? setOpen(true) : onPick(addKey(e)))}
          onContextMenu={(e) => {
            e.preventDefault()
            setOpen(true)
          }}
          className={cn(
            'relative flex h-7 items-center gap-1.5 rounded-md px-2 text-[0.6875rem] tabular-nums outline-none transition-colors focus-visible:ring-2 focus-visible:ring-ring/50',
            on ? 'bg-accent text-accent-foreground' : 'text-dimmer hover:text-foreground',
            down && 'text-destructive ring-1 ring-destructive/60 ring-inset hover:text-destructive',
          )}
        >
          <HostDot host={h.name} shared={isShared(h)} className={cn(!on && !down && 'opacity-60')} />
          {down ? '!' : count}
          {waiting > 0 ? (
            <span aria-hidden className="absolute top-0.5 right-0.5 size-1.5 rounded-full bg-status-waiting ring-1 ring-card" />
          ) : null}
        </button>
      </PopoverAnchor>
      <PopoverContent align="end" className="w-80">
        <HostCard host={h} self={self} now={now} />
      </PopoverContent>
    </Popover>
  )
}
