import { useCallback, useEffect, useMemo, useState } from 'react'
import { HourglassIcon, Loader2Icon, UnlockIcon } from 'lucide-react'
import { toast } from 'sonner'

import { ApiError, api } from '@/api/client'
import type { UsageCheck, UsageFallbackSettings, UsageFallbackView } from '@/api/types'
import { HostBadge } from '@/components/HostBadge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { useFleet } from '@/hooks/useFleet'
import { harnessLabel } from '@/lib/harness'
import { cn } from '@/lib/utils'

type Loaded = { state: 'ok'; view: UsageFallbackView } | { state: 'error'; error: string }

const POLL_MS = 60 * 1000
const HARNESSES = ['claude', 'codex', 'pi'] as const
const PROVIDER: Record<string, string> = { claude: 'Claude', openai: 'ChatGPT' }

const why = (err: unknown) => (err instanceof ApiError && (err.status === 404 || err.status === 501) ? 'update fleet there' : (err as Error)?.message || 'unreachable')
const isCheck = (c: UsageCheck | { error: string } | undefined): c is UsageCheck => !!c && !('error' in c)
const hhmm = (ms: number) => new Date(ms).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })

/**
 * Settings → Usage: per host, whether each subscription is at its limit and where new sessions
 * go, the fallback settings (config `usage`, saved per host, live at once), and an override that
 * turns the check off for a while.
 */
export function UsageFallbackPanel() {
  const { fleet } = useFleet()
  const hostsKey = (fleet?.hosts ?? []).map((h) => h.name).join('\n')
  const hosts = useMemo(() => (hostsKey ? hostsKey.split('\n') : []), [hostsKey])
  const [views, setViews] = useState<Record<string, Loaded>>({})

  const load = useCallback(async () => {
    const got = await Promise.all(
      hosts.map(async (h): Promise<[string, Loaded]> => {
        try {
          return [h, { state: 'ok', view: await api.usageSettings(h) }]
        } catch (err) {
          return [h, { state: 'error', error: why(err) }]
        }
      }),
    )
    setViews(Object.fromEntries(got))
  }, [hosts])

  useEffect(() => {
    if (!hosts.length) return
    const first = setTimeout(() => void load(), 0)
    const t = setInterval(() => void load(), POLL_MS)
    return () => {
      clearTimeout(first)
      clearInterval(t)
    }
  }, [hosts, load])

  if (!hosts.length) {
    return (
      <div className="flex items-center gap-2 py-4 text-sm text-dimmer">
        <Loader2Icon className="size-4 animate-spin" /> Loading…
      </div>
    )
  }

  return (
    <div>
      <p className="py-2 text-xs text-dimmer">
        When a session's subscription is at its limit, it starts on the fallback agent (the other subscription). A session you start is marked in the new-session form
        and can keep its agent; Slack, GitHub, bots and schedules switch on their own, and wait when both are at the limit. Bots and schedules also switch when the
        week's use runs ahead of the week (pace). Each host keeps its own settings; changes apply at once.
      </p>
      {hosts.map((h) => (
        <HostCard key={h} host={h} loaded={views[h]} onView={(v) => setViews((m) => ({ ...m, [h]: { state: 'ok', view: v } }))} />
      ))}
    </div>
  )
}

