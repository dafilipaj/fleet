import { useRef } from 'react'
import { ChevronLeftIcon, RefreshCwIcon, TriangleAlertIcon, XIcon } from 'lucide-react'
import { useLocation } from 'wouter'

import type { UsageLimit } from '@/api/types'
import { HostDot } from '@/components/HostBadge'
import { ScreenHeader } from '@/components/ScreenHeader'
import { Button } from '@/components/ui/button'
import { Skeleton } from '@/components/ui/skeleton'
import { useNow } from '@/hooks/useNow'
import { useSwipeBack } from '@/hooks/useSwipeBack'
import { useUsage } from '@/hooks/useUsage'
import { relTime } from '@/lib/format'
import { CTX_FILL } from '@/lib/styles'
import { PROVIDER_NAME, money, resetsLabel, usageLevel, type AccountUsage } from '@/lib/usage'
import { cn } from '@/lib/utils'

const LEVEL_TEXT = { low: 'text-foreground', warn: 'text-status-waiting', hot: 'text-status-error' } as const

/**
 * `#/usage`: the subscription limits of every account logged in across the fleet — Claude
 * (current session, weekly, per-model weekly, extra usage) and the ChatGPT plan Codex and pi
 * use (session, weekly, credits). Hosts on one account share one card. `screen` = the mobile page (back, swipe back); `pane` = the desktop main pane.
 */
export function UsageScreen({ layout = 'screen' }: { layout?: 'screen' | 'pane' }) {
  const pane = layout === 'pane'
  const [, navigate] = useLocation()
  const { accounts, errors, loading, refresh, refreshing } = useUsage()
  const now = useNow(30_000)

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

  const content = (
    <div className="mx-auto w-full max-w-lg space-y-4 px-4 pt-3 pb-[max(1.5rem,env(safe-area-inset-bottom))] md:max-w-2xl">
      {loading && !accounts.length ? (
        <div className="space-y-3 rounded-xl border p-4">
          <Skeleton className="h-4 w-1/2" />
          <Skeleton className="h-2 w-full" />
          <Skeleton className="h-2 w-full" />
          <Skeleton className="h-2 w-full" />
        </div>
      ) : null}
      {accounts.map((a) => (
        <AccountCard key={a.key} account={a} now={now} />
      ))}
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
      <p className="text-xs text-dimmer">The same numbers as Claude Code’s /usage and Codex’s /status, read from each host’s Claude and Codex logins. Refreshed every minute.</p>
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
  return (
    <section className="rounded-xl border bg-card p-4">
      <header className="mb-3 flex flex-wrap items-baseline gap-x-2 gap-y-1">
        <span className="rounded-full bg-muted px-1.5 py-px text-[0.6875rem] font-medium text-muted-foreground">{name}</span>
        <h2 className="min-w-0 truncate font-semibold">{who}</h2>
        {a.account?.plan_label ? <span className="text-sm text-muted-foreground">{a.account.plan_label}</span> : null}
        <span className="ml-auto flex items-center gap-1.5 text-xs text-dimmer">
          {a.hosts.map((h) => (
            <span key={h} className="inline-flex items-center gap-1">
              <HostDot host={h} />
              {h}
            </span>
          ))}
        </span>
      </header>
      <div className="space-y-3.5">
        {u.limits.map((l) => (
          <LimitRow key={`${l.kind}-${l.label}`} limit={l} now={now} />
        ))}
        {!u.limits.length ? <p className="text-sm text-muted-foreground">No limits reported for this account.</p> : null}
      </div>
      {x ? (
        <div className="mt-3.5 flex items-baseline justify-between gap-2 border-t pt-3 text-sm">
          <span>{openai ? 'Credits' : 'Extra usage'}</span>
          <span className="text-muted-foreground tabular-nums">
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
      <footer className="mt-3 text-xs text-dimmer">
        {u.stale ? (
          <span className="inline-flex items-start gap-1 text-status-waiting">
            <TriangleAlertIcon className="mt-px size-3.5 shrink-0" />
            As of {relTime(fetched, now) || 'earlier'} ago — {u.error ?? 'refresh failed'}
          </span>
        ) : (
          <>Updated {relTime(fetched, now) || 'just now'}{relTime(fetched, now) ? ' ago' : ''}</>
        )}
      </footer>
    </section>
  )
}

function LimitRow({ limit: l, now }: { limit: UsageLimit; now: number }) {
  const level = usageLevel(l)
  const pct = Math.max(0, Math.round(l.percent))
  const resets = resetsLabel(l.resets_at, now)
  return (
    <div>
      <div className="mb-1.5 flex items-baseline gap-2 text-sm">
        <span className="min-w-0 truncate">{l.label}</span>
        {l.active ? (
          <span className="rounded-full bg-muted px-1.5 py-px text-[0.6875rem] text-muted-foreground" title="The limit that stops you first">
            binding
          </span>
        ) : null}
        <span className={cn('ml-auto font-medium tabular-nums', LEVEL_TEXT[level])}>{pct}%</span>
      </div>
      <div
        className="relative h-2 overflow-hidden rounded-full bg-muted"
        role="meter"
        aria-label={l.label}
        aria-valuemin={0}
        aria-valuemax={100}
        aria-valuenow={pct}
      >
        <span className={cn('absolute inset-y-0 left-0 rounded-full', level === 'low' ? 'bg-primary' : CTX_FILL[level])} style={{ width: `${Math.min(100, pct)}%` }} />
      </div>
      {resets ? <div className="mt-1 text-xs text-dimmer">Resets {resets}</div> : null}
    </div>
  )
}
