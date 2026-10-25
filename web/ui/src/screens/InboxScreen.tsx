import { useEffect, useRef, useState } from 'react'
import {
  BotIcon,
  CheckIcon,
  ChevronLeftIcon,
  GitPullRequestIcon,
  InboxIcon,
  MailIcon,
  MessageSquareIcon,
  RefreshCwIcon,
  SparklesIcon,
  SquareCheckBigIcon,
  TriangleAlertIcon,
} from 'lucide-react'
import { useLocation } from 'wouter'

import type { InboxItem, PullRequest } from '@/api/types'
import { Markdown } from '@/components/Markdown'
import { ChecksIcon } from '@/components/PrChip'
import { ScreenHeader } from '@/components/ScreenHeader'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { useInbox } from '@/hooks/useInbox'
import { useNow } from '@/hooks/useNow'
import { useSwipeBack } from '@/hooks/useSwipeBack'
import { relTime } from '@/lib/format'
import { briefMarkdown, dueChip, inboxSections, isNewItem, itemTarget, sourceLines } from '@/lib/inbox'
import { storage } from '@/lib/storage'
import { cn } from '@/lib/utils'

const LAST_VISIT_KEY = 'fleet.inboxLastVisit'

const SOURCE_ICON: Record<InboxItem['source'], typeof InboxIcon> = {
  github: GitPullRequestIcon,
  productive: SquareCheckBigIcon,
  session: BotIcon,
  slack: MessageSquareIcon,
  email: MailIcon,
}

/**
 * `#/inbox`: everything waiting on you — PRs, tasks, sessions that need an answer, Slack and email
 * — with a short brief on top. The server keeps the list (`inbox.host`): items come and go with
 * their sources. Done marks one handled at once (it would otherwise wait for its source). `pane` = the desktop layout, `screen` = mobile.
 */
