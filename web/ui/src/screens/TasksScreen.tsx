import { useRef, useState } from 'react'
import { ChevronLeftIcon, ListChecksIcon, MoonIcon, RefreshCwIcon, SearchIcon, SquareCheckBigIcon, TriangleAlertIcon } from 'lucide-react'
import { useLocation } from 'wouter'

import type { PullRequest, Session, Task } from '@/api/types'
import { HostDot } from '@/components/HostBadge'
import { ChecksIcon, PrSessionLink } from '@/components/PrChip'
import { ScreenHeader } from '@/components/ScreenHeader'
import { TaskDialog } from '@/components/TaskDialog'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Skeleton } from '@/components/ui/skeleton'
import { useNow } from '@/hooks/useNow'
import { usePersistentState } from '@/hooks/usePersistentState'
import { useSwipeBack } from '@/hooks/useSwipeBack'
import { useTasks } from '@/hooks/useTasks'
import { relTime } from '@/lib/format'
import { prHref } from '@/lib/prs'
import { DUE_TONE, dueInfo, groupTasks, IDLE_DAYS, idleDays, isIdle, parseTaskLocation, statusTone, taskHref, taskProjects, type TaskGroup } from '@/lib/tasks'
import { cn } from '@/lib/utils'

/**
 * `#/tasks`: your open Productive tasks (`fleet tasks` on one host), by column — due soon or
 * overdue, in progress, to do. `pane` = the desktop layout (a board in the pane beside the session
 * list, like the pull requests); `screen` = mobile (the columns stacked, swipe back).
 */
