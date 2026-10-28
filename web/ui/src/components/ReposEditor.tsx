import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { ChevronDownIcon, CircleAlertIcon, Loader2Icon, PlusIcon, RefreshCwIcon, Trash2Icon } from 'lucide-react'
import { toast } from 'sonner'

import { ApiError, api } from '@/api/client'
import type { RepoRow, ReposResponse, ReposSettings } from '@/api/types'
import { Button } from '@/components/ui/button'
import {
  DropdownMenu,
  DropdownMenuContent,
  DropdownMenuRadioGroup,
  DropdownMenuRadioItem,
  DropdownMenuSeparator,
  DropdownMenuTrigger,
} from '@/components/ui/dropdown-menu'
import { Input } from '@/components/ui/input'
import { Switch } from '@/components/ui/switch'
import { ToggleGroup, ToggleGroupItem } from '@/components/ui/toggle-group'
import { useFleet } from '@/hooks/useFleet'
import { useNow } from '@/hooks/useNow'
import { relTime } from '@/lib/format'
import {
  OUTCOME_LABEL,
  REPO_INTERVALS,
  outcomeTone,
  repoPosition,
  rootProblem,
  rowInterval,
  sameSettings,
  setRowInterval,
  syncSummary,
  type OutcomeTone,
} from '@/lib/repos'
import { cn } from '@/lib/utils'

type Load = { state: 'ok'; data: ReposResponse } | { state: 'error'; error: string }

const segItem =
  'h-8 flex-1 rounded-md px-2 text-[0.8125rem] text-muted-foreground data-[state=on]:bg-background data-[state=on]:text-foreground data-[state=on]:shadow-sm dark:data-[state=on]:bg-accent'

const TONE: Record<OutcomeTone, string> = {
  ok: 'text-dimmer',
  warn: 'text-status-waiting',
  bad: 'text-destructive',
  muted: 'text-dimmer',
}

function errorText(err: unknown): string {
  if (err instanceof ApiError && err.status === 501) return err.message || 'this host’s server is too old — update fleet there'
  return (err as Error)?.message || 'unreachable'
}

/**
 * Settings → Git repos: `fleet repos` on each host — which roots it scans, how often each repo
 * is fetched and fast-forwarded, the launchd timer, and every repo's state. Each host has its
 * own settings (Save writes the shown host; "Save to all" writes the same settings everywhere).
 */
