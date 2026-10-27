import { useState } from 'react'
import { CalendarClockIcon, CheckIcon, ExternalLinkIcon, FolderIcon, ListChecksIcon, Loader2Icon, PlayIcon } from 'lucide-react'
import { toast } from 'sonner'
import { useLocation } from 'wouter'

import { api } from '@/api/client'
import type { PullRequest, Session, Task, TasksResponse } from '@/api/types'
import { watchForSpawned } from '@/api/spawnWatch'
import { HostDot } from '@/components/HostBadge'
import { ChecksIcon, PrSessionLink } from '@/components/PrChip'
import { ScheduledList } from '@/components/ScheduledList'
import { Alert, AlertDescription } from '@/components/ui/alert'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Kbd } from '@/components/ui/kbd'
import { Label } from '@/components/ui/label'
import { Textarea } from '@/components/ui/textarea'
import { useFleet } from '@/hooks/useFleet'
import { useNow } from '@/hooks/useNow'
import { useSchedule } from '@/hooks/useSchedule'
import { shortCwd } from '@/lib/format'
import { prHref } from '@/lib/prs'
import { scheduleWhen } from '@/lib/schedule'
import { sessionHref, spawnTargets } from '@/lib/sessions'
import { isMacPlatform, isSubmitChord } from '@/lib/shortcuts'
import { storage } from '@/lib/storage'
import { DUE_TONE, dueInfo, promptChips, statusOptions, statusTone } from '@/lib/tasks'
import { cn } from '@/lib/utils'

// The directory last used for a project's tasks, else for a new session on that host.
const projectDirKey = (host: string, project: string) => `fleet.taskDirLabel.${host}.${project}`
const spawnDirKey = (host: string) => `fleet.spawnDirLabel.${host}`
const SCHEDULE_TIME_KEY = 'fleet.taskScheduleTime'

/**
 * One task's actions on `host`: move it to another status, start a session on it (in one of
 * the host's spawn directories, told what the task is and where it lives) now or on its start
 * date (`fleet schedule --task`), or open it in Productive. Key it by the task so a new task starts with a fresh form.
 */