export function InboxScreen({ layout = 'screen' }: { layout?: 'screen' | 'pane' }) {
  const pane = layout === 'pane'
  const [, navigate] = useLocation()
  const { data, error, loading, refresh, refreshing, dismiss } = useInbox()
  const now = useNow(30_000)
  // "New" = first seen since the previous visit: read it once, then this visit is the last one.
  const [lastVisit] = useState(() => Number(storage.get(LAST_VISIT_KEY)) || 0)
  useEffect(() => {
    const mark = () => storage.set(LAST_VISIT_KEY, String(Date.now()))
    mark()
    return mark
  }, [])

  const back = () => {
    if (!pane && window.history.length > 1) window.history.back()
    else navigate('/', { replace: !pane })
  }
  const screenRef = useRef<HTMLDivElement>(null)
  useSwipeBack(screenRef, back, !pane)

  const scanning = Boolean(data?.sources.comms?.running)
  const refreshButton = (
    <Button
      variant="outline"
      size="sm"
      title="Scan Slack & email now and re-read GitHub, Productive and the sessions"
      onClick={() => void refresh()}
      disabled={refreshing || scanning || data?.enabled === false}
      className={cn('shrink-0', !pane && '-my-1')}
    >
      <RefreshCwIcon className={cn((refreshing || scanning) && 'animate-spin')} />
      {scanning ? 'Scanning…' : 'Scan now'}
    </Button>
  )

  const lines = sourceLines(data?.sources, now)
  const status = (
    <div className="space-y-0.5 text-xs text-dimmer">
      {error ? (
        <p className="inline-flex items-start gap-1 text-status-waiting">
          <TriangleAlertIcon className="mt-px size-3.5 shrink-0" />
          {error}
        </p>
      ) : null}
      {data?.error && data.enabled ? <p className="text-status-waiting">{data.error}</p> : null}
      {lines.length ? (
        <p className="flex flex-wrap gap-x-3 gap-y-0.5">
          {lines.map((l) => (
            <span key={l.key} className={cn(l.warn && 'text-status-waiting')} title={l.warn ? l.text : undefined}>
              <span className="text-muted-foreground">{l.label}</span> <span className={cn(l.warn && 'line-clamp-1 inline')}>{l.text}</span>
            </span>
          ))}
          {data?.host ? <span>on {data.host}</span> : null}
        </p>
      ) : null}
    </div>
  )

  const brief = data?.brief ? (
    <section aria-label="Brief" className="rounded-lg border bg-card p-3 text-sm">
      <h2 className="mb-1 flex items-center gap-1.5 text-xs font-medium text-muted-foreground">
        <SparklesIcon className="size-3.5" /> Brief
        <span className="ml-auto font-normal text-dimmer tabular-nums" title={new Date(data.brief.at).toLocaleString()}>
          {relTime(data.brief.at, now)} ago
        </span>
      </h2>
      <Markdown text={briefMarkdown(data.brief.text, window.location.origin)} />
    </section>
  ) : null

  const skeleton = (
    <div className="space-y-2">
      {[0, 1, 2, 3].map((i) => (
        <div key={i} className="space-y-2 rounded-lg border p-3">
          <Skeleton className="h-3 w-1/3" />
          <Skeleton className="h-4 w-full" />
        </div>
      ))}
    </div>
  )

  const sections = inboxSections(data?.items ?? [])
  const body =
    data?.enabled === false ? (
      <div className="space-y-1 rounded-lg border border-dashed p-4 text-sm">
        <p className="font-medium">The inbox is not set up</p>
        <p className="text-muted-foreground">{data.error ?? 'No server runs the inbox.'}</p>
        <p className="text-xs text-dimmer">
          Pick the always-on host that builds it: <code className="font-mono">fleet config set inbox.host &lt;host&gt;</code>
        </p>
      </div>
    ) : loading && !data && !error ? (
      skeleton
    ) : (
      <>
        {brief}
        {sections.map((s) => (
          <section key={s.id} aria-label={s.title}>
            <h2 className="mb-2 flex items-baseline gap-2 text-sm font-semibold">
              {s.title}
              <span className="text-xs font-normal text-dimmer tabular-nums">{s.items.length}</span>
            </h2>
            <div className="space-y-2">
              {s.items.map((it) => (
                <InboxRow key={it.id} item={it} now={now} fresh={isNewItem(it, lastVisit, now)} onDone={() => void dismiss(it.id)} />
              ))}
            </div>
          </section>
        ))}
        {data && !sections.length ? <p className="py-8 text-center text-sm text-muted-foreground">Nothing waits on you.</p> : null}
      </>
    )

  if (pane) {
    return (
      <section aria-label="Inbox" className="flex h-full min-w-0 flex-1 flex-col bg-background">
        <header
          data-tauri-drag-region="deep"
          className="titlebar titlebar-lead flex shrink-0 items-center gap-2 border-b py-1.5 pr-2 pl-4 [--titlebar-pad:1rem]"
        >
          <h1 className="flex shrink-0 items-center gap-1.5 text-[0.9375rem] font-bold">
            <InboxIcon className="size-4 text-muted-foreground" /> Inbox
          </h1>
          <div className="flex-1" />
          {refreshButton}
        </header>
        <div className="min-h-0 flex-1 overflow-y-auto">
          <div className="mx-auto w-full max-w-3xl space-y-5 px-4 pt-3 pb-6">
            {status}
            {body}
          </div>
        </div>
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
          <h1 className="flex-1 text-xl font-bold tracking-tight">Inbox</h1>
          {refreshButton}
        </div>
      </ScreenHeader>
      <div className="mx-auto w-full max-w-lg space-y-5 px-4 pt-3 pb-[max(1.5rem,env(safe-area-inset-bottom))] md:max-w-2xl">
        {status}
        {body}
      </div>
    </div>
  )
}

function InboxRow({ item: it, now, fresh, onDone }: { item: InboxItem; now: number; fresh: boolean; onDone: () => void }) {
  const Icon = SOURCE_ICON[it.source] ?? InboxIcon
  const target = itemTarget(it)
  const m = it.meta
  const due = dueChip(m, now)
  const chips = (
    <>
      {m.repo ? (
        <span className="font-mono">
          {m.repo}
          {m.number != null ? `#${m.number}` : ''}
        </span>
      ) : null}
      {m.size ? <span className="rounded border px-1 leading-4">{m.size}</span> : null}
      {m.additions != null || m.deletions != null ? (
        <span className="font-mono tabular-nums">
          +{m.additions ?? 0} −{m.deletions ?? 0}
        </span>
      ) : null}
      {m.checks ? <ChecksIcon checks={m.checks as PullRequest['checks']} className="size-3" /> : null}
      {m.draft ? <span>draft</span> : null}
      {m.project ? <span className="min-w-0 truncate">{m.project}</span> : null}
      {m.status ? <span className="rounded border px-1 leading-4">{m.status}</span> : null}
      {due ? <span className={due.overdue ? 'text-status-error' : 'text-status-waiting'}>{due.label}</span> : null}
      {m.host && it.source === 'session' ? <span>{m.host}</span> : null}
      {it.effort && it.effort !== 'quick' && it.section !== 'quick' ? <span className="text-dimmer">{it.effort}</span> : null}
    </>
  )
  const inner = (
    <>
      <div className="flex items-start gap-2.5">
        <Icon className="mt-0.5 size-4 shrink-0 text-muted-foreground" />
        <div className="min-w-0 flex-1">
          <div className="flex items-baseline gap-2">
            <span className="line-clamp-2 min-w-0 flex-1 font-medium text-foreground">{it.title}</span>
            {fresh ? <span className="size-2 shrink-0 self-center rounded-full bg-primary" title="New since your last visit" /> : null}
            <span className="shrink-0 text-xs text-dimmer tabular-nums" title={new Date(it.at).toLocaleString()}>
              {relTime(it.at, now)}
            </span>
          </div>
          {it.ask ? <p className="mt-0.5 line-clamp-2 text-xs text-muted-foreground">{it.ask}</p> : null}
          <div className="mt-1 flex flex-wrap items-center gap-x-2 gap-y-1 text-xs text-muted-foreground">
            {it.from ? <span className="text-foreground/80">{it.from}</span> : null}
            {chips}
          </div>
        </div>
      </div>
    </>
  )
  const cls = 'block min-w-0 flex-1 p-3 pr-1 text-left text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring/50 rounded-l-lg'
  const link = !target ? (
    <div className={cls}>{inner}</div>
  ) : target.external ? (
    <a href={target.href} target="_blank" rel="noreferrer" className={cls}>
      {inner}
    </a>
  ) : (
    <a href={target.href} className={cls}>
      {inner}
    </a>
  )
  return (
    <div className="group flex items-start rounded-lg border bg-card hover:border-primary/40 hover:bg-muted/40">
      {link}
      <Button
        variant="ghost"
        size="icon"
        aria-label={`Done: ${it.title}`}
        title="Done — take it off the list"
        onClick={onDone}
        className="m-1.5 size-8 shrink-0 rounded-lg text-dimmer hover:text-primary"
      >
        <CheckIcon className="size-4" />
      </Button>
    </div>
  )
}
