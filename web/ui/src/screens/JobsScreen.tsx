import { useRef, useState } from 'react'
import { BotIcon, ChevronDownIcon, ChevronLeftIcon, Loader2Icon, PlayIcon, RefreshCwIcon, XIcon } from 'lucide-react'
import { toast } from 'sonner'
import { Link, useLocation } from 'wouter'

import { ApiError, api } from '@/api/client'
import type { FleetResponse, Job, JobRun } from '@/api/types'
import { HostBadge, HostDot } from '@/components/HostBadge'
import { ScheduledList } from '@/components/ScheduledList'
import { ScreenHeader } from '@/components/ScreenHeader'
import { StatusDot } from '@/components/StatusDot'
import { Button } from '@/components/ui/button'
import { Dialog, DialogContent, DialogDescription, DialogFooter, DialogHeader, DialogTitle } from '@/components/ui/dialog'
import { Skeleton } from '@/components/ui/skeleton'
import { Switch } from '@/components/ui/switch'
import { useFleet } from '@/hooks/useFleet'
import { useJobShow, useJobs } from '@/hooks/useJobs'
import { useNow } from '@/hooks/useNow'
import { useSchedule } from '@/hooks/useSchedule'
import { useSwipeBack } from '@/hooks/useSwipeBack'
import { relTime } from '@/lib/format'
import { OUTCOME_TEXT, jobKey, keepLabel, liveSession, liveSummary, outcomeTone, promptLabel, runTree, whenLabel } from '@/lib/jobs'
import { sessionHref, statusLabel, statusMeta } from '@/lib/sessions'
import { STATUS_TEXT } from '@/lib/styles'
import { sessionTitle } from '@/lib/title'
import { cn } from '@/lib/utils'

type HostedJob = Job & { host: string }

/**
 * `#/bots`: the bots of every host (`fleet bots`), each with its schedule, next run, an
 * enabled switch, Run now and its last run; expanding one lists its runs and the sessions each run
 * started, as a tree. `screen` = the mobile page (back, swipe back); `pane` = the desktop main pane.
 */
