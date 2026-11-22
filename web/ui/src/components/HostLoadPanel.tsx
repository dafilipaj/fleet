import { useCallback, useEffect, useMemo, useState } from 'react'
import { GaugeIcon, Loader2Icon, MoonIcon, RefreshCwIcon, RotateCwIcon, UnlockIcon } from 'lucide-react'
import { toast } from 'sonner'

import { ApiError, api } from '@/api/client'
import type { HostLoadView, HostProcesses, LoadLevel, LoadSettings } from '@/api/types'
import { HostBadge } from '@/components/HostBadge'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { useFleet } from '@/hooks/useFleet'
import { relTime } from '@/lib/format'
import { cn } from '@/lib/utils'

type Loaded = { state: 'ok'; view: HostLoadView } | { state: 'error'; error: string }

const POLL_MS = 15 * 1000

const levelClass: Record<LoadLevel | 'off', string> = {
  ok: 'bg-status-idle/15 text-status-idle',
  hot: 'bg-status-waiting/15 text-status-waiting',
  critical: 'bg-destructive/15 text-destructive',
  off: 'bg-muted text-dimmer',
}

const why = (err: unknown) => (err instanceof ApiError && (err.status === 404 || err.status === 501) ? 'update fleet there' : (err as Error)?.message || 'unreachable')

/**
 * Settings → Host load: every host's load verdict and numbers, its `resources` settings (saved
 * per host, applied live), and what to do about a busy one — let new sessions start for a while,
 * see which sessions use the machine and deactivate one, sample again, restart its web server.
 */
