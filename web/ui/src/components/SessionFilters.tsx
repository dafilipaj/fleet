import { useRef } from 'react'
import { ListFilterIcon, ServerIcon, UsersIcon } from 'lucide-react'

import { HostDot } from '@/components/HostBadge'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuCheckboxItem,
  DropdownMenuContent,
  DropdownMenuLabel,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group'
import type { SessionListState } from '@/hooks/useSessionList'
import { isShared, normalizeSelection, selectionLabel, type HostScope } from '@/lib/hosts'
import { STATUS_FILTERS, visibleStatusFilters, type StatusFilterId } from '@/lib/sessions'
import { cn } from '@/lib/utils'

/** The kinds of session the segmented toggle switches between; the rest are status filters. */
const KINDS: StatusFilterId[] = ['scheduled', 'slack', 'embedded']
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
 * Sessions | Bots (| Slack | Apps) toggle, then status and host pickers behind one icon each.
 * `spread`: the pickers sit at the row's right end. `touch`: phone-sized (the mobile list).
 * `hostPicker={false}`: no host menu (the desktop top bar has the HostRail instead).
 */
export function SessionFilters({ list, spread = false, touch = false, hostPicker = true }: { list: SessionListState; spread?: boolean; touch?: boolean; hostPicker?: boolean }) {
  const seg = cn(segment, touch && TOUCH.segment)
  const trig = cn(trigger, touch && TOUCH.trigger)
  const item = touch ? TOUCH.item : undefined
  const { fleet, status, setStatus, hostNames, view } = list
  const counts = fleet ? view.counts : null
  const kind = KINDS.includes(status) ? status : SESSIONS
  // The status picked under Sessions, restored when coming back from Bots / Slack / Apps.
  const lastStatus = useRef<StatusFilterId>('all')
  const pickKind = (v: string) => {
    if (!v || v === kind) return
    if (kind === SESSIONS) lastStatus.current = status
    setStatus(v === SESSIONS ? lastStatus.current : (v as StatusFilterId))
  }

  const statusFilters = STATUS_FILTERS.filter((f) => !KINDS.includes(f.id))
  const kinds = visibleStatusFilters(counts, status).filter((f) => f.id === 'slack' || f.id === 'embedded')
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

      {hostPicker && hostNames.length > 1 ? <HostMenu list={list} trig={trig} item={item} touch={touch} /> : null}
    </div>
  )
}

/**
 * The host filter as a menu (the mobile list): Mine / Shared / All, then every host as a checkbox
 * (multi-select; the menu stays open while ticking). Shared sheds are marked and listed last.
 */
function HostMenu({ list, trig, item, touch }: { list: SessionListState; trig: string; item?: string; touch: boolean }) {
  const { selection, setSelection, hosts, hostSet, hostCounts } = list
  const shown = hostSet ?? new Set(hosts.map((h) => h.name))
  const norm = normalizeSelection(shown, hosts)
  const scope = 'scope' in norm ? norm.scope : ''
  const text = selectionLabel(selection, hosts)
  const anyShared = hosts.some(isShared)
  const toggle = (name: string, on: boolean) => {
    const next = new Set(shown)
    if (on) next.add(name)
    else if (next.size > 1) next.delete(name)
    setSelection(normalizeSelection(next, hosts))
  }
  const ordered = [...hosts.filter((h) => !isShared(h)), ...hosts.filter(isShared)]
  return (
    <DropdownMenu>
      <DropdownMenuTrigger asChild>
        <Button variant="ghost" className={trig} data-active={text != null} aria-label={`Filter by host: ${text ?? 'Mine'}`} title="Filter by host">
          {shown.size === 1 ? <HostDot host={[...shown][0]} /> : <ServerIcon className={touch ? 'size-4' : 'size-3.5'} />}
          {text}
        </Button>
      </DropdownMenuTrigger>
      <DropdownMenuContent align="end" className="min-w-48">
        <DropdownMenuLabel className="text-xs text-dimmer">Hosts</DropdownMenuLabel>
        {anyShared ? (
          <>
            <DropdownMenuRadioGroup value={scope} onValueChange={(v) => setSelection({ scope: v as HostScope })}>
              <DropdownMenuRadioItem value="mine" className={item}>Mine</DropdownMenuRadioItem>
              <DropdownMenuRadioItem value="shared" className={item}>
                <UsersIcon className="size-3.5 text-dimmer" />
                Shared
              </DropdownMenuRadioItem>
              <DropdownMenuRadioItem value="all" className={item}>All hosts</DropdownMenuRadioItem>
            </DropdownMenuRadioGroup>
            <DropdownMenuSeparator />
          </>
        ) : null}
        {ordered.map((h) => (
          <DropdownMenuCheckboxItem
            key={h.name}
            checked={shown.has(h.name)}
            onCheckedChange={(on) => toggle(h.name, on === true)}
            onSelect={(e) => e.preventDefault()}
            className={item}
          >
            <HostDot host={h.name} shared={isShared(h)} />
            <span className={cn(h.ok === false && 'text-destructive')}>{h.name}</span>
            <span className={count}>{h.ok === false ? '!' : (hostCounts.get(h.name) ?? 0)}</span>
          </DropdownMenuCheckboxItem>
        ))}
      </DropdownMenuContent>
    </DropdownMenu>
  )
}