function HostCard({ host, loaded, onView }: { host: string; loaded: Loaded | undefined; onView: (v: UsageFallbackView) => void }) {
  const [busy, setBusy] = useState<string | null>(null)
  const [editing, setEditing] = useState(false)

  const act = async (name: string, fn: () => Promise<UsageFallbackView>) => {
    setBusy(name)
    try {
      onView(await fn())
    } catch (err) {
      toast.error(`${host}: ${(err as Error)?.message || 'failed'}`)
    } finally {
      setBusy(null)
    }
  }

  if (!loaded) {
    return (
      <div className="flex items-center gap-2 border-t border-border/60 py-3 text-sm text-dimmer">
        <HostBadge host={host} /> <Loader2Icon className="size-3.5 animate-spin" />
      </div>
    )
  }
  if (loaded.state === 'error') {
    return (
      <div className="flex items-center gap-2 border-t border-border/60 py-3 text-sm">
        <HostBadge host={host} />
        <span className="text-xs text-dimmer">{loaded.error}</span>
      </div>
    )
  }
  const v = loaded.view
  if (!v.spawn) {
    return (
      <div className="flex items-center gap-2 border-t border-border/60 py-3 text-sm">
        <HostBadge host={host} />
        <span className="text-xs text-dimmer">runs no sessions</span>
      </div>
    )
  }
  const on = v.settings.enabled
  const overridden = on && v.overrideUntil
  const checks = ['claude', 'codex'].map((h) => v.checks[h]).filter(isCheck)
  const anyExhausted = checks.some((c) => c.primaryState.exhausted)

  return (
    <div className="space-y-2 border-t border-border/60 py-3">
      <div className="flex flex-wrap items-center gap-2">
        <HostBadge host={host} />
        <span
          className={cn(
            'rounded-md px-1.5 py-0.5 text-[0.6875rem] font-semibold uppercase',
            !on ? 'bg-muted text-dimmer' : overridden ? 'bg-muted text-foreground' : anyExhausted ? 'bg-status-waiting/15 text-status-waiting' : 'bg-status-idle/15 text-status-idle',
          )}
        >
          {!on ? 'off' : overridden ? 'overridden' : anyExhausted ? 'at a limit' : 'ok'}
        </span>
      </div>

      {on ? (
        <ul className="space-y-1 text-sm">
          {checks.map((c) => (
            <li key={c.primary} className="flex flex-wrap items-baseline gap-x-2">
              <span className="font-medium">{PROVIDER[c.primaryState.provider] ?? c.primaryState.provider}</span>
              {c.primaryState.exhausted ? (
                <span className="text-status-waiting">
                  <HourglassIcon className="mr-1 inline size-3.5" />
                  at its limit ({c.primaryState.reasons.join(', ')})
                </span>
              ) : (
                <span className="text-dimmer">below its limits</span>
              )}
              <span className="text-xs text-dimmer">
                {harnessLabel(c.primary)} sessions →{' '}
                {c.hold
                  ? 'automatic ones wait'
                  : c.fellBack
                    ? harnessLabel(c.harness)
                    : c.fallback
                      ? c.fallback.usable
                        ? `${harnessLabel(c.primary)} (fallback ${harnessLabel(c.fallback.harness)})`
                        : `${harnessLabel(c.primary)} (fallback ${harnessLabel(c.fallback.harness)} unusable: ${c.fallback.whyNot})`
                      : `${harnessLabel(c.primary)} (no fallback)`}
              </span>
            </li>
          ))}
        </ul>
      ) : null}
      {overridden ? <div className="text-sm">Check off until {hhmm(v.overrideUntil!)}: sessions start on the agent asked for.</div> : null}

      <div className="flex flex-wrap gap-2 pt-1">
        {on && !overridden ? (
          <Button variant={anyExhausted ? 'default' : 'outline'} size="sm" disabled={!!busy} onClick={() => void act('override', () => api.usageOverride(host, 60))}>
            <UnlockIcon /> Ignore limits 1 h
          </Button>
        ) : null}
        {overridden ? (
          <Button variant="outline" size="sm" disabled={!!busy} onClick={() => void act('override', () => api.usageOverride(host, 0))}>
            End the override
          </Button>
        ) : null}
        <Button variant="outline" size="sm" disabled={!!busy} onClick={() => setEditing((e) => !e)}>
          {editing ? 'Close settings' : 'Settings'}
        </Button>
      </div>

      {editing ? <SettingsForm view={v} onSaved={(nv) => (onView(nv), toast.success(`Saved on ${host}`))} /> : null}
    </div>
  )
}

function SettingsForm({ view, onSaved }: { view: UsageFallbackView; onSaved: (v: UsageFallbackView) => void }) {
  const [s, setS] = useState<UsageFallbackSettings>(view.settings)
  const [saving, setSaving] = useState(false)
  const num = (k: keyof UsageFallbackSettings['limits'], label: string, hint: string) => (
    <label className="flex items-center justify-between gap-2 text-sm" title={hint}>
      <span>{label}</span>
      <Input
        type="number"
        min={k === 'paceMargin' ? 0 : 1}
        max={100}
        value={s.limits[k]}
        onChange={(e) => setS({ ...s, limits: { ...s.limits, [k]: Number(e.target.value) } })}
        className="h-8 w-20 text-right"
      />
    </label>
  )
  const save = async () => {
    setSaving(true)
    try {
      onSaved(await api.saveUsageSettings(view.host, s))
    } catch (err) {
      toast.error((err as Error)?.message || 'could not save')
    } finally {
      setSaving(false)
    }
  }
  return (
    <div className="space-y-3 rounded-lg border border-border/60 p-3">
      <label className="flex items-center justify-between gap-2 text-sm">
        <span>Fall back when a subscription is at its limit</span>
        <Switch checked={s.enabled} onCheckedChange={(enabled) => setS({ ...s, enabled })} />
      </label>
      <div className="grid gap-2 sm:grid-cols-3">
        {num('session', 'Session limit %', 'A subscription whose current (5-hour) session is at this share counts as at its limit')}
        {num('weekly', 'Weekly limit %', 'A weekly limit at this share counts as at its limit')}
        {num('paceMargin', 'Pace margin', 'Bots and schedules only: switch when the week is used this many points ahead of the share of the week gone')}
      </div>
      <div className="space-y-1">
        {HARNESSES.map((from) => (
          <label key={from} className="flex items-center justify-between gap-2 text-sm">
            <span>{harnessLabel(from)} falls back to</span>
            <select
              value={s.fallback[from] ?? ''}
              onChange={(e) => setS({ ...s, fallback: { ...s.fallback, [from]: e.target.value || null } })}
              className="h-8 rounded-md border border-input bg-background px-2 text-sm"
            >
              <option value="">nothing</option>
              {HARNESSES.filter((h) => h !== from).map((h) => (
                <option key={h} value={h}>
                  {harnessLabel(h)}
                </option>
              ))}
            </select>
          </label>
        ))}
      </div>
      <div className="flex justify-end">
        <Button size="sm" disabled={saving} onClick={() => void save()}>
          {saving ? <Loader2Icon className="animate-spin" /> : null} Save
        </Button>
      </div>
    </div>
  )
}
