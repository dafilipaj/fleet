import type { KeyboardEvent, Ref } from 'react'
import { PlusIcon, SearchIcon, XIcon } from 'lucide-react'

import { GroupsStatus } from '@/components/board/GroupsStatus'
import { HostShellButtons } from '@/components/desktop/HostShellButtons'
import { FocusToggle } from '@/components/FocusToggle'
import { ModuleButtons } from '@/components/ModuleNav'
import { SessionFilters } from '@/components/SessionFilters'
import { StatusDot } from '@/components/StatusDot'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Kbd } from '@/components/ui/kbd'
import { ViewToggle } from '@/components/ViewToggle'
import type { GroupsState } from '@/hooks/useGroups'
import type { SessionListState } from '@/hooks/useSessionList'
import type { ViewMode } from '@/lib/groups'
import { ariaShortcut, shortcutHint, withHint } from '@/lib/shortcuts'

export interface TopBarProps {
  list: SessionListState
  now: number
  view: ViewMode
  onView: (v: ViewMode) => void
  searchRef: Ref<HTMLInputElement>
  /** ↑ / ↓ / Enter inside the search field drive the list / board cursor. */
  onSearchNav: (action: 'next' | 'prev' | 'open') => void
  onNew: () => void
  /** The Board's last grouping run + Regroup (omitted on the List). */
  groups?: GroupsState
}

/**
 * The desktop top bar, the same over the List and the Board: summary, List | Board, search,
 * session filters, modules, New. The desktop app's traffic lights sit over its left end.
 */
export function TopBar({ list, now, view, onView, searchRef, onSearchNav, onNew, groups }: TopBarProps) {
  const { fleet, query, setQuery, summary, note } = list

  const onSearchKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.nativeEvent.isComposing) return
    if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
      e.preventDefault()
      onSearchNav(e.key === 'ArrowDown' ? 'next' : 'prev')
    } else if (e.key === 'Enter') {
      e.preventDefault()
      onSearchNav('open')
    } else if (e.key === 'Escape' && query) {
      // First Esc clears, the second leaves the field (window handler).
      e.preventDefault()
      e.stopPropagation()
      setQuery('')
    }
  }

  return (
    <header
      data-tauri-drag-region="deep"
      className="titlebar titlebar-lead flex shrink-0 flex-wrap items-center gap-x-3 gap-y-2 border-b px-4 py-2.5 [--titlebar-pad:1rem]"
    >
      <h1 className="text-lg font-bold tracking-tight">Fleet</h1>
      {fleet ? (
        <div className="flex items-center gap-2 text-xs text-muted-foreground tabular-nums" aria-label="Status summary">
          {summary.waiting > 0 ? (
            <span className="flex items-center gap-1 font-semibold text-status-waiting" title="Needs you">
              <StatusDot status="waiting" className="size-2" />
              {summary.waiting}
            </span>
          ) : null}
          <span className="flex items-center gap-1" title="Busy">
            <StatusDot status="busy" className="size-2" />
            {summary.busy}
          </span>
          <span className="flex items-center gap-1" title="Idle">
            <StatusDot status="idle" className="size-2" />
            {summary.idle}
          </span>
        </div>
      ) : null}
      <ViewToggle value={view} onChange={onView} />

      <div className="relative w-64 min-w-40">
        <SearchIcon className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-dimmer" />
        <Input
          ref={searchRef}
          type="search"
          autoComplete="off"
          autoCorrect="off"
          autoCapitalize="none"
          spellCheck={false}
          placeholder="Search name, title, cwd…"
          aria-label="Search sessions"
          aria-keyshortcuts={ariaShortcut('search')}
          value={query}
          onChange={(e) => setQuery(e.target.value)}
          onKeyDown={onSearchKeyDown}
          className="h-8 rounded-lg bg-card pr-9 pl-8 [&::-webkit-search-cancel-button]:hidden"
        />
        {query ? (
          <button
            type="button"
            aria-label="Clear search"
            onClick={() => setQuery('')}
            className="absolute inset-y-0 right-0 grid w-9 place-items-center rounded-r-lg text-dimmer outline-none hover:text-foreground focus-visible:ring-2 focus-visible:ring-ring/50"
          >
            <XIcon className="size-4" />
          </button>
        ) : (
          <Kbd className="absolute top-1/2 right-2 -translate-y-1/2">{shortcutHint('search')}</Kbd>
        )}
      </div>

      <SessionFilters list={list} />

      <div className="ml-auto flex min-w-0 items-center gap-2">
        {groups ? <GroupsStatus state={groups} now={now} quiet /> : null}
        {note.error ? <span className="hidden truncate text-[0.6875rem] text-destructive tabular-nums xl:inline">{note.text}</span> : null}
        {/* Modules you work in, then the helpers (a terminal per host, focus, usage, settings) set apart. */}
        <nav aria-label="Modules" className="flex shrink-0 items-center gap-0.5 rounded-lg border border-border bg-card p-0.5">
          <ModuleButtons size="icon-sm" group="work" className="text-muted-foreground" />
        </nav>
        <span aria-hidden className="h-5 w-px shrink-0 bg-border" />
        <div className="flex shrink-0 items-center gap-0.5">
          <HostShellButtons />
          <FocusToggle size="icon-sm" />
          <ModuleButtons size="icon-sm" group="utility" />
        </div>
        <Button size="icon" aria-label="New session" title={withHint('New session', 'new')} onClick={onNew} className="shrink-0 rounded-full">
          <PlusIcon />
        </Button>
      </div>
    </header>
  )
}