export function ReposEditor() {
  const { fleet } = useFleet()
  const hostsKey = (fleet?.hosts ?? []).map((h) => h.name).join('\n')
  const hosts = useMemo(() => (hostsKey ? hostsKey.split('\n') : []), [hostsKey])
  const [host, setHost] = useState<string | null>(null)
  const current = host ?? fleet?.self ?? hosts[0] ?? null

  const [loads, setLoads] = useState<Record<string, Load>>({})
  const [drafts, setDrafts] = useState<Record<string, ReposSettings>>({})
  const [busy, setBusy] = useState<'save' | 'saveAll' | 'sync' | 'service' | null>(null)
  const now = useNow(30_000)

  // No entry yet renders as loading; `requested` keeps a host from being fetched twice.
  const requested = useRef(new Set<string>())
  const load = useCallback(async (h: string) => {
    requested.current.add(h)
    try {
      const data = await api.repos(h)
      setLoads((l) => ({ ...l, [h]: { state: 'ok', data } }))
      return data
    } catch (err) {
      setLoads((l) => ({ ...l, [h]: { state: 'error', error: errorText(err) } }))
      return null
    }
  }, [])

  useEffect(() => {
    if (current && !requested.current.has(current)) void load(current)
  }, [current, load])

  const l = current ? loads[current] : undefined
  const data = l?.state === 'ok' ? l.data : null
  const draft = current ? (drafts[current] ?? data?.settings ?? null) : null
  const dirty = Boolean(data && draft && !sameSettings(draft, data.settings))
  const rootErrors = draft?.roots.map(rootProblem) ?? []
  const valid = Boolean(draft && draft.roots.length && rootErrors.every((e) => !e))

  useEffect(() => {
    if (!dirty) return
    const onBeforeUnload = (e: BeforeUnloadEvent) => e.preventDefault()
    window.addEventListener('beforeunload', onBeforeUnload)
    return () => window.removeEventListener('beforeunload', onBeforeUnload)
  }, [dirty])

  const edit = (fn: (s: ReposSettings) => ReposSettings) => {
    if (!current || !draft) return
    setDrafts((d) => ({ ...d, [current]: fn(draft) }))
  }

  const clean = (s: ReposSettings): ReposSettings => ({ ...s, roots: s.roots.map((r) => r.trim()).filter(Boolean) })

  const save = async (all: boolean) => {
    if (!current || !draft || !valid) return
    setBusy(all ? 'saveAll' : 'save')
    const targets = all ? hosts : [current]
    const settings = clean(draft)
    const out = await Promise.all(
      targets.map(async (h) => {
        try {
          const r = await api.saveRepos(h, settings)
          setLoads((x) => ({ ...x, [h]: { state: 'ok', data: r } }))
          setDrafts((d) => {
            const { [h]: _, ...rest } = d
            return rest
          })
          return { h, ok: true, msg: '' }
        } catch (err) {
          return { h, ok: false, msg: errorText(err) }
        }
      }),
    )
    setBusy(null)
    const failed = out.filter((r) => !r.ok)
    if (!failed.length) toast.success('Repo settings saved', { description: `On ${targets.join(' and ')}` })
    else toast.error(failed.length === out.length ? 'Not saved' : 'Saved on some hosts only', { description: failed.map((r) => `${r.h}: ${r.msg}`).join('\n') })
  }

  const sync = async (names: string[] = []) => {
    if (!current) return
    setBusy('sync')
    try {
      const r = await api.syncRepos(current, names)
      const s = syncSummary(r.results)
      ;(s.bad ? toast.warning : toast.success)(s.title, { description: s.description })
    } catch (err) {
      toast.error('Sync failed', { description: errorText(err) })
    } finally {
      setBusy(null)
      void load(current)
    }
  }

  const setService = async (install: boolean) => {
    if (!current) return
    setBusy('service')
    try {
      const r = await api.reposService(current, install)
      setLoads((x) => ({ ...x, [current]: { state: 'ok', data: r } }))
      toast.success(install ? 'Auto-sync on' : 'Auto-sync off', { description: current })
    } catch (err) {
      toast.error('Could not change auto-sync', { description: errorText(err) })
    } finally {
      setBusy(null)
    }
  }

  if (!fleet) return <p className="py-3 text-sm text-dimmer">Loading hosts…</p>

  return (
    <div className="space-y-3 py-3">
      <p className="text-xs text-dimmer">
        Every git repo directly under these directories is fetched and fast-forwarded on its interval. Nothing is ever stashed, reset or rebased: a repo that
        can’t fast-forward is only reported.
      </p>

      {hosts.length > 1 ? (
        <ToggleGroup
          type="single"
          value={current ?? ''}
          onValueChange={(v) => v && setHost(v)}
          spacing={0}
          aria-label="Host"
          className="w-full rounded-lg bg-muted p-1"
        >
          {hosts.map((h) => (
            <ToggleGroupItem key={h} value={h} className={segItem}>
              {h}
              {drafts[h] && loads[h]?.state === 'ok' && !sameSettings(drafts[h], (loads[h] as { data: ReposResponse }).data.settings) ? ' •' : ''}
            </ToggleGroupItem>
          ))}
        </ToggleGroup>
      ) : null}

      {l?.state === 'error' ? (
        <div className="space-y-2">
          <p role="status" className="flex items-start gap-1.5 text-xs text-status-waiting">
            <CircleAlertIcon className="mt-px size-3.5 shrink-0" />
            <span>
              {current}: {l.error}
            </span>
          </p>
          <Button variant="outline" size="sm" onClick={() => current && void load(current)}>
            Retry
          </Button>
        </div>
      ) : !data || !draft ? (
        <p className="flex items-center gap-2 text-sm text-dimmer">
          <Loader2Icon className="size-4 animate-spin" /> Loading…
        </p>
      ) : (
        <>
          <div className="flex min-h-12 items-center justify-between gap-3">
            <div className="min-w-0">
              <div className="text-sm">Auto-sync</div>
              <div className="text-xs text-dimmer">
                {data.service.supported
                  ? `launchd checks every ${data.service.tickMinutes} min (and at login); each repo syncs when its interval is up`
                  : 'macOS only — elsewhere run `fleet repos sync --due` from cron'}
              </div>
            </div>
            {busy === 'service' ? <Loader2Icon className="size-4 animate-spin text-dimmer" /> : null}
            <Switch
              checked={data.service.installed}
              disabled={!data.service.supported || busy != null}
              onCheckedChange={(v) => void setService(v)}
              aria-label={`Auto-sync on ${current}`}
            />
          </div>

          <div className="space-y-2">
            <div>
              <div className="text-sm">Default interval</div>
              <div className="text-xs text-dimmer">A daily one also runs on the first check of each new day</div>
            </div>
            <ToggleGroup
              type="single"
              value={draft.every}
              onValueChange={(v) => v && edit((s) => ({ ...s, every: v }))}
              spacing={0}
              aria-label="Default interval"
              className="w-full rounded-lg bg-muted p-1"
            >
              {(REPO_INTERVALS as readonly string[]).concat(REPO_INTERVALS.includes(draft.every as never) ? [] : [draft.every]).map((v) => (
                <ToggleGroupItem key={v} value={v} className={segItem}>
                  {v}
                </ToggleGroupItem>
              ))}
            </ToggleGroup>
          </div>

          <div className="space-y-2">
            <div>
              <div className="text-sm">Directories</div>
              <div className="text-xs text-dimmer">Scanned one level deep on {current}</div>
            </div>
            {draft.roots.map((r, i) => {
              const id = `repos-root-${i}`
              const problem = rootErrors[i]
              return (
                <div key={i} className="flex items-start gap-1">
                  <div className="min-w-0 flex-1">
                    <label htmlFor={id} className="sr-only">
                      Directory {i + 1}
                    </label>
                    <Input
                      id={id}
                      value={r}
                      placeholder="~/Code"
                      spellCheck={false}
                      autoCapitalize="off"
                      autoCorrect="off"
                      aria-invalid={problem ? true : undefined}
                      onChange={(e) => edit((s) => ({ ...s, roots: s.roots.map((x, j) => (j === i ? e.target.value : x)) }))}
                      className="h-9 font-mono text-[0.8125rem] md:h-8"
                    />
                    {problem ? <p className="pt-1 text-xs text-destructive">{problem}</p> : null}
                  </div>
                  <Button
                    variant="ghost"
                    size="icon"
                    aria-label={`Remove ${r || 'directory'}`}
                    title="Remove"
                    disabled={draft.roots.length <= 1}
                    onClick={() => edit((s) => ({ ...s, roots: s.roots.filter((_, j) => j !== i) }))}
                    className="size-9 text-muted-foreground hover:text-destructive md:size-8"
                  >
                    <Trash2Icon />
                  </Button>
                </div>
              )
            })}
            <Button variant="outline" size="sm" disabled={draft.roots.length >= 20} onClick={() => edit((s) => ({ ...s, roots: [...s.roots, ''] }))}>
              <PlusIcon /> Add directory
            </Button>
          </div>

          <div className="flex flex-wrap items-center gap-2 pt-1">
            <Button variant="outline" size="sm" disabled={busy != null} onClick={() => void sync()}>
              {busy === 'sync' ? <Loader2Icon className="animate-spin" /> : <RefreshCwIcon />}
              {busy === 'sync' ? 'Syncing…' : 'Sync all now'}
            </Button>
            <span className="flex-1" />
            {dirty ? <span className="text-xs text-status-waiting">Unsaved changes</span> : null}
            <Button
              variant="ghost"
              size="sm"
              disabled={!dirty || busy != null}
              onClick={() =>
                current &&
                setDrafts((d) => {
                  const { [current]: _, ...rest } = d
                  return rest
                })
              }
            >
              Revert
            </Button>
            {hosts.length > 1 ? (
              <Button variant="outline" size="sm" disabled={!valid || busy != null} onClick={() => void save(true)} title="Write these settings to every host">
                {busy === 'saveAll' ? <Loader2Icon className="animate-spin" /> : null}
                Save to all hosts
              </Button>
            ) : null}
            <Button size="sm" disabled={!dirty || !valid || busy != null} onClick={() => void save(false)}>
              {busy === 'save' ? <Loader2Icon className="animate-spin" /> : null}
              Save
            </Button>
          </div>

          <RepoList
            data={data}
            draft={draft}
            now={now}
            syncing={busy === 'sync'}
            onInterval={(row, v) => edit((s) => setRowInterval(s, row, data.repos, v))}
            onSync={(row) => void sync([row.path])}
          />
        </>
      )}
    </div>
  )
}