export function JobsScreen({ layout = 'screen' }: { layout?: 'screen' | 'pane' }) {
  const pane = layout === 'pane'
  const [, navigate] = useLocation()
  const { jobs, errors, loading, reload, patch } = useJobs()
  const { fleet, refresh: refreshFleet } = useFleet()
  const now = useNow(30_000)
  const { pending: scheduled, reload: reloadSchedule } = useSchedule()
  const [open, setOpen] = useState<Set<string>>(() => new Set())
  const [confirm, setConfirm] = useState<HostedJob | null>(null)
  // A run that started: bumps the expanded job's runs.
  const [runEpoch, setRunEpoch] = useState(0)

  const back = () => {
    if (!pane && window.history.length > 1) window.history.back()
    else navigate('/', { replace: !pane })
  }
  const screenRef = useRef<HTMLDivElement>(null)
  useSwipeBack(screenRef, back, !pane)

  const toggle = (key: string) =>
    setOpen((prev) => {
      const next = new Set(prev)
      if (next.has(key)) next.delete(key)
      else next.add(key)
      return next
    })

  const setEnabled = async (j: HostedJob, enabled: boolean) => {
    patch(j.host, j.name, { enabled })
    try {
      const r = await api.setJobEnabled(j.host, j.name, enabled)
      if (r.job) patch(j.host, j.name, { enabled: r.job.enabled, nextRun: r.job.nextRun })
    } catch (err) {
      patch(j.host, j.name, { enabled: !enabled })
      toast.error(`Could not ${enabled ? 'enable' : 'pause'} ${j.name}`, { description: (err as Error)?.message })
    }
  }

  const refreshButton = (
    <Button
      variant="ghost"
      size="icon"
      aria-label="Refresh bots"
      title="Refresh"
      onClick={() => {
        reload()
        reloadSchedule()
      }}
      className={cn('shrink-0', pane ? 'size-11 rounded-xl' : '-my-1.5 size-11 rounded-xl')}
    >
      <RefreshCwIcon className="size-5" />
    </Button>
  )

  const content = (
    <div className="mx-auto w-full max-w-lg space-y-3 px-4 pt-3 pb-[max(1.5rem,env(safe-area-inset-bottom))] md:max-w-2xl">
      {loading && !jobs.length ? (
        <div className="space-y-2 rounded-xl border p-4">
          <Skeleton className="h-4 w-1/2" />
          <Skeleton className="h-3 w-full" />
        </div>
      ) : null}
      {scheduled.length ? (
        <div className="space-y-1.5">
          <h2 className="px-1 text-xs font-medium tracking-wide text-muted-foreground uppercase">Scheduled sessions</h2>
          <ScheduledList entries={scheduled} now={now} onChanged={reloadSchedule} />
        </div>
      ) : null}
      {jobs.map((j) => (
        <JobCard
          key={jobKey(j)}
          job={j}
          now={now}
          fleet={fleet}
          open={open.has(jobKey(j))}
          runEpoch={runEpoch}
          onToggle={() => toggle(jobKey(j))}
          onEnabled={(v) => void setEnabled(j, v)}
          onRun={() => setConfirm(j)}
        />
      ))}
      {errors.length ? (
        <ul className="space-y-1.5">
          {errors.map((e) => (
            <li key={e.host} className="flex items-start gap-2 text-sm text-muted-foreground">
              <HostDot host={e.host} className="mt-1.5" />
              <span className="min-w-0 break-words">
                <span className="font-medium text-foreground">{e.host}</span> — {e.error}
              </span>
            </li>
          ))}
        </ul>
      ) : null}
      {!loading && !jobs.length && !errors.length ? (
        <p className="py-8 text-center text-sm text-muted-foreground">No bots. Add one under <span className="font-mono">bots</span> in a host’s fleet config.</p>
      ) : null}
      <p className="text-xs text-dimmer">
        Bots live in each host’s fleet config; that host’s <span className="font-mono">fleet bots tick</span> service starts them on schedule, and the
        one-off sessions scheduled from New session or a task (<span className="font-mono">fleet schedule</span>).
      </p>
    </div>
  )

  const dialog = confirm ? (
    <RunDialog
      job={confirm}
      onClose={() => setConfirm(null)}
      onStarted={() => {
        setConfirm(null)
        setOpen((prev) => new Set(prev).add(jobKey(confirm)))
        setRunEpoch((n) => n + 1)
        reload()
        refreshFleet()
      }}
    />
  ) : null

  if (pane) {
    return (
      <section aria-label="Bots" className="flex h-full min-w-0 flex-1 flex-col bg-background">
        <header
          data-tauri-drag-region="deep"
          className="titlebar titlebar-lead flex shrink-0 items-center gap-1 border-b py-1.5 pr-2 pl-4 [--titlebar-pad:1rem]"
        >
          <h1 className="flex min-w-0 flex-1 items-center gap-1.5 text-[0.9375rem] leading-tight font-bold">
            <BotIcon className="size-4 text-muted-foreground" /> Bots
          </h1>
          {refreshButton}
          <Button variant="ghost" size="icon" aria-label="Close bots" title="Close (Esc)" onClick={back} className="size-11 shrink-0 rounded-xl">
            <XIcon className="size-5" />
          </Button>
        </header>
        <div className="min-h-0 flex-1 overflow-y-auto">{content}</div>
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
          <h1 className="flex-1 text-xl font-bold tracking-tight">Bots</h1>
          {refreshButton}
        </div>
      </ScreenHeader>
      {content}
      {dialog}
    </div>
  )
}

function JobCard({
  job: j,
  now,
  fleet,
  open,
  runEpoch,
  onToggle,
  onEnabled,
  onRun,
}: {
  job: HostedJob
  now: number
  fleet: FleetResponse | null
  open: boolean
  runEpoch: number
  onToggle: () => void
  onEnabled: (v: boolean) => void
  onRun: () => void
}) {
  const lastRun = j.runs?.[0] ?? null
  const live = liveSummary(j)
  const waiting = j.live?.some((l) => l.status === 'waiting')
  const prompt = promptLabel(j.prompt)
  return (
    <section aria-label={`Job ${j.name} on ${j.host}`} className="rounded-xl border bg-card">
      <div className="flex items-start gap-3 p-3.5">
        <button
          type="button"
          onClick={onToggle}
          aria-expanded={open}
          className="min-w-0 flex-1 rounded-md text-left outline-none focus-visible:ring-2 focus-visible:ring-ring/50"
        >
          <div className="flex min-w-0 items-center gap-2">
            <ChevronDownIcon className={cn('size-4 shrink-0 text-dimmer transition-transform', !open && '-rotate-90')} />
            <h2 className="min-w-0 truncate font-semibold">{j.name}</h2>
            <HostBadge host={j.host} />
          </div>
          <div className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-0.5 pl-6 text-xs text-muted-foreground">
            <span title={j.schedule}>
              {j.scheduleText ?? <span className="font-mono">{j.schedule}</span>}
            </span>
            <span>·</span>
            <span>{j.enabled ? (j.nextRun ? `next ${whenLabel(j.nextRun, now)}` : 'no next run') : 'paused'}</span>
          </div>
          <div className="mt-0.5 flex flex-wrap items-center gap-x-2 gap-y-0.5 pl-6 text-xs">
            {lastRun ? (
              <span className="text-muted-foreground">
                last <span className={OUTCOME_TEXT[outcomeTone(lastRun.outcome)]}>{lastRun.outcome}</span> {relTime(lastRun.startedAt, now)} ago
              </span>
            ) : (
              <span className="text-dimmer">never ran</span>
            )}
            {live ? <span className={cn(waiting ? 'font-medium text-status-waiting' : 'text-status-busy')}>{live}</span> : null}
          </div>
        </button>
        <div className="flex shrink-0 flex-col items-end gap-2.5">
          <Switch checked={j.enabled} onCheckedChange={onEnabled} aria-label={`${j.name} on ${j.host} enabled`} />
          <Button variant="outline" size="sm" onClick={onRun} title="Start a run now">
            <PlayIcon /> Run now
          </Button>
        </div>
      </div>
      {open ? (
        <div className="space-y-3 border-t px-3.5 py-3">
          <dl className="grid grid-cols-[auto_minmax(0,1fr)] gap-x-3 gap-y-1 text-xs">
            <dt className="text-dimmer">dir</dt>
            <dd className="truncate font-mono">{j.dir}</dd>
            {prompt ? (
              <>
                <dt className="text-dimmer">prompt</dt>
                <dd className="line-clamp-2 break-words">
                  {j.prompt && 'file' in j.prompt && j.prompt.editorUrl ? (
                    <a
                      href={j.prompt.rootEditorUrl ?? j.prompt.editorUrl}
                      onClick={() => openPromptFile(j.prompt)}
                      title={`Open ${j.prompt.root ?? j.prompt.abs ?? j.prompt.file} in the editor`}
                      className="underline decoration-dimmer underline-offset-2 hover:text-foreground"
                    >
                      {prompt}
                    </a>
                  ) : (
                    prompt
                  )}
                </dd>
              </>
            ) : null}
            {j.model ? (
              <>
                <dt className="text-dimmer">model</dt>
                <dd className="truncate font-mono">{j.model}</dd>
              </>
            ) : null}
            <dt className="text-dimmer">sessions</dt>
            <dd>{keepLabel(j.keep)} (when idle)</dd>
          </dl>
          <JobRuns host={j.host} name={j.name} now={now} fleet={fleet} runEpoch={runEpoch} />
        </div>
      ) : null}
    </section>
  )
}

/**
 * After the link opens the prompt's folder (its git checkout), open the file itself: the editor
 * puts it in the window whose folder holds it. A second URL, because one URL opens one or the other.
 */
function openPromptFile(prompt: Job['prompt']) {
  if (!prompt || !('file' in prompt) || !prompt.rootEditorUrl || !prompt.editorUrl) return
  const url = prompt.editorUrl
  window.setTimeout(() => window.location.assign(url), 1500)
}

function JobRuns({ host, name, now, fleet, runEpoch }: { host: string; name: string; now: number; fleet: FleetResponse | null; runEpoch: number }) {
  // Re-key on a new run so the poller starts over at once.
  return <JobRunsInner key={runEpoch} host={host} name={name} now={now} fleet={fleet} />
}

function JobRunsInner({ host, name, now, fleet }: { host: string; name: string; now: number; fleet: FleetResponse | null }) {
  const { job, error } = useJobShow(host, name, true)
  if (error && !job) return <p className="text-xs text-destructive">{error}</p>
  if (!job) {
    return (
      <p className="flex items-center gap-2 text-xs text-dimmer">
        <Loader2Icon className="size-3.5 animate-spin" /> Loading runs…
      </p>
    )
  }
  if (!job.runs?.length) return <p className="text-xs text-dimmer">No runs yet.</p>
  return (
    <ol className="space-y-2.5" aria-label={`Runs of ${name}`}>
      {job.runs.map((r) => (
        <RunItem key={r.id} run={r} host={host} now={now} fleet={fleet} />
      ))}
    </ol>
  )
}

function RunItem({ run: r, host, now, fleet }: { run: JobRun; host: string; now: number; fleet: FleetResponse | null }) {
  const rows = runTree(r)
  const tone = outcomeTone(r.outcome)
  return (
    <li className="rounded-lg border bg-background/50 px-3 py-2">
      <div className="flex flex-wrap items-baseline gap-x-2 gap-y-0.5 text-xs">
        <span className={cn('font-semibold', OUTCOME_TEXT[tone])}>{r.outcome}</span>
        <span className="text-muted-foreground">{whenLabel(r.startedAt, now)}</span>
        <span className="text-dimmer">{r.trigger}</span>
        <span className="ml-auto font-mono text-[0.6875rem] text-dimmer">{r.id}</span>
      </div>
      {r.reason ? <p className="mt-0.5 text-xs break-words text-muted-foreground">{r.reason}</p> : null}
      {rows.length ? (
        <ul className="mt-1.5 space-y-0.5">
          {rows.map(({ session: rs, depth }) => {
            const s = liveSession(fleet, host, rs.tmuxSession)
            const indent = { paddingLeft: `${depth * 1.1}rem` }
            if (s) {
              const meta = statusMeta(s.status)
              return (
                <li key={rs.tmuxSession} style={indent}>
                  <Link
                    href={sessionHref(s)}
                    className="flex min-h-9 min-w-0 items-center gap-2 rounded-md px-1.5 text-sm outline-none hover:bg-muted/60 focus-visible:ring-2 focus-visible:ring-ring/50 active:bg-muted"
                  >
                    {depth ? <span className="shrink-0 text-dimmer">└</span> : null}
                    <StatusDot status={s.status} className="size-2" />
                    <span className="min-w-0 flex-1 truncate">{sessionTitle(s)}</span>
                    <span className={cn('shrink-0 text-xs', STATUS_TEXT[meta.key])}>{statusLabel(s)}</span>
                  </Link>
                </li>
              )
            }
            return (
              <li key={rs.tmuxSession} style={indent} className="flex min-h-8 min-w-0 items-center gap-2 px-1.5 text-sm text-dimmer">
                {depth ? <span className="shrink-0">└</span> : null}
                <span className="inline-block size-2 shrink-0 rounded-full border border-dimmer" aria-hidden />
                <span className="min-w-0 flex-1 truncate font-mono text-xs">{rs.name || rs.tmuxSession}</span>
                <span className="shrink-0 text-xs">{rs.closedAt ? `closed ${relTime(rs.closedAt, now)} ago` : 'gone'}</span>
              </li>
            )
          })}
        </ul>
      ) : null}
    </li>
  )
}

/** "Run now" confirmation; a 409 (a session of the job still works) offers "Run anyway" (`--force`). */
function RunDialog({ job: j, onClose, onStarted }: { job: HostedJob; onClose: () => void; onStarted: () => void }) {
  const [busy, setBusy] = useState(false)
  const [refused, setRefused] = useState<string | null>(null)
  const [error, setError] = useState<string | null>(null)
  const run = async (force: boolean) => {
    setBusy(true)
    setError(null)
    try {
      const r = await api.runJob(j.host, j.name, force)
      toast.success(`Started ${j.name} on ${j.host}`, { description: r.output.replace(/^→\s*/, '') || undefined })
      onStarted()
    } catch (err) {
      if (err instanceof ApiError && err.status === 409) setRefused(err.message)
      else setError((err as Error)?.message || 'could not start the run')
    } finally {
      setBusy(false)
    }
  }
  return (
    <Dialog open onOpenChange={(o) => (o || busy ? null : onClose())}>
      <DialogContent>
        <DialogHeader>
          <DialogTitle>Run {j.name} now?</DialogTitle>
          <DialogDescription>
            Starts a new agent session on {j.host} in <span className="font-mono">{j.dir}</span>, whatever the schedule says.
          </DialogDescription>
        </DialogHeader>
        {refused ? <p className="text-sm text-status-waiting">{refused}</p> : null}
        {error ? <p className="text-sm text-destructive">{error}</p> : null}
        <DialogFooter>
          <Button variant="outline" onClick={onClose} disabled={busy}>
            Cancel
          </Button>
          <Button onClick={() => void run(Boolean(refused))} disabled={busy}>
            {busy ? <Loader2Icon className="animate-spin" /> : <PlayIcon />}
            {refused ? 'Run anyway' : 'Run now'}
          </Button>
        </DialogFooter>
      </DialogContent>
    </Dialog>
  )
}
