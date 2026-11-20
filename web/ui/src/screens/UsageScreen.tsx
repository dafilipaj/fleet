import { useRef } from 'react'
import { ChevronLeftIcon, RefreshCwIcon, TriangleAlertIcon, XIcon } from 'lucide-react'
import { useLocation } from 'wouter'

import type { UsageLimit } from '@/api/types'
import { HostDot } from '@/components/HostBadge'
import { ScreenHeader } from '@/components/ScreenHeader'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { useFleet } from '@/hooks/useFleet'
import { useNow } from '@/hooks/useNow'
import { useSwipeBack } from '@/hooks/useSwipeBack'
import { useUsage } from '@/hooks/useUsage'
import { relTime } from '@/lib/format'
import { isShared } from '@/lib/hosts'
import { CTX_FILL } from '@/lib/styles'
import { PROVIDER_NAME, money, resetsLabel, usageLevel, type AccountUsage } from '@/lib/usage'
import { cn } from '@/lib/utils'

const LEVEL_TEXT = { low: 'text-foreground', warn: 'text-status-waiting', hot: 'text-status-error' } as const

/**
 * `#/usage`: the subscription limits of every account logged in across the fleet — Claude
 * (current session, weekly, per-model weekly, extra usage) and the ChatGPT plan Codex and pi
 * use (session, weekly, credits). Hosts on one account share one card. Accounts only shared sheds
 * use (`hosts.<name>.shared`) come in their own group: their limits are not the user's. Each group
 * is a grid of compact cards that fills the width, so a wide pane shows every account at once.
 * `screen` = the mobile page (back, swipe back); `pane` = the desktop main pane.
 */
