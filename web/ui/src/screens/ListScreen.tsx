import { useRef, useState, type TouchEvent } from 'react'
import { EllipsisIcon, MicIcon, PlusIcon, SearchIcon, TriangleAlertIcon, XIcon } from 'lucide-react'
import { useLocation } from 'wouter'

import { GroupedList } from '@/components/board/GroupedList'
import { GroupsStatus } from '@/components/board/GroupsStatus'
import { ClosedSection } from '@/components/ClosedSection'
import { DormantSection } from '@/components/DormantSection'
import { NewSessionDrawer } from '@/components/NewSessionDrawer'
import { MemoRecordFab } from '@/components/MemoRecorder'
import { FocusMenuItem } from '@/components/FocusToggle'
import { MENU_ITEM, ModuleMenuItems } from '@/components/ModuleNav'
import { ScreenHeader } from '@/components/ScreenHeader'
import { SessionListSkeleton } from '@/components/SessionListSkeleton'
import { SessionFilters } from '@/components/SessionFilters'
import { SessionRow } from '@/components/SessionRow'
import { StackGroup } from '@/components/stack/StackGroup'
import { StatusDot } from '@/components/StatusDot'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { DropdownMenu, DropdownMenuContent, DropdownMenuSeparator, DropdownMenuTrigger } from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'
import { ViewToggle } from '@/components/ViewToggle'
import { useBoardColumns, useGroups, useViewMode } from '@/hooks/useGroups'
import { useSettings } from '@/hooks/useSettings'
import { useNow } from '@/hooks/useNow'
import { ALL_HOSTS, useSessionList } from '@/hooks/useSessionList'
import { pullIntent } from '@/lib/gestures'
import { allSessions, stackRuns, findStatusFilter } from '@/lib/sessions'
import { cn } from '@/lib/utils'

