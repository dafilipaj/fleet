import { useRef } from 'react'
import { ListFilterIcon, ServerIcon } from 'lucide-react'

import { HostDot } from '@/components/HostBadge'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group'
import { ALL_HOSTS, type SessionListState } from '@/hooks/useSessionList'
import { STATUS_FILTERS, visibleStatusFilters, type StatusFilterId } from '@/lib/sessions'
import { cn } from '@/lib/utils'

/** The kinds of session the segmented toggle switches between; the rest are status filters. */
const KINDS: StatusFilterId[] = ['scheduled', 'embedded']
const SESSIONS = 'sessions'

const segment = 'h-7 gap-1 rounded-md px-2.5 text-xs text-muted-foreground data-[state=on]:bg-accent data-[state=on]:text-accent-foreground'
const trigger = cn(
  'h-8 shrink-0 gap-1 rounded-lg border border-border bg-card px-2 text-xs font-normal text-muted-foreground',
  'data-[active=true]:border-primary/50 data-[active=true]:bg-accent data-[active=true]:text-accent-foreground',
)
const count = 'ml-auto pl-3 text-[0.6875rem] text-dimmer tabular-nums'
/** `touch` (the mobile list): finger-sized segments, triggers and menu rows. */
const TOUCH = {
  segment: 'h-8 px-3 text-sm',
  trigger: 'h-10 gap-1.5 px-3 text-sm [&_svg:not([class*=size-])]:size-4',
  item: 'min-h-11 gap-3 px-3 text-[0.9375rem]',
}

/**
 * Sessions | Bots (| Apps) toggle, then status and host pickers behind one icon each.
 * `spread`: the pickers sit at the row's right end. `touch`: phone-sized (the mobile list).
 */
export function SessionFilters({ list, spread = false, touch = false }: { list: SessionListState; spread?: boolean; touch?: boolean }) {
  const seg = cn(segment, touch && TOUCH.segment)
  const trig = cn(trigger, touch && TOUCH.trigger)
  const item = touch ? TOUCH.item : undefined
  const { fleet, status, setStatus, setHostFilter, hosts, hostNames, host, view, hostCounts } = list
  const counts = fleet ? view.counts : null
  const kind = KINDS.includes(status) ? status : SESSIONS
  // The status picked under Sessions, restored when coming back from Bots / Apps.
  const lastStatus = useRef<StatusFilterId>('all')
  const pickKind = (v: string) => {
    if (!v || v === kind) return
    if (kind === SESSIONS) lastStatus.current = status
    setStatus(v === SESSIONS ? lastStatus.current : (v as StatusFilterId))
  }

  const statusFilters = STATUS_FILTERS.filter((f) => !KINDS.includes(f.id))
  const kinds = visibleStatusFilters(counts, status).filter((f) => f.id === 'embedded')
  const current = statusFilters.find((f) => f.id === status)
  const statusActive = kind === SESSIONS && status !== 'all'

  return (
    <div className="flex items-center gap-1.5">
      <ToggleGroup
        type="single"
        value={kind}
        onValueChange={pickKind}
        aria-label="Sessions or bots"
        spacing={0}
        className={cn("shrink-0 rounded-lg border border-border bg-card p-0.5", touch && "p-1")}
      >
        <ToggleGroupItem value={SESSIONS} className={seg}>
          Sessions
        </ToggleGroupItem>
        <ToggleGroupItem value="scheduled" className={seg} title="Sessions started by bots">
          Bots
          {counts?.scheduled ? <span className="text-[0.6875rem] text-dimmer tabular-nums">{counts.scheduled}</span> : null}
        </ToggleGroupItem>
        {kinds.map((f) => (
          <ToggleGroupItem key={f.id} value={f.id} className={seg} title={f.title}>
            {f.label}
            <span className="text-[0.6875rem] text-dimmer tabular-nums">{counts?.[f.id] ?? ''}</span>
          </ToggleGroupItem>
        ))}
      </ToggleGroup>

      {spread ? <span className="ml-auto" /> : null}

      <DropdownMenu>
        <DropdownMenuTrigger asChild disabled={kind !== SESSIONS}>
          <Button
            variant="ghost"
            className={trig}
            data-active={statusActive}
            aria-label={`Filter by status: ${current?.label ?? 'All'}`}
            title="Filter by status"
          >
            <ListFilterIcon className={touch ? "size-4" : "size-3.5"} />
            {statusActive && current ? current.label : null}
          </Button>
        </DropdownMenuTrigger>
        <DropdownMenuContent align="end" className="min-w-40">
          <DropdownMenuLabel className="text-xs text-dimmer">Status</DropdownMenuLabel>
          <DropdownMenuRadioGroup value={status} onValueChange={(v) => setStatus(v as StatusFilterId)}>
            {statusFilters.map((f) => (
              <DropdownMenuRadioItem key={f.id} value={f.id} className={item}>
                {f.label}
                <span className={count}>{counts?.[f.id] ?? ''}</span>
              </DropdownMenuRadioItem>
            ))}
          </DropdownMenuRadioGroup>
        </DropdownMenuContent>
      </DropdownMenu>

      {hostNames.length > 1 ? (
        <DropdownMenu>
          <DropdownMenuTrigger asChild>
            <Button
              variant="ghost"
              className={trig}
              data-active={host != null}
              aria-label={`Filter by host: ${host ?? 'All'}`}
              title="Filter by host"
            >
              {host ? <HostDot host={host} /> : <ServerIcon className={touch ? "size-4" : "size-3.5"} />}
              {host ?? null}
            </Button>
          </DropdownMenuTrigger>
          <DropdownMenuContent align="end" className="min-w-44">
            <DropdownMenuLabel className="text-xs text-dimmer">Host</DropdownMenuLabel>
            <DropdownMenuRadioGroup value={host ?? ALL_HOSTS} onValueChange={setHostFilter}>
              <DropdownMenuRadioItem value={ALL_HOSTS} className={item}>All hosts</DropdownMenuRadioItem>
              {hosts.map((h) => (
                <DropdownMenuRadioItem key={h.name} value={h.name} className={item}>
                  <HostDot host={h.name} />
                  <span className={cn(h.ok === false && 'text-destructive')}>{h.name}</span>
                  <span className={count}>{h.ok === false ? '!' : (hostCounts.get(h.name) ?? 0)}</span>
                </DropdownMenuRadioItem>
              ))}
            </DropdownMenuRadioGroup>
          </DropdownMenuContent>
        </DropdownMenu>
      ) : null}
    </div>
  )
}
