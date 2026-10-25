import { useEffect, useRef, type KeyboardEvent, type Ref } from 'react'
import { NotebookTextIcon, PanelLeftCloseIcon, PanelLeftOpenIcon, PlusIcon, SearchIcon, XIcon } from 'lucide-react'

import { ClosedSection } from '@/components/ClosedSection'
import { DormantSection } from '@/components/DormantSection'
import { FocusToggle } from '@/components/FocusToggle'
import { SessionListSkeleton } from '@/components/SessionListSkeleton'
import type { Session } from '@/api/types'
import { SessionRow } from '@/components/SessionRow'
import { StackGroup } from '@/components/stack/StackGroup'
import { StatusDot } from '@/components/StatusDot'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { ModuleButtons } from '@/components/ModuleNav'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Kbd } from '@/components/ui/kbd'
import { ViewToggle } from '@/components/ViewToggle'
import { SidebarFilters } from '@/components/desktop/SidebarFilters'
import { useNotesHosts } from '@/hooks/useNotes'
import type { SessionListState } from '@/hooks/useSessionList'
import type { ViewMode } from '@/lib/groups'
import { allSessions, stackRuns } from '@/lib/sessions'
import { ariaShortcut, sessionKey, shortcutHint, withHint } from '@/lib/shortcuts'
import { cn } from '@/lib/utils'

export interface SidebarProps {
  list: SessionListState
  now: number
  cursorKey: string | null
  selectedKey: string | null
  searchRef: Ref<HTMLInputElement>
  /** ↑ / ↓ / Enter inside the search field drive the list cursor. */
  onSearchNav: (action: 'next' | 'prev' | 'open') => void
  onNew: () => void
  onCollapse: () => void
  /** List | Board toggle (omitted → no toggle). */
  viewMode?: ViewMode
  onViewMode?: (v: ViewMode) => void
}

/** Desktop left column: header + summary, search, status/host filters, the session list. */
export function Sidebar({ list, now, cursorKey, selectedKey, searchRef, onSearchNav, onNew, onCollapse, viewMode, onViewMode }: SidebarProps) {
  const { fleet, error, query, setQuery, host, view, summary, unreachable, note } = list
  const listRef = useRef<HTMLDivElement>(null)

  // Keep the cursor row visible (j/k past the edge).
  useEffect(() => {
    if (!cursorKey) return
    const el = listRef.current?.querySelector<HTMLElement>(`[data-session-key="${CSS.escape(cursorKey)}"]`)
    el?.scrollIntoView({ block: 'nearest' })
  }, [cursorKey])

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
    <div className="flex h-full min-w-0 flex-col">
      <div data-tauri-drag-region className="shrink-0 space-y-2 border-b px-3 pt-3 pb-2.5">
        {/* The desktop app's traffic lights sit over this row (see index.css → Desktop app). */}
        <div data-tauri-drag-region="deep" className="titlebar-lead flex min-h-8 items-center gap-2 [--titlebar-offset:0.75rem]">
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
          <span
            className={cn('ml-auto truncate text-right text-[0.6875rem] tabular-nums', note.error ? 'text-destructive' : 'text-dimmer')}
            aria-live="polite"
          >
            {note.error ? note.text : null}
          </span>
          <Button size="icon" aria-label="New session" title={withHint('New session', 'new')} onClick={onNew} className="shrink-0 rounded-full">
            <PlusIcon />
          </Button>
          <Button variant="ghost" size="icon" aria-label="Collapse sidebar" title={withHint('Collapse sidebar', 'sidebar')} onClick={onCollapse} className="shrink-0">
            <PanelLeftCloseIcon />
          </Button>
        </div>

        <div className="flex items-center gap-2">
        <div className="relative min-w-0 flex-1">
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
            className="h-9 rounded-lg bg-card pr-9 pl-8 [&::-webkit-search-cancel-button]:hidden"
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
          {viewMode && onViewMode ? <ViewToggle value={viewMode} onChange={onViewMode} labels={false} className="p-px" /> : null}
        </div>

        <SidebarFilters list={list} />
      </div>

      <div ref={listRef} data-session-list className="min-h-0 flex-1 overflow-y-auto overscroll-contain px-2 py-2">
        {error ? (
          <Alert variant="destructive" className="mb-2">
            <AlertDescription>
              {fleet ? `Refresh failed: ${error} — showing last known data` : `Could not reach fleet web: ${error}`}
            </AlertDescription>
          </Alert>
        ) : null}
        {unreachable.map((h) => (
          <Alert key={h.name} variant="destructive" className="mb-2">
            <AlertDescription className="break-words">
              {h.name} unreachable: {h.error || 'no response'}
            </AlertDescription>
          </Alert>
        ))}
        {!fleet ? (
          <SessionListSkeleton />
        ) : view.sessions.length > 0 ? (
          <nav aria-label="Sessions" className="flex flex-col gap-1.5">
            {stackRuns(view.sessions).map((e) => {
              const row = (s: Session, inStack?: boolean) => {
                const key = sessionKey(s)
                return <SessionRow key={key} session={s} now={now} selected={key === selectedKey} cursor={key === cursorKey && key !== selectedKey} inStack={inStack} />
              }
              return e.kind === 'stack' ? (
                <StackGroup
                  key={e.key}
                  stackKey={e.key}
                  sessions={e.sessions}
                  renderRow={(s) => row(s, true)}
                  forceOpen={e.sessions.slice(1).some((s) => [selectedKey, cursorKey].includes(sessionKey(s)))}
                />
              ) : (
                row(e.session)
              )
            })}
          </nav>
        ) : unreachable.length === 0 ? (
          <div className="px-4 py-10 text-center text-sm text-dimmer">
            {allSessions(fleet).length > 0 ? 'Nothing matches.' : 'No Claude sessions running.'}
          </div>
        ) : null}
        {fleet ? <DormantSection now={now} host={host} query={query} desktop className="mt-4" /> : null}
        {fleet ? <ClosedSection now={now} host={host} query={query} desktop className="mt-3" /> : null}
      </div>

      <div className="shrink-0 border-t px-2 py-1.5 text-xs text-dimmer">
        {/* Work modules on the left, helper screens (usage, settings) on the right. Jump and the shortcuts live in ⌘K / ⌘? and Settings. */}
        <nav aria-label="Modules" className="flex items-center gap-1">
          <ModuleButtons size="icon-sm" group="work" />
          <span className="ml-auto" />
          <FocusToggle size="icon-sm" />
          <ModuleButtons size="icon-sm" group="utility" />
        </nav>
      </div>
    </div>
  )
}