function RepoList({
  data,
  draft,
  now,
  syncing,
  onInterval,
  onSync,
}: {
  data: ReposResponse
  draft: ReposSettings
  now: number
  syncing: boolean
  onInterval: (row: RepoRow, value: string) => void
  onSync: (row: RepoRow) => void
}) {
  if (data.error) {
    return (
      <p role="status" className="flex items-start gap-1.5 text-xs text-destructive">
        <CircleAlertIcon className="mt-px size-3.5 shrink-0" /> {data.error}
      </p>
    )
  }
  if (!data.repos.length) return <p className="text-sm text-dimmer">No git repos under these directories (save to rescan).</p>
  return (
    <ul aria-label={`Repos on ${data.host}`} className="divide-y divide-border/60 rounded-xl border">
      {data.repos.map((r) => {
        const v = rowInterval(draft, r)
        const off = v === 'off'
        const tone = outcomeTone(r.outcome)
        return (
          <li key={r.path} className={cn('flex items-center gap-2 px-3 py-2', off && 'opacity-55')}>
            <div className="min-w-0 flex-1">
              <div className="flex min-w-0 items-baseline gap-2">
                <span className="truncate text-sm font-medium" title={r.path}>
                  {r.name}
                </span>
                <span className="truncate font-mono text-xs text-dimmer">{r.branch ?? '(detached)'}</span>
                <span className="shrink-0 font-mono text-xs text-muted-foreground tabular-nums">{repoPosition(r)}</span>
              </div>
              <div className="flex min-w-0 gap-1.5 text-xs">
                <span className="shrink-0 text-dimmer tabular-nums">{r.lastFetch ? `synced ${relTime(r.lastFetch, now)} ago` : 'never synced'}</span>
                {r.outcome ? (
                  <span className={cn('min-w-0 truncate', TONE[tone])} title={r.detail ?? undefined}>
                    · {OUTCOME_LABEL[r.outcome]}
                    {r.detail && tone !== 'ok' ? ` — ${r.detail}` : ''}
                  </span>
                ) : null}
              </div>
            </div>
            <Button
              variant="ghost"
              size="icon"
              aria-label={`Sync ${r.name} now`}
              title="Sync now"
              disabled={syncing}
              onClick={() => onSync(r)}
              className="size-8 shrink-0 text-muted-foreground"
            >
              <RefreshCwIcon className="size-3.5" />
            </Button>
            <DropdownMenu>
              <DropdownMenuTrigger asChild>
                <Button
                  variant="outline"
                  size="sm"
                  aria-label={`${r.name} interval`}
                  title={v === 'default' ? 'The default interval' : v === 'off' ? 'Excluded: never synced' : 'Its own interval'}
                  className={cn('w-[6.5rem] justify-between tabular-nums', v === 'default' ? 'font-normal text-muted-foreground' : 'font-semibold')}
                >
                  {v === 'default' ? draft.every : v}
                  <ChevronDownIcon className="size-3.5 opacity-60" />
                </Button>
              </DropdownMenuTrigger>
              <DropdownMenuContent align="end">
                <DropdownMenuRadioGroup value={v} onValueChange={(x) => onInterval(r, x)}>
                  <DropdownMenuRadioItem value="default">Default ({draft.every})</DropdownMenuRadioItem>
                  <DropdownMenuSeparator />
                  {(REPO_INTERVALS as readonly string[]).concat(REPO_INTERVALS.includes(v as never) || v === 'default' || v === 'off' ? [] : [v]).map((x) => (
                    <DropdownMenuRadioItem key={x} value={x}>
                      Every {x}
                    </DropdownMenuRadioItem>
                  ))}
                  <DropdownMenuSeparator />
                  <DropdownMenuRadioItem value="off">Off (excluded)</DropdownMenuRadioItem>
                </DropdownMenuRadioGroup>
              </DropdownMenuContent>
            </DropdownMenu>
          </li>
        )
      })}
    </ul>
  )
}