export function TasksScreen({ layout = 'screen' }: { layout?: 'screen' | 'pane' }) {
  const pane = layout === 'pane'
  const [location, navigate] = useLocation()
  const { hosts, host, setHost, data, error, unconfigured, loading, refresh, refreshing, reload, setStatus, links } = useTasks()
  // `#/tasks/<id>`: that task's dialog is open (session task chips link here).
  const sel = parseTaskLocation(location)
  const setOpen = (t: Task) => navigate(taskHref(t))
  const [q, setQ] = useState('')
  const [project, setProject] = usePersistentState<string>('fleet.tasksProject', '')
  const now = useNow(30_000)
  // Tasks nobody touched in IDLE_DAYS are mostly leftovers: hidden unless asked for (remembered).
  const [showIdle, setShowIdle] = usePersistentState<boolean>('fleet.tasksShowIdle', false, (r) => r === 'true')
  const projects = taskProjects(data?.tasks ?? [])
  const projectId = projects.some((p) => p.id === project) ? project : null
  const groups = groupTasks(data, q, { project: projectId, hideIdle: !showIdle, now })
  const idle = data ? data.tasks.filter((t) => isIdle(t, now)).length : 0

  const back = () => {
    if (!pane && window.history.length > 1) window.history.back()
    else navigate('/', { replace: !pane })
  }
  const screenRef = useRef<HTMLDivElement>(null)
  useSwipeBack(screenRef, back, !pane)

  const pill = (on: boolean) =>
    cn(
      'flex shrink-0 items-center gap-1.5 rounded-full border px-2.5 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring/50',
      pane ? 'h-7' : 'h-9',
      on ? 'border-primary/40 bg-primary/10 text-foreground' : 'text-muted-foreground hover:bg-muted/60',
    )

  const refreshButton = (
    <Button
      variant="ghost"
      size="icon"
      aria-label="Refresh tasks"
      title="Fetch from Productive now"
      onClick={() => void refresh()}
      disabled={refreshing || !host}
      className={cn('shrink-0', pane ? 'size-9 rounded-lg' : '-my-1.5 size-11 rounded-xl')}
    >
      <RefreshCwIcon className={cn('size-5', refreshing && 'animate-spin')} />
    </Button>
  )

  const hostPicker =
    hosts.length > 1 ? (
      <div role="radiogroup" aria-label="Host" title="Whose Productive token to use (and where sessions start)" className="no-scrollbar flex gap-1 overflow-x-auto">
        {hosts.map((h) => (
          <button key={h} type="button" role="radio" aria-checked={h === host} onClick={() => setHost(h)} className={pill(h === host)}>
            <HostDot host={h} />
            {h}
          </button>
        ))}
      </div>
    ) : null

  const filter = (
    <div className="relative w-full">
      <SearchIcon className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-muted-foreground" />
      <Input
        type="search"
        value={q}
        onChange={(e) => setQ(e.target.value)}
        placeholder="Filter by title, #123, status, tag"
        aria-label="Filter tasks"
        className="h-8 pl-8"
      />
    </div>
  )

  const projectPicker =
    projects.length > 1 ? (
      <select
        value={projectId ?? ''}
        onChange={(e) => setProject(e.target.value)}
        aria-label="Project"
        className={cn('max-w-48 min-w-0 shrink rounded-lg border bg-card px-2 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring/50', pane ? 'h-8' : 'h-9')}
      >
        <option value="">All projects</option>
        {projects.map((p) => (
          <option key={p.id} value={p.id}>
            {p.name} ({p.count})
          </option>
        ))}
      </select>
    ) : null

  const idleToggle = (
    <button
      type="button"
      aria-pressed={showIdle}
      onClick={() => setShowIdle(!showIdle)}
      title={showIdle ? `Hide tasks with no activity in ${IDLE_DAYS} days` : `Show tasks with no activity in ${IDLE_DAYS} days`}
      className={pill(showIdle)}
    >
      <MoonIcon className="size-3.5" />
      Idle
      {idle ? <span className="text-dimmer tabular-nums">{idle}</span> : null}
    </button>
  )

  const fetched = data ? relTime(data.fetchedAt, now) : ''
  const status = unconfigured ? (
    <div className="space-y-1 rounded-lg border border-dashed p-4 text-sm">
      <p className="font-medium">Productive is not set up on {host}</p>
      <p className="text-muted-foreground">{error}</p>
      <p className="text-xs text-dimmer">
        On {host}: <code className="font-mono">fleet config set tasks.token &lt;API token&gt;</code> and{' '}
        <code className="font-mono">fleet config set tasks.organization &lt;organization id&gt;</code>.
      </p>
    </div>
  ) : (
    <p className={cn('text-xs', data?.stale || error ? 'text-status-waiting' : 'text-dimmer')}>
      {error && !data ? (
        <span className="inline-flex items-start gap-1">
          <TriangleAlertIcon className="mt-px size-3.5 shrink-0" />
          {error}
        </span>
      ) : data ? (
        <>
          {data.stale || error ? <TriangleAlertIcon className="mr-1 inline size-3.5 align-[-2px]" /> : null}
          {fetched ? `Fetched ${fetched} ago` : 'Fetched just now'}
          {data.user?.name ? ` as ${data.user.name}` : ''}
          {data.organization?.name ? ` · ${data.organization.name}` : ''}
          {data.error || error ? ` — ${data.error ?? error}` : ''}
          {data.truncated ? ' · some tasks are missing (hit the page limit)' : ''}
        </>
      ) : null}
    </p>
  )

  const skeleton = (
    <div className="space-y-2">
      {[0, 1, 2].map((i) => (
        <div key={i} className="space-y-2 rounded-lg border p-3">
          <Skeleton className="h-3 w-1/3" />
          <Skeleton className="h-4 w-full" />
        </div>
      ))}
    </div>
  )
  const showSkeleton = loading && !data && !error

  const current = sel ? (data?.tasks.find((t) => t.id === sel) ?? null) : null
  const dialog = current ? (
    <TaskDialog
      key={current.id}
      task={current}
      host={host}
      statuses={data?.statuses}
      sessions={links.byTask.get(current.id) ?? []}
      prs={links.prsByTask.get(current.id) ?? []}
      onClose={() => navigate('/tasks', { replace: true })}
      onChanged={reload}
      onStatus={setStatus}
    />
  ) : null
  const cardProps = {
    now,
    onOpen: setOpen,
    sessionsOf: (t: Task) => links.byTask.get(t.id),
    prsOf: (t: Task) => links.prsByTask.get(t.id),
  }

  if (pane) {
    return (
      <section aria-label="Tasks" className="flex h-full min-w-0 flex-1 flex-col bg-background">
        <header
          data-tauri-drag-region="deep"
          className="titlebar titlebar-lead flex shrink-0 items-center gap-2 border-b py-1.5 pr-2 pl-2 [--titlebar-pad:0.5rem]"
        >
          <Button asChild variant="ghost" size="sm" className="shrink-0 text-muted-foreground">
            <a href="#/" title="Back to the sessions (Esc)">
              <ChevronLeftIcon /> Sessions
            </a>
          </Button>
          <h1 className="flex shrink-0 items-center gap-1.5 text-[0.9375rem] font-bold">
            <SquareCheckBigIcon className="size-4 text-muted-foreground" /> Tasks
          </h1>
          {hostPicker}
          <div className="mx-2 flex max-w-md min-w-0 flex-1">{filter}</div>
          {projectPicker}
          {idleToggle}
          {refreshButton}
        </header>
        <div className="shrink-0 px-4 pt-2">{status}</div>
        <div className="flex min-h-0 flex-1 gap-3 overflow-x-auto px-4 pt-2 pb-4">
          {showSkeleton
            ? [0, 1, 2].map((i) => (
                <div key={i} className="w-80 shrink-0">
                  {skeleton}
                </div>
              ))
            : unconfigured
              ? null
              : groups.map((g) => <Column key={g.id} group={g} {...cardProps} />)}
        </div>
        {dialog}
      </section>
    )
  }

  return (
    <div ref={screenRef} className="flex min-h-app flex-col bg-background">
      <ScreenHeader>
        <div className="-ml-2 flex min-h-8 items-center gap-1">
          <Button variant="ghost" size="icon" aria-label="Back" onClick={back} className="-my-1.5 size-11 shrink-0 rounded-xl">
            <ChevronLeftIcon className="size-6" />
          </Button>
          <h1 className="flex-1 text-xl font-bold tracking-tight">Tasks</h1>
          {refreshButton}
        </div>
        <div className="mt-2 space-y-2">
          {hostPicker}
          <div className="flex items-center gap-2">
            {filter}
            {idleToggle}
          </div>
          {projectPicker}
        </div>
      </ScreenHeader>
      <div className="mx-auto w-full max-w-lg space-y-5 px-4 pt-3 pb-[max(1.5rem,env(safe-area-inset-bottom))] md:max-w-2xl">
        {status}
        {showSkeleton ? skeleton : null}
        {groups
          .filter((g) => g.tasks.length)
          .map((g) => (
            <section key={g.id} aria-label={g.title}>
              <ColumnTitle group={g} />
              <div className="space-y-2">
                {g.tasks.map((t) => (
                  <TaskCard key={t.id} task={t} now={now} onOpen={setOpen} sessions={cardProps.sessionsOf(t)} prs={cardProps.prsOf(t)} />
                ))}
              </div>
            </section>
          ))}
        {data && !groups.some((g) => g.tasks.length) ? (
          <p className="py-8 text-center text-sm text-muted-foreground">{q || projectId ? 'No task matches.' : 'No open tasks.'}</p>
        ) : null}
      </div>
      {dialog}
    </div>
  )
}