export function UsageScreen({ layout = 'screen' }: { layout?: 'screen' | 'pane' }) {
  const pane = layout === 'pane'
  const [, navigate] = useLocation()
  const { accounts, errors, loading, refresh, refreshing } = useUsage()
  const now = useNow(30_000)
  const { fleet } = useFleet()
  const shared = new Set((fleet?.hosts ?? []).filter(isShared).map((h) => h.name))
  const onlyShared = (a: AccountUsage) => a.hosts.length > 0 && a.hosts.every((h) => shared.has(h))
  const mine = accounts.filter((a) => !onlyShared(a))
  const sheds = accounts.filter(onlyShared)

  const back = () => {
    if (!pane && window.history.length > 1) window.history.back()
    else navigate('/', { replace: !pane })
  }
  const screenRef = useRef<HTMLDivElement>(null)
  useSwipeBack(screenRef, back, !pane)

  const refreshButton = (
    <Button
      variant="ghost"
      size="icon"
      aria-label="Refresh usage"
      title="Refresh"
      onClick={() => void refresh()}
      disabled={refreshing}
      className={cn('shrink-0', pane ? 'size-11 rounded-xl' : '-my-1.5 size-11 rounded-xl')}
    >
      <RefreshCwIcon className={cn('size-5', refreshing && 'animate-spin')} />
    </Button>
  )

  const grid = 'grid grid-cols-[repeat(auto-fill,minmax(min(100%,19rem),1fr))] items-start gap-3'
  const group = (label: string, list: AccountUsage[]) => (
    <section aria-label={label} className="space-y-2">
      {mine.length && sheds.length ? <h2 className="px-1 text-xs font-medium tracking-wide text-muted-foreground uppercase">{label}</h2> : null}
      <div className={grid}>
        {list.map((a) => (
          <AccountCard key={a.key} account={a} now={now} />
        ))}
      </div>
    </section>
  )

  const content = (
    <div className={cn('mx-auto w-full space-y-4 px-4 pt-3 pb-[max(1.5rem,env(safe-area-inset-bottom))]', pane ? 'max-w-7xl' : 'max-w-lg md:max-w-4xl')}>
      {loading && !accounts.length ? (
        <div className={grid}>
          <div className="space-y-3 rounded-xl border p-3">
            <Skeleton className="h-4 w-1/2" />
            <Skeleton className="h-1.5 w-full" />
            <Skeleton className="h-1.5 w-full" />
            <Skeleton className="h-1.5 w-full" />
          </div>
        </div>
      ) : null}
      {mine.length ? group('My hosts', mine) : null}
      {sheds.length ? group('Shared sheds', sheds) : null}
      {errors.length ? (
        <ul className="space-y-1.5">
          {errors.map((e) => (
            <li key={e.key} className="flex items-start gap-2 text-sm text-muted-foreground">
              <HostDot host={e.host} className="mt-1.5" />
              <span className="min-w-0">
                <span className="font-medium text-foreground">{e.host}</span> — {e.error}
              </span>
            </li>
          ))}
        </ul>
      ) : null}
      {!loading && !accounts.length && !errors.length ? <p className="py-8 text-center text-sm text-muted-foreground">No hosts to read usage from.</p> : null}
      <p className="px-1 text-xs text-dimmer">The same numbers as Claude Code’s /usage and Codex’s /status, read from each host’s Claude and Codex logins. Refreshed every minute.</p>
    </div>
  )

  if (pane) {
    return (
      <section aria-label="Usage" className="flex h-full min-w-0 flex-1 flex-col bg-background">
        <header
          data-tauri-drag-region="deep"
          className="titlebar titlebar-lead flex shrink-0 items-center gap-1 border-b py-1.5 pr-2 pl-4 [--titlebar-pad:1rem]"
        >
          <h1 className="min-w-0 flex-1 text-[0.9375rem] leading-tight font-bold">Usage</h1>
          {refreshButton}
          <Button variant="ghost" size="icon" aria-label="Close usage" title="Close (Esc)" onClick={back} className="size-11 shrink-0 rounded-xl">
            <XIcon className="size-5" />
          </Button>
        </header>
        <div className="min-h-0 flex-1 overflow-y-auto">{content}</div>
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
          <h1 className="flex-1 text-xl font-bold tracking-tight">Usage</h1>
          {refreshButton}
        </div>
      </ScreenHeader>
      {content}
    </div>
  )
}

function AccountCard({ account: a, now }: { account: AccountUsage; now: number }) {
  const u = a.usage
  const name = PROVIDER_NAME[a.provider]
  const who = a.account?.email ?? a.account?.organization ?? `${name} account`
  const x = u.extra_usage
  const openai = a.provider === 'openai'
  const fetched = Date.parse(u.fetched_at)
  const ago = relTime(fetched, now)
  return (
    <section className="rounded-xl border bg-card p-3">
      <header className="mb-2.5 space-y-0.5">
        <div className="flex items-baseline gap-2">
          <span className="shrink-0 rounded-full bg-muted px-1.5 py-px text-[0.6875rem] font-medium text-muted-foreground">{name}</span>
          <h3 className="min-w-0 truncate text-sm font-semibold" title={who}>
            {who}
          </h3>
          {a.account?.plan_label ? <span className="ml-auto shrink-0 text-xs text-muted-foreground">{a.account.plan_label}</span> : null}
        </div>
        <div className="flex flex-wrap items-center gap-x-2 gap-y-0.5 text-xs text-dimmer">
          {a.hosts.map((h) => (
            <span key={h} className="inline-flex items-center gap-1">
              <HostDot host={h} />
              {h}
            </span>
          ))}
          {!u.stale ? <span className="ml-auto" title="Last read">{ago ? `${ago} ago` : 'just now'}</span> : null}
        </div>
      </header>
      <div className="space-y-2">
        {u.limits.map((l) => (
          <LimitRow key={`${l.kind}-${l.label}`} limit={l} now={now} />
        ))}
        {!u.limits.length ? <p className="text-sm text-muted-foreground">No limits reported for this account.</p> : null}
      </div>
      {x ? (
        <div className="mt-2.5 flex items-baseline justify-between gap-2 border-t pt-2 text-xs">
          <span className="text-muted-foreground">{openai ? 'Credits' : 'Extra usage'}</span>
          <span className="tabular-nums">
            {openai
              ? !x.enabled
                ? 'None'
                : x.balance != null
                  ? `${x.balance.toFixed(2)} left`
                  : 'On'
              : !x.enabled
              ? 'Off'
              : x.limit != null
                ? `${money(x.used ?? 0, x.currency)} / ${money(x.limit, x.currency)}`
                : x.used != null
                  ? `${money(x.used, x.currency)} spent`
                  : 'On'}
          </span>
        </div>
      ) : null}
      {u.stale ? (
        <footer className="mt-2 flex items-start gap-1 text-xs text-status-waiting">
          <TriangleAlertIcon className="mt-px size-3.5 shrink-0" />
          As of {ago || 'earlier'} ago — {u.error ?? 'refresh failed'}
        </footer>
      ) : null}
    </section>
  )
}

function LimitRow({ limit: l, now }: { limit: UsageLimit; now: number }) {
  const level = usageLevel(l)
  const pct = Math.max(0, Math.round(l.percent))
  const resets = resetsLabel(l.resets_at, now)
  return (
    <div>
      <div className="mb-1 flex items-baseline gap-1.5 text-sm">
        <span className="min-w-0 truncate">{l.label}</span>
        {l.active ? (
          <span className="shrink-0 rounded-full bg-muted px-1.5 text-[0.625rem] text-muted-foreground" title="The limit that stops you first">
            binding
          </span>
        ) : null}
        {resets ? (
          <span className="ml-auto shrink-0 text-xs text-dimmer" title="Resets">
            ↻ {resets.replace(/^in /, '')}
          </span>
        ) : null}
        <span className={cn('w-10 shrink-0 text-right font-medium tabular-nums', !resets && 'ml-auto', LEVEL_TEXT[level])}>{pct}%</span>
      </div>
      <div
        className="relative h-1.5 overflow-hidden rounded-full bg-muted"
        role="meter"
        aria-label={l.label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={pct}
      >
        <span className={cn('absolute inset-y-0 left-0 rounded-full', level === 'low' ? 'bg-primary' : CTX_FILL[level])} style={{ width: `${Math.min(100, pct)}%` }} />
      </div>
    </div>
  )
}