/** The collapsed sidebar: a thin rail with the essentials. */
export function SidebarRail({
  waiting,
  onExpand,
  onNew,
  onPalette,
}: {
  waiting: number
  onExpand: () => void
  onNew: () => void
  onPalette: () => void
}) {
  return (
    <div data-tauri-drag-region className="titlebar-rail flex h-full w-12 shrink-0 flex-col items-center gap-1 border-r py-3">
      <Button variant="ghost" size="icon" aria-label="Expand sidebar" title={withHint('Expand sidebar', 'sidebar')} onClick={onExpand}>
        <PanelLeftOpenIcon />
      </Button>
      <Button size="icon" aria-label="New session" title={withHint('New session', 'new')} onClick={onNew} className="rounded-full">
        <PlusIcon />
      </Button>
      <Button variant="ghost" size="icon" aria-label="Jump to a session" title={withHint('Jump to a session', 'palette')} onClick={onPalette}>
        <SearchIcon />
      </Button>
      {waiting > 0 ? (
        <span
          className="mt-1 flex items-center gap-1 text-xs font-semibold text-status-waiting tabular-nums"
          title={`${waiting} need you`}
          aria-label={`${waiting} sessions need you`}
        >
          <StatusDot status="waiting" className="size-2" />
          {waiting}
        </span>
      ) : null}
      <div className="mt-auto flex flex-col items-center gap-1">
        <FocusToggle />
        <ModuleButtons />
      </div>
    </div>
  )
}

/** Link to the notes explorer; nothing when no host has notes. */
export function NotesLink({ className, size = 'icon' }: { className?: string; size?: 'icon' | 'icon-sm' }) {
  const hosts = useNotesHosts()
  if (!hosts.length) return null
  return (
    <Button asChild variant="ghost" size={size} className={cn('text-dimmer hover:text-foreground', className)}>
      <a href="#/notes" aria-label="Notes" title={withHint('Notes', 'notes')}>
        <NotebookTextIcon />
      </a>
    </Button>
  )
}