function ColumnTitle({ group: g }: { group: TaskGroup }) {
  return (
    <h2 className="mb-2 flex items-baseline gap-2 text-sm font-semibold">
      {g.title}
      <span className="text-xs font-normal text-dimmer tabular-nums">{g.tasks.length}</span>
    </h2>
  )
}

interface CardCtx {
  now: number
  onOpen: (t: Task) => void
  sessionsOf: (t: Task) => Session[] | undefined
  prsOf: (t: Task) => PullRequest[] | undefined
}

function Column({ group: g, now, onOpen, sessionsOf, prsOf }: { group: TaskGroup } & CardCtx) {
  return (
    <section aria-label={g.title} className="flex w-80 shrink-0 flex-col">
      <ColumnTitle group={g} />
      <div className="min-h-0 flex-1 space-y-2 overflow-y-auto pr-1">
        {g.tasks.map((t) => (
          <TaskCard key={t.id} task={t} now={now} onOpen={onOpen} sessions={sessionsOf(t)} prs={prsOf(t)} />
        ))}
        {!g.tasks.length ? <p className="rounded-lg border border-dashed p-3 text-center text-xs text-dimmer">Nothing here</p> : null}
      </div>
    </section>
  )
}

function TaskCard({
  task: t,
  now,
  onOpen,
  sessions,
  prs,
}: {
  task: Task
  now: number
  onOpen: (t: Task) => void
  sessions?: Session[]
  prs?: PullRequest[]
}) {
  const due = dueInfo(t.dueDate, now)
  const idle = idleDays(t, now)
  const activity = t.lastActivityAt ?? t.updatedAt
  return (
    <div className={cn('rounded-lg border bg-card text-sm hover:border-primary/40', due?.tone === 'overdue' && 'border-l-[3px] border-l-status-error')}>
      <button
        type="button"
        onClick={() => onOpen(t)}
        className="block w-full rounded-lg p-3 text-left outline-none hover:bg-muted/40 focus-visible:ring-2 focus-visible:ring-ring/50"
      >
        <div className="mb-1 flex items-center gap-1.5 text-xs text-muted-foreground">
          <span className="min-w-0 truncate" title={t.taskList ? `${t.project.name} · ${t.taskList}` : t.project.name}>
            {t.project.name}
          </span>
          <span className="shrink-0 font-mono">#{t.number}</span>
          <span className="ml-auto shrink-0 tabular-nums" title={activity ? `Last activity ${new Date(activity).toLocaleString()}` : undefined}>
            {activity ? relTime(activity, now) : ''}
          </span>
        </div>
        <div className="line-clamp-2 font-medium text-foreground">{t.title}</div>
        <div className="mt-1.5 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground">
          <span className={cn('rounded border px-1 leading-4', statusTone(t.status))}>{t.status.name}</span>
          {due ? <span className={DUE_TONE[due.tone]}>{due.label}</span> : null}
          {t.todos ? (
            <span className="inline-flex items-center gap-0.5" title={`${t.openTodos} of ${t.todos} todos open`}>
              <ListChecksIcon className="size-3" />
              {t.todos - t.openTodos}/{t.todos}
            </span>
          ) : null}
          {t.tags.slice(0, 3).map((g) => (
            <span key={g} className="text-dimmer">
              {g}
            </span>
          ))}
          {idle != null && idle >= IDLE_DAYS && t.column !== 'due' ? <span className="text-dimmer">idle {idle}d</span> : null}
        </div>
      </button>
      {prs?.length ? (
        <div className="flex flex-wrap gap-1 border-t px-2 py-1.5">
          {prs.map((p) => (
            <a
              key={p.url}
              href={`#${prHref(p)}`}
              title={`${p.owner}/${p.repo}#${p.number}: ${p.title}`}
              className="inline-flex h-5 items-center gap-0.5 rounded-[5px] border px-1 font-mono text-[0.6875rem] text-muted-foreground hover:bg-muted"
            >
              {p.repo}#{p.number}
              <ChecksIcon checks={p.checks} className="size-3" />
            </a>
          ))}
        </div>
      ) : null}
      {sessions?.length ? (
        <div className="flex flex-col gap-0.5 border-t px-2 py-1.5">
          {sessions.map((s) => (
            <PrSessionLink key={`${s.host}/${s.session_id}`} session={s} className="text-muted-foreground" />
          ))}
        </div>
      ) : null}
    </div>
  )
}