export function TaskDialog({
  task: t,
  host,
  statuses,
  sessions = [],
  prs = [],
  onClose,
  onChanged,
  onStatus,
}: {
  task: Task | null
  host: string | null
  statuses: TasksResponse['statuses'] | undefined
  /** Sessions already working on it. */
  sessions?: Session[]
  /** PRs that link it. */
  prs?: PullRequest[]
  onClose: () => void
  onChanged: () => void
  onStatus: (t: Task, status: string) => Promise<void>
}) {
  const [, navigate] = useLocation()
  const { fleet, applyFleet } = useFleet()
  const now = useNow(60_000)
  const [prompt, setPrompt] = useState('')
  const [busy, setBusy] = useState<null | 'spawn' | 'status'>(null)
  // Start now, or schedule it for the task's start date (following the date if it moves).
  const [later, setLater] = useState(false)
  const [time, setTime] = useState(() => storage.get(SCHEDULE_TIME_KEY) || '08:00')
  const { pending, reload: reloadSchedule } = useSchedule(Boolean(t))
  const [error, setError] = useState<string | null>(null)
  const [dirLabel, setDirLabel] = useState<string | null>(null)
  if (!t || !host) return null

  const dirs = spawnTargets(fleet).find((h) => h.name === host)?.dirs ?? []
  const remembered = dirLabel ?? storage.get(projectDirKey(host, t.project.id)) ?? storage.get(spawnDirKey(host))
  const dir = dirs.find((d) => d.label === remembered) ?? dirs[0] ?? null
  const options = statusOptions(t, statuses)
  const due = dueInfo(t.dueDate, now)
  const scheduled = pending.filter((e) => e.task?.id === t.id)

  const changeStatus = async (id: string) => {
    if (id === t.status.id || busy) return
    setBusy('status')
    setError(null)
    try {
      await onStatus(t, id)
      toast.success(`#${t.number} → ${options.find((s) => s.id === id)?.name ?? 'moved'}`)
    } catch (err) {
      setError((err as Error)?.message || 'Could not change the status')
    } finally {
      setBusy(null)
    }
  }

  const schedule = async () => {
    if (busy || !dir || !/^\d{1,2}:\d{2}$/.test(time)) return
    setBusy('spawn')
    setError(null)
    try {
      storage.set(projectDirKey(host, t.project.id), dir.label)
      storage.set(SCHEDULE_TIME_KEY, time)
      const res = await api.scheduleAdd(host, { task: t.id, time, dir: dir.path, prompt: prompt.trim() || undefined })
      toast.success(`#${t.number} scheduled on ${res.host}`, { description: scheduleWhen(res.entry) })
      reloadSchedule()
      setPrompt('')
      setLater(false)
    } catch (err) {
      setError((err as Error)?.message || 'Could not schedule it')
    } finally {
      setBusy(null)
    }
  }

  const submit = () => void (later ? schedule() : spawn())

  const spawn = async () => {
    if (busy || !dir) return
    setBusy('spawn')
    setError(null)
    try {
      storage.set(projectDirKey(host, t.project.id), dir.label)
      const res = await api.taskSpawn(host, { task: t.id, dir: dir.path, prompt: prompt.trim() || undefined })
      const name = res.spawn?.name || res.spawn?.tmuxSession || `#${t.number}`
      onClose()
      onChanged()
      const id = toast.loading(`Starting ${name} on ${host}…`, { description: shortCwd(dir.path) })
      watchForSpawned(
        { host: res.spawn?.host || host, name, tmuxSession: res.spawn?.tmuxSession ?? name },
        {
          onFleet: applyFleet,
          onFound: (s) => {
            toast.success(`${name} is up`, { id, description: undefined })
            navigate(sessionHref(s))
          },
          onTimeout: () => toast.error(`${name} has not shown up yet`, { id, description: 'Check the list in a moment' }),
        },
      )
    } catch (err) {
      setError((err as Error)?.message || 'Failed to start')
      setBusy(null)
    }
  }

  return (
    <Dialog open onOpenChange={(o) => (o ? null : onClose())}>
      <DialogContent className="max-h-[90dvh] overflow-y-auto sm:max-w-lg">
        <DialogHeader>
          <DialogDescription className="flex min-w-0 items-center gap-1.5 text-xs">
            <span className="min-w-0 truncate">
              {t.project.name}
              {t.taskList ? ` · ${t.taskList}` : ''}
            </span>
            <span className="shrink-0 font-mono">#{t.number}</span>
          </DialogDescription>
          <DialogTitle className="text-base leading-snug">{t.title}</DialogTitle>
        </DialogHeader>

        <dl className="grid grid-cols-[auto_1fr] items-center gap-x-3 gap-y-1.5 text-sm">
          <dt className="text-muted-foreground">Status</dt>
          <dd className="flex min-w-0 items-center gap-2">
            <select
              value={t.status.id}
              onChange={(e) => void changeStatus(e.target.value)}
              disabled={!!busy || options.length < 2}
              aria-label="Status"
              className={cn(
                'h-8 min-w-0 rounded-lg border bg-card px-2 text-sm outline-none focus-visible:ring-3 focus-visible:ring-ring/50 disabled:opacity-70',
                statusTone(t.status),
              )}
            >
              {options.map((s) => (
                <option key={s.id} value={s.id}>
                  {s.name}
                  {s.category === 'closed' ? ' (closes it)' : ''}
                </option>
              ))}
            </select>
            {busy === 'status' ? <Loader2Icon className="size-4 animate-spin text-muted-foreground" /> : null}
          </dd>
          {due ? (
            <>
              <dt className="text-muted-foreground">Due</dt>
              <dd className={DUE_TONE[due.tone]}>
                {due.label} <span className="text-xs font-normal text-dimmer">({t.dueDate})</span>
              </dd>
            </>
          ) : null}
          {t.startDate ? (
            <>
              <dt className="text-muted-foreground">Start</dt>
              <dd>{t.startDate}</dd>
            </>
          ) : null}
          {t.todos ? (
            <>
              <dt className="text-muted-foreground">Todos</dt>
              <dd className="inline-flex items-center gap-1">
                <ListChecksIcon className="size-3.5 text-muted-foreground" />
                {t.todos - t.openTodos}/{t.todos} done
              </dd>
            </>
          ) : null}
          {t.tags.length ? (
            <>
              <dt className="text-muted-foreground">Tags</dt>
              <dd className="flex flex-wrap gap-1">
                {t.tags.map((g) => (
                  <span key={g} className="rounded border px-1 text-xs text-muted-foreground">
                    {g}
                  </span>
                ))}
              </dd>
            </>
          ) : null}
        </dl>

        {t.description ? <p className="line-clamp-6 rounded-lg border p-2.5 text-sm whitespace-pre-line text-muted-foreground">{t.description}</p> : null}

        {prs.length ? (
          <div className="space-y-1">
            <Label>Pull requests</Label>
            <div className="flex flex-col gap-0.5" onClickCapture={onClose}>
              {prs.map((p) => (
                <a
                  key={p.url}
                  href={`#${prHref(p)}`}
                  className="inline-flex min-w-0 items-center gap-1.5 rounded-md px-1 py-0.5 text-sm hover:bg-muted"
                >
                  <span className="shrink-0 font-mono text-xs text-muted-foreground">
                    {p.repo}#{p.number}
                  </span>
                  <ChecksIcon checks={p.checks} />
                  <span className="min-w-0 truncate">{p.title}</span>
                </a>
              ))}
            </div>
          </div>
        ) : null}

        {sessions.length ? (
          <div className="space-y-1">
            <Label>Sessions on it</Label>
            <div className="flex flex-col gap-0.5" onClickCapture={onClose}>
              {sessions.map((s) => (
                <PrSessionLink key={`${s.host}/${s.session_id}`} session={s} className="py-0.5 text-sm" />
              ))}
            </div>
          </div>
        ) : null}

        {scheduled.length ? (
          <div className="space-y-1">
            <Label>Scheduled</Label>
            <ScheduledList entries={scheduled} now={now} onChanged={reloadSchedule} />
          </div>
        ) : null}

        <div className="space-y-2">
          <div className="flex flex-wrap items-center gap-2">
            <Label htmlFor="task-prompt" className="mr-auto">
              {sessions.length ? 'Start another session' : 'Start a session on it'}
            </Label>
            <div role="radiogroup" aria-label="When" className="inline-flex rounded-lg border p-0.5 text-xs">
              {[
                { on: false, label: 'Now' },
                { on: true, label: 'On start date' },
              ].map((o) => (
                <button
                  key={o.label}
                  type="button"
                  role="radio"
                  aria-checked={later === o.on}
                  onClick={() => setLater(o.on)}
                  className={cn('rounded-md px-2 py-0.5', later === o.on ? 'bg-accent font-medium text-foreground' : 'text-muted-foreground hover:text-foreground')}
                >
                  {o.label}
                </button>
              ))}
            </div>
            {later ? (
              <input
                type="time"
                value={time}
                onChange={(e) => setTime(e.target.value)}
                aria-label="Time on the start date"
                className="h-7 rounded-lg border bg-card px-1.5 text-xs outline-none focus-visible:ring-3 focus-visible:ring-ring/50"
              />
            ) : null}
          </div>
          {dirs.length ? (
            <div role="radiogroup" aria-label="Directory" className="grid grid-cols-1 gap-1 sm:grid-cols-2">
              {dirs.map((d) => {
                const on = d === dir
                return (
                  <button
                    key={d.label}
                    type="button"
                    role="radio"
                    aria-checked={on}
                    onClick={() => setDirLabel(d.label)}
                    title={d.path}
                    className={cn(
                      'flex min-h-9 items-center gap-2 rounded-lg border px-2.5 py-1 text-left text-sm transition-colors',
                      on ? 'border-primary/50 bg-accent' : 'border-border bg-card hover:bg-muted/60',
                    )}
                  >
                    <FolderIcon className={cn('size-3.5 shrink-0', on ? 'text-primary' : 'text-dimmer')} />
                    <span className="min-w-0 flex-1 truncate">{d.label}</span>
                    {on ? <CheckIcon className="size-3.5 shrink-0 text-primary" /> : null}
                  </button>
                )
              })}
            </div>
          ) : (
            <p className="text-sm text-dimmer">{host} advertises no directories — add some in Settings → Start directories.</p>
          )}
          <div className="flex flex-wrap gap-1.5">
            {promptChips(t).map((c) => (
              <button
                key={c.label}
                type="button"
                onClick={() => setPrompt(c.text)}
                className="rounded-full border px-2.5 py-0.5 text-xs text-muted-foreground hover:bg-muted/60 hover:text-foreground"
              >
                {c.label}
              </button>
            ))}
          </div>
          <Textarea
            id="task-prompt"
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
            onKeyDown={(e) => {
              if (isSubmitChord(e)) {
                e.preventDefault()
                submit()
              }
            }}
            rows={3}
            placeholder="What should it do? (optional)"
          />
          <p className="flex items-center gap-1 text-xs text-dimmer">
            <HostDot host={host} />
            Runs on {host}
            {dir ? ` in ${shortCwd(dir.path)}` : ''}
            {later
              ? ` at ${time} on ${t.startDate ? `its start date (${t.startDate})` : 'its start date, once it has one'} — follows the date if it moves.`
              : '; the session is told the task and its link.'}
          </p>
        </div>

        {error ? (
          <Alert variant="destructive">
            <AlertDescription>{error}</AlertDescription>
          </Alert>
        ) : null}

        <div className="flex flex-wrap items-center gap-2">
          <Button asChild variant="ghost" size="sm" className="text-muted-foreground">
            <a href={t.url} target="_blank" rel="noreferrer">
              <ExternalLinkIcon /> Productive
            </a>
          </Button>
          <Button size="sm" className="ml-auto" onClick={submit} disabled={!!busy || !dir}>
            {busy === 'spawn' ? <Loader2Icon className="animate-spin" /> : later ? <CalendarClockIcon /> : <PlayIcon />} {later ? 'Schedule' : 'Start session'}
            <Kbd className="ml-1 hidden bg-primary-foreground/20 text-primary-foreground sm:inline-flex">{isMacPlatform() ? '⌘↵' : 'Ctrl↵'}</Kbd>
          </Button>
        </div>
      </DialogContent>
    </Dialog>
  )
}