export function HostLoadPanel() {
  const { fleet } = useFleet()
  const hostsKey = (fleet?.hosts ?? []).map((h) => h.name).join('\n')
  const hosts = useMemo(() => (hostsKey ? hostsKey.split('\n') : []), [hostsKey])
  const [loads, setLoads] = useState<Record<string, Loaded>>({})

  const load = useCallback(async () => {
    const got = await Promise.all(
      hosts.map(async (h): Promise<[string, Loaded]> => {
        try {
          return [h, { state: 'ok', view: await api.hostLoad(h) }]
        } catch (err) {
          return [h, { state: 'error', error: why(err) }]
        }
      }),
    )
    setLoads(Object.fromEntries(got))
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

  const put = (h: string, view: HostLoadView) => setLoads((l) => ({ ...l, [h]: { state: 'ok', view } }))

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
        While a host is under heavy load, a session you start there asks first and the automatic ones (Slack, GitHub, bots, schedule) wait until it recovers. Send,
        peek and kill always work. Each host keeps its own settings; changes apply at once.
      </p>
      {hosts.map((h) => {
        const l = loads[h]
        return <HostCard key={h} host={h} loaded={l} onView={(v) => put(h, v)} />
      })}
    </div>
  )
}

function HostCard({ host, loaded, onView }: { host: string; loaded: Loaded | undefined; onView: (v: HostLoadView) => void }) {
  const [busy, setBusy] = useState<string | null>(null)
  const [procs, setProcs] = useState<HostProcesses | null>(null)
  const [procsOpen, setProcsOpen] = useState(false)
  const [editing, setEditing] = useState(false)
  const [confirmRestart, setConfirmRestart] = useState(false)

  const act = async (name: string, fn: () => Promise<HostLoadView | void>) => {
    setBusy(name)
    try {
      const v = await fn()
      if (v) onView(v)
    } catch (err) {
      toast.error(`${host}: ${(err as Error)?.message || 'failed'}`)
    } finally {
      setBusy(null)
    }
  }

  const loadProcs = async () => {
    setProcsOpen(true)
    setBusy('procs')
    try {
      setProcs(await api.hostProcesses(host))
    } catch (err) {
      toast.error(`${host}: ${(err as Error)?.message || 'could not list processes'}`)
      setProcsOpen(false)
    } finally {
      setBusy(null)
    }
  }

  const deactivate = async (id: string, name: string) => {
    setBusy(`sleep:${id}`)
    try {
      await api.sleep(host, id)
      toast.success(`Deactivated ${name} on ${host}`, { description: 'Resume it from Inactive sessions.' })
      await loadProcs()
    } catch (err) {
      toast.error(`${host}: ${(err as Error)?.message || 'could not deactivate'}`)
    } finally {
      setBusy(null)
    }
  }

  const restart = async () => {
    setConfirmRestart(false)
    await act('restart', async () => {
      await api.restartHostWeb(host)
      toast.success(`Restarting ${host}'s web server`, { description: 'It answers again in a few seconds.' })
    })
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
  const l = v.load
  const s = l?.sample
  const label: LoadLevel | 'off' = v.enabled ? (l?.override ? 'ok' : (l?.state ?? 'ok')) : 'off'
  const held = v.enabled && l && l.state !== 'ok'
  const overridden = v.enabled && l?.override

  return (
    <div className="space-y-2 border-t border-border/60 py-3">
      <div className="flex flex-wrap items-center gap-2">
        <HostBadge host={host} />
        <span className={cn('rounded-md px-1.5 py-0.5 text-[0.6875rem] font-semibold uppercase', levelClass[label])}>{v.enabled ? label : 'off'}</span>
        {v.enabled && l && l.state !== 'ok' ? <span className="text-xs text-dimmer">since {relTime(l.since)}</span> : null}
        <span className="ml-auto flex items-center gap-1">
          <Button variant="ghost" size="sm" disabled={!!busy} onClick={() => void act('sample', () => api.sampleHostLoad(host))} title="Sample now">
            {busy === 'sample' ? <Loader2Icon className="animate-spin" /> : <RefreshCwIcon />}
          </Button>
        </span>
      </div>

      {held && l?.reasons.length ? <div className="text-sm text-status-waiting">{l.reasons.join(', ')} — new sessions wait.</div> : null}
      {overridden ? (
        <div className="text-sm">
          New sessions allowed whatever the load until {new Date(l!.override!.until).toLocaleTimeString([], { hour: '2-digit', minute: '2-digit' })}
          {l!.override!.held !== 'ok' ? <span className="text-dimmer"> (it would be {l!.override!.held})</span> : null}.
        </div>
      ) : null}

      {s ? (
        <div className="grid grid-cols-2 gap-x-4 gap-y-0.5 text-xs text-dimmer sm:grid-cols-4">
          <Metric
            label={s.load5 != null ? 'Load (5 min)' : 'Load'}
            value={`${s.perCore}× cores`}
            hint={`${s.load5 ?? s.load1} on ${s.cores} cores${s.load5 != null ? `; 1 min: ${s.load1}` : ''}`}
            warn={v.enabled ? s.perCore >= v.settings.load.hot : false}
          />
          {s.freePct != null ? <Metric label="Memory free" value={`${s.freePct}%`} warn={s.freePct < (v.settings.memory?.hot ?? 10)} /> : null}
          {s.pressure ? <Metric label="Memory pressure" value={s.pressure} warn={s.pressure === 'critical'} /> : null}
          {s.swapUsedMb != null ? <Metric label="Swap used" value={`${(s.swapUsedMb / 1024).toFixed(1)} GB`} /> : null}
          {s.psi ? <Metric label="Memory stalls" value={`${s.psi.some ?? 0}% some · ${s.psi.full ?? 0}% full`} warn={(s.psi.some ?? 0) >= v.settings.psi.hot} /> : null}
          {l?.level && l.level !== l.state ? <Metric label="Right now" value={l.level} hint={l.latest.join(', ')} /> : null}
        </div>
      ) : l?.error ? (
        <div className="text-xs text-dimmer">Cannot read this machine's load ({l.error}) — it counts as ok.</div>
      ) : null}

      <div className="flex flex-wrap gap-2 pt-1">
        {v.enabled && !overridden ? (
          <>
            <Button variant={held ? 'default' : 'outline'} size="sm" disabled={!!busy} onClick={() => void act('override', () => api.overrideHostLoad(host, 30))}>
              <UnlockIcon /> Allow new sessions 30 min
            </Button>
            {held ? (
              <Button variant="outline" size="sm" disabled={!!busy} onClick={() => void act('override', () => api.overrideHostLoad(host, 120))}>
                2 h
              </Button>
            ) : null}
          </>
        ) : null}
        {overridden ? (
          <Button variant="outline" size="sm" disabled={!!busy} onClick={() => void act('override', () => api.overrideHostLoad(host, 0))}>
            End the override
          </Button>
        ) : null}
        <Button variant="outline" size="sm" disabled={!!busy} onClick={() => (procsOpen ? setProcsOpen(false) : void loadProcs())}>
          {busy === 'procs' ? <Loader2Icon className="animate-spin" /> : <GaugeIcon />} {procsOpen ? 'Hide' : 'What uses it'}
        </Button>
        <Button variant="outline" size="sm" disabled={!!busy} onClick={() => setEditing((e) => !e)}>
          {editing ? 'Close settings' : 'Settings'}
        </Button>
        {v.restart ? (
          confirmRestart ? (
            <>
              <Button variant="destructive" size="sm" disabled={!!busy} onClick={() => void restart()}>
                Restart its web server?
              </Button>
              <Button variant="ghost" size="sm" onClick={() => setConfirmRestart(false)}>
                Cancel
              </Button>
            </>
          ) : (
            <Button variant="ghost" size="sm" disabled={!!busy} onClick={() => setConfirmRestart(true)} title="Restart this host's fleet web server (launchd / systemd starts it again)">
              {busy === 'restart' ? <Loader2Icon className="animate-spin" /> : <RotateCwIcon />} Restart web
            </Button>
          )
        ) : null}
      </div>

      {procsOpen && procs ? <ProcessList host={host} procs={procs} busy={busy} onDeactivate={deactivate} /> : null}
      {editing ? <SettingsForm view={v} onSaved={(nv) => (onView(nv), toast.success(`Saved on ${host}`))} /> : null}
    </div>
  )
}

function Metric({ label, value, hint, warn }: { label: string; value: string; hint?: string; warn?: boolean }) {
  return (
    <div title={hint}>
      {label}: <span className={cn('font-medium text-foreground', warn && 'text-status-waiting')}>{value}</span>
    </div>
  )
}

function ProcessList({ host, procs, busy, onDeactivate }: { host: string; procs: HostProcesses; busy: string | null; onDeactivate: (id: string, name: string) => void }) {
  const [sure, setSure] = useState<string | null>(null)
  const gb = (mb: number) => (mb >= 1024 ? `${(mb / 1024).toFixed(1)} GB` : `${mb} MB`)
  return (
    <div className="space-y-2 rounded-lg border border-border/60 p-2 text-sm">
      <div className="text-xs text-dimmer">
        Sessions use {gb(procs.sessionsMb)} of {gb(procs.totalMb)} resident memory. Deactivating one ends its agent and keeps it resumable.
      </div>
      {procs.sessions.length ? (
        <ul className="divide-y divide-border/60">
          {procs.sessions.map((s) => (
            <li key={s.pid} className="flex items-center gap-2 py-1.5">
              <span className="min-w-0 flex-1">
                {s.id ? (
                  <a href={`#/s/${encodeURIComponent(host)}/${encodeURIComponent(s.id)}`} className="block truncate hover:underline">
                    {s.name}
                  </a>
                ) : (
                  <span className="block truncate">{s.name}</span>
                )}
                <span className="block text-xs text-dimmer">
                  {s.status ?? '—'} · {s.processes} process{s.processes === 1 ? '' : 'es'}
                </span>
              </span>
              <span className="w-16 text-right font-mono text-xs tabular-nums">{gb(s.rssMb)}</span>
              <span className="w-12 text-right font-mono text-xs text-dimmer tabular-nums">{s.cpu}%</span>
              {s.id ? (
                sure === s.id ? (
                  <Button variant="destructive" size="sm" disabled={!!busy} onClick={() => (setSure(null), onDeactivate(s.id!, s.name))}>
                    {busy === `sleep:${s.id}` ? <Loader2Icon className="animate-spin" /> : null} Sure?
                  </Button>
                ) : (
                  <Button variant="ghost" size="sm" disabled={!!busy} onClick={() => setSure(s.id)} title="Deactivate: end its agent, keep it resumable">
                    <MoonIcon />
                  </Button>
                )
              ) : (
                <span className="w-9" />
              )}
            </li>
          ))}
        </ul>
      ) : (
        <div className="text-xs text-dimmer">No agent sessions on {host}.</div>
      )}
      {procs.others.length ? (
        <>
          <div className="pt-1 text-xs font-semibold text-dimmer">Other heavy processes</div>
          <ul className="space-y-0.5 text-xs">
            {procs.others.map((p) => (
              <li key={p.pid} className="flex gap-2">
                <span className="min-w-0 flex-1 truncate">{p.name}</span>
                <span className="w-16 text-right font-mono tabular-nums">{gb(p.rssMb)}</span>
                <span className="w-12 text-right font-mono text-dimmer tabular-nums">{p.cpu}%</span>
                <span className="w-9" />
              </li>
            ))}
          </ul>
        </>
      ) : null}
    </div>
  )
}

function SettingsForm({ view, onSaved }: { view: HostLoadView; onSaved: (v: HostLoadView) => void }) {
  const [form, setForm] = useState<LoadSettings>(view.settings)
  const [saving, setSaving] = useState(false)
  const [error, setError] = useState<string | null>(null)
  const linux = !!view.load?.sample?.psi
  const num = (v: string) => Number(v.replace(',', '.'))
  const field = (label: string, value: number, set: (n: number) => void, hint?: string, step = '0.5') => (
    <label className="flex items-center gap-2 text-sm">
      <span className="min-w-0 flex-1">
        {label}
        {hint ? <span className="block text-xs text-dimmer">{hint}</span> : null}
      </span>
      <Input type="number" inputMode="decimal" step={step} min="0" className="h-8 w-24 text-right" value={String(value)} onChange={(e) => set(num(e.target.value))} />
    </label>
  )

  const save = async () => {
    setSaving(true)
    setError(null)
    try {
      onSaved(await api.setHostLoad(view.host, form))
    } catch (err) {
      setError((err as Error)?.message || 'could not save')
    } finally {
      setSaving(false)
    }
  }

  return (
    <div className="space-y-3 rounded-lg border border-border/60 p-3">
      <label className="flex items-center gap-2 text-sm">
        <span className="min-w-0 flex-1">
          Hold new sessions under heavy load
          <span className="block text-xs text-dimmer">Off: {view.host} is only measured.</span>
        </span>
        <Switch checked={form.enabled} onCheckedChange={(enabled) => setForm({ ...form, enabled })} />
      </label>
      {field('Hot at load', form.load.hot, (hot) => setForm({ ...form, load: { ...form.load, hot } }), `× cores (5-minute average; default ${view.defaults.load.hot})`)}
      {field('Critical at load', form.load.critical, (critical) => setForm({ ...form, load: { ...form.load, critical } }), `× cores (default ${view.defaults.load.critical})`)}
      {linux
        ? field('Hot at memory stalls', form.psi.hot, (hot) => setForm({ ...form, psi: { ...form.psi, hot } }), `% of time some tasks wait on memory (default ${view.defaults.psi.hot})`, '1')
        : null}
      {linux
        ? field('Critical at memory stalls', form.psi.critical, (critical) => setForm({ ...form, psi: { ...form.psi, critical } }), `% of time all tasks wait (default ${view.defaults.psi.critical})`, '1')
        : null}
      {!linux
        ? field('Hot below free memory', form.memory?.hot ?? view.defaults.memory.hot, (hot) => setForm({ ...form, memory: { ...(form.memory ?? view.defaults.memory), hot } }), `% (default ${view.defaults.memory.hot})`, '1')
        : null}
      {!linux
        ? field(
            'Critical below free memory',
            form.memory?.critical ?? view.defaults.memory.critical,
            (critical) => setForm({ ...form, memory: { ...(form.memory ?? view.defaults.memory), critical } }),
            `% (default ${view.defaults.memory.critical}); macOS memory pressure “critical” always counts`,
            '1',
          )
        : null}
      {field('Goes hot after', form.enterSeconds, (enterSeconds) => setForm({ ...form, enterSeconds }), `seconds above the threshold (default ${view.defaults.enterSeconds})`, '15')}
      {field(
        'Goes critical after',
        form.criticalEnterSeconds ?? view.defaults.criticalEnterSeconds,
        (criticalEnterSeconds) => setForm({ ...form, criticalEnterSeconds }),
        `seconds above the critical threshold (default ${view.defaults.criticalEnterSeconds})`,
        '15',
      )}
      {field('Recovers after', form.exitSeconds, (exitSeconds) => setForm({ ...form, exitSeconds }), `seconds below it (default ${view.defaults.exitSeconds})`, '15')}
      <label className="flex items-center gap-2 text-sm">
        <span className="min-w-0 flex-1">
          Slack DM when it goes critical
          <span className="block text-xs text-dimmer">And when it is no longer critical. Off for a second fleet on the same machine.</span>
        </span>
        <Switch checked={form.alerts !== false} onCheckedChange={(alerts) => setForm({ ...form, alerts })} />
      </label>
      {error ? <div className="text-sm text-destructive">{error}</div> : null}
      <div className="flex gap-2">
        <Button size="sm" disabled={saving} onClick={() => void save()}>
          {saving ? <Loader2Icon className="animate-spin" /> : null} Save on {view.host}
        </Button>
        <Button variant="ghost" size="sm" disabled={saving} onClick={() => setForm({ ...view.defaults, enabled: form.enabled, alerts: form.alerts })}>
          Defaults
        </Button>
      </div>
    </div>
  )
}