export function ListScreen() {
  const now = useNow(1000)
  const list = useSessionList(now)
  const { fleet, error, query, setQuery, status, setStatus, setHostFilter, host, view, summary, unreachable, note } = list
  const [newOpen, setNewOpen] = useState(false)
  const [, navigate] = useLocation()
  const [mode, setMode] = useViewMode()
  const settings = useSettings()
  const board = mode === 'board'
  const groups = useGroups(board)
  const { columns } = useBoardColumns(board, view.sessions, groups.groups, fleet, host, query)
  // The filter rows live in a drawer under the search field, closed by default: swipe down on
  // the header (or tap the handle) opens it, swipe up closes it. Active filters show on the handle.
  const [filtersOpen, setFiltersOpen] = useState(false)
  const filterSummary = [status !== 'all' ? findStatusFilter(status).label : null, host].filter(Boolean).join(' · ')
  const clearFilters = () => {
    setStatus('all')
    setHostFilter(ALL_HOSTS)
  }
  const touch = useRef<{ x: number; y: number } | null>(null)
  const onTouchStart = (e: TouchEvent) => {
    // The search field keeps its own gestures (caret, selection).
    touch.current = e.target instanceof HTMLInputElement ? null : { x: e.touches[0].clientX, y: e.touches[0].clientY }
  }
  const onTouchEnd = (e: TouchEvent, onHandle: boolean) => {
    const t = touch.current
    touch.current = null
    if (!t) return
    const intent = pullIntent(e.changedTouches[0].clientX - t.x, e.changedTouches[0].clientY - t.y)
    if (intent === 'open') setFiltersOpen(true)
    else if (intent === 'close') setFiltersOpen(false)
    else if (intent === 'tap' && onHandle) setFiltersOpen((o) => !o)
  }

  return (
    <div className="flex min-h-app flex-col overflow-x-clip">
      <ScreenHeader>
        <div onTouchStart={onTouchStart} onTouchEnd={(e) => onTouchEnd(e, false)}>
        <div className="flex min-h-8 items-center gap-0.5 min-[400px]:gap-1 sm:gap-2.5">
          <h1 className="mr-1 shrink-0 text-xl font-bold tracking-tight sm:mr-0">Fleet</h1>
          {fleet ? (
            <div className="flex min-w-0 items-center gap-2 overflow-hidden text-xs text-muted-foreground tabular-nums" aria-label="Status summary">
              {summary.waiting > 0 ? (
                <span className="flex items-center gap-1 font-semibold text-status-waiting">
                  <StatusDot status="waiting" className="size-2" />
                  {summary.waiting}
                </span>
              ) : null}
              <span className="flex items-center gap-1">
                <StatusDot status="busy" className="size-2" />
                {summary.busy}
              </span>
              <span className="flex items-center gap-1">
                <StatusDot status="idle" className="size-2" />
                {summary.idle}
              </span>
            </div>
          ) : null}
          {/* Freshness lives in Settings → Status; the header only flags trouble. */}
          <span className="ml-auto" />
          {note.error || unreachable.length ? (
            <Button asChild variant="ghost" size="icon" className="-my-1 size-8 shrink-0 rounded-full min-[400px]:size-9 sm:size-10 text-destructive [&_svg:not([class*='size-'])]:size-5">
              <a
                href="#/settings"
                role="status"
                aria-label={note.error ? `Offline — ${note.text}` : `${unreachable.map((h) => h.name).join(', ')} unreachable`}
                title={note.error ? note.text : `${unreachable.map((h) => h.name).join(', ')} unreachable`}
              >
                <TriangleAlertIcon />
              </a>
            </Button>
          ) : null}
          {/* The rest of the screens sit behind one button: the header row is narrow on a phone. */}
          <DropdownMenu modal={false}>
            <DropdownMenuTrigger asChild>
              <Button
                variant="ghost"
                size="icon"
                aria-label="More"
                title="More"
                className="-my-1 size-8 shrink-0 rounded-full min-[400px]:size-9 sm:size-10 text-muted-foreground [&_svg:not([class*='size-'])]:size-5"
              >
                <EllipsisIcon />
              </Button>
            </DropdownMenuTrigger>
            <DropdownMenuContent align="end" className="w-60 p-1.5">
              {/* The modules (the operator is the mic button), then the helpers set apart. */}
              <ModuleMenuItems group="work" exclude={['operator']} />
              <DropdownMenuSeparator className="my-1.5" />
              <FocusMenuItem className={MENU_ITEM} />
              <ModuleMenuItems group="utility" />
            </DropdownMenuContent>
          </DropdownMenu>
        </div>

        <div className="mt-2 flex items-center gap-2">
        <div className="relative min-w-0 flex-1">
          <SearchIcon className="pointer-events-none absolute top-1/2 left-3 size-4 -translate-y-1/2 text-dimmer" />
          <Input
            type="search"
            inputMode="search"
            autoComplete="off"
            autoCorrect="off"
            autoCapitalize="none"
            spellCheck={false}
            placeholder="Search name, title, cwd…"
            aria-label="Search sessions"
            value={query}
            onChange={(e) => setQuery(e.target.value)}
            className="h-11 rounded-xl bg-card pr-10 pl-9 text-base md:text-base [&::-webkit-search-cancel-button]:hidden"
          />
          {query ? (
            <button
              type="button"
              aria-label="Clear search"
              onClick={() => setQuery('')}
              className="absolute inset-y-0 right-0 grid w-11 place-items-center text-dimmer"
            >
              <XIcon className="size-4" />
            </button>
          ) : null}
        </div>
          <ViewToggle value={mode} onChange={setMode} size="lg" />
        </div>

        <div
          className={cn('grid transition-[grid-template-rows] duration-200 ease-out', filtersOpen ? 'grid-rows-[1fr]' : 'grid-rows-[0fr]')}
          inert={!filtersOpen}
        >
        <div className="min-h-0 overflow-hidden">
        <div className="mt-2">
          <SessionFilters list={list} spread touch />
        </div>

        {board ? <GroupsStatus state={groups} now={now} size="lg" className="mt-1 -mb-1 justify-between" /> : null}
        </div>
        </div>

        <div className="relative -mb-1.5 flex min-h-6 items-center justify-center">
          <button
            type="button"
            aria-label={filtersOpen ? 'Hide filters' : 'Show filters'}
            aria-expanded={filtersOpen}
            className="flex h-6 min-w-0 flex-1 touch-none items-center justify-center gap-2 outline-none"
            onTouchStart={(e) => {
              e.stopPropagation()
              touch.current = { x: e.touches[0].clientX, y: e.touches[0].clientY }
            }}
            onTouchEnd={(e) => {
              e.stopPropagation()
              e.preventDefault() // no synthetic click after a touch — the touch already decided
              onTouchEnd(e, true)
            }}
            onClick={() => setFiltersOpen((o) => !o)}
          >
            <span className={cn('h-1 w-9 shrink-0 rounded-full transition-colors', filtersOpen ? 'bg-muted-foreground/60' : 'bg-muted-foreground/30')} />
            {filterSummary && !filtersOpen ? (
              <span className="min-w-0 truncate text-xs font-medium text-primary">
                {filterSummary}
              </span>
            ) : null}
          </button>
          {filterSummary && !filtersOpen ? (
            <button
              type="button"
              aria-label="Clear filters"
              title="Clear filters"
              onClick={clearFilters}
              className="relative -mr-1 grid size-6 shrink-0 place-items-center rounded-full text-dimmer after:absolute after:-inset-2.5 active:text-foreground"
            >
              <XIcon className="size-3.5" />
            </button>
          ) : null}
        </div>
        </div>
      </ScreenHeader>

      <main className="flex-1 pb-safe px-safe">
        <div className={cn('mx-auto w-full max-w-3xl px-3 pt-2', settings.operator && settings.memos ? 'pb-52' : settings.operator || settings.memos ? 'pb-36' : 'pb-20')}>
          {error ? (
            <Alert variant="destructive" className="my-2">
              <AlertDescription>
                {fleet ? `Refresh failed: ${error} — showing last known data` : `Could not reach fleet web: ${error}`}
              </AlertDescription>
            </Alert>
          ) : null}

          {unreachable.map((h) => (
            <Alert key={h.name} variant="destructive" className="my-2">
              <AlertDescription className="break-words">
                {h.name} unreachable: {h.error || 'no response'}
              </AlertDescription>
            </Alert>
          ))}

          {!fleet ? (
            <SessionListSkeleton />
          ) : board && columns.length > 0 ? (
            <GroupedList columns={columns} now={now} />
          ) : view.sessions.length > 0 ? (
            <div className="flex flex-col gap-2">
              {stackRuns(view.sessions).map((e) =>
                e.kind === 'stack' ? (
                  <StackGroup
                    key={e.key}
                    stackKey={e.key}
                    sessions={e.sessions}
                    renderRow={(s) => <SessionRow key={`${s.host}/${s.session_id}`} session={s} now={now} inStack />}
                  />
                ) : (
                  <SessionRow key={`${e.session.host}/${e.session.session_id}`} session={e.session} now={now} />
                ),
              )}
            </div>
          ) : unreachable.length === 0 ? (
            <EmptyState anySessions={allSessions(fleet).length > 0} />
          ) : null}

          {fleet ? <DormantSection now={now} host={host} query={query} className="mt-6" /> : null}
          {fleet ? <ClosedSection now={now} host={host} query={query} className="mt-4" /> : null}
        </div>
      </main>

      {/* Thumb actions, stacked bottom-right: record a memo, talk to the operator, new session. */}
      <div className="fixed right-[max(1rem,env(safe-area-inset-right))] bottom-[max(1.25rem,calc(env(safe-area-inset-bottom)+0.5rem))] z-30 flex flex-col items-end gap-3">
        <MemoRecordFab />
        {settings.operator ? (
          <a
            href="#/operator"
            aria-label="Talk to the operator"
            title="Operator — talk to the fleet"
            className="grid size-12 place-items-center rounded-full border bg-background text-foreground shadow-lg transition-transform active:scale-95"
          >
            <MicIcon className="size-5" />
          </a>
        ) : null}
        <button
          type="button"
          aria-label="New session"
          title="New session"
          onClick={() => setNewOpen(true)}
          className="grid size-12 place-items-center rounded-full bg-primary text-primary-foreground shadow-lg transition-transform active:scale-95"
        >
          <PlusIcon className="size-6" />
        </button>
      </div>
      <NewSessionDrawer open={newOpen} onOpenChange={setNewOpen} onOpenSession={(href) => navigate(href)} />
    </div>
  )
}

function EmptyState({ anySessions }: { anySessions: boolean }) {
  return (
    <div className="px-4 py-10 text-center text-sm text-dimmer">
      {anySessions ? 'Nothing matches.' : 'No Claude sessions running.'}
    </div>
  )
}
