import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { ArrowDownIcon, ArrowUpIcon, CheckIcon, CircleAlertIcon, EyeIcon, EyeOffIcon, Loader2Icon, PlusIcon, Trash2Icon } from 'lucide-react'
import { toast } from 'sonner'

import { ApiError, api, isAbortError } from '@/api/client'
import type { SpawnDirCheck, SpawnDirEntry, SpawnDirsError, SpawnDirsResponse } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { useFleet } from '@/hooks/useFleet'
import {
  SPAWN_DIR_LIMITS,
  baseHost,
  checksByPath,
  divergedHosts,
  emptyRow,
  fromDraft,
  hostPathErrors,
  moveRow,
  sameList,
  toDraft,
  validateDraft,
  withOffered,
  type DraftRow,
} from '@/lib/spawnDirs'
import { cn } from '@/lib/utils'

type Load = { state: 'loading' } | { state: 'ok'; data: SpawnDirsResponse } | { state: 'error'; error: string }
type Result = { host: string; ok: boolean; message: string }

const CHECK_DEBOUNCE_MS = 600

/**
 * Settings → Start directories: the `spawnDirs` every host offers in the New session form.
 * One row per label with a directory per host (empty = not offered there). The list is
 * shared: Save writes the same list to every reachable host (each validates its own paths
 * first — a dry run on all of them, then the writes), so the hosts' configs stay in sync.
 * A hidden entry (eye toggle) stays allowed for spawns — e.g. an app's embedded panel starting
 * sessions in its own dir — but is left out of the New session menu.
 */
export function SpawnDirsEditor() {
  const { fleet, applyFleet, refresh } = useFleet()
  const hostsKey = (fleet?.hosts ?? []).map((h) => h.name).join('\n')
  const hosts = useMemo(() => (hostsKey ? hostsKey.split('\n') : []), [hostsKey])
  const self = fleet?.self

  const [loads, setLoads] = useState<Record<string, Load>>({})
  const [base, setBase] = useState<SpawnDirEntry[] | null>(null)
  const [baseFrom, setBaseFrom] = useState<string | null>(null)
  const [rows, setRows] = useState<DraftRow[]>([])
  // Per host: path → the host's own check / error message (GET, dry runs, saves).
  const [checks, setChecks] = useState<Record<string, Record<string, SpawnDirCheck>>>({})
  const [serverErrors, setServerErrors] = useState<Record<string, Record<string, string>>>({})
  const [checking, setChecking] = useState(false)
  const [saving, setSaving] = useState(false)
  const [results, setResults] = useState<Result[] | null>(null)

  const loaded = useMemo(() => {
    const out: Record<string, SpawnDirsResponse> = {}
    for (const [h, l] of Object.entries(loads)) if (l.state === 'ok') out[h] = l.data
    return out
  }, [loads])
  const reachable = useMemo(() => hosts.filter((h) => loads[h]?.state === 'ok'), [hosts, loads])

  const noteChecks = useCallback((host: string, r: { spawnDirs?: SpawnDirEntry[]; checks?: (SpawnDirCheck | null)[] }, sent?: SpawnDirEntry[]) => {
    setChecks((c) => ({ ...c, [host]: { ...c[host], ...checksByPath(r, sent, host) } }))
  }, [])

  const load = useCallback(async () => {
    if (!hosts.length) return
    setLoads(Object.fromEntries(hosts.map((h) => [h, { state: 'loading' } as Load])))
    const got = await Promise.all(
      hosts.map(async (h): Promise<[string, Load]> => {
        try {
          const data = await api.spawnDirs(h)
          noteChecks(h, data)
          return [h, { state: 'ok', data }]
        } catch (err) {
          return [h, { state: 'error', error: err instanceof ApiError && err.status === 501 ? 'this host’s server is too old to edit its list' : (err as Error)?.message || 'unreachable' }]
        }
      }),
    )
    const next = Object.fromEntries(got)
    setLoads(next)
    const ok: Record<string, SpawnDirsResponse> = {}
    for (const [h, l] of got) if (l.state === 'ok') ok[h] = l.data
    const from = baseHost(self, ok, hosts)
    setBaseFrom(from)
    setBase(from ? ok[from].spawnDirs : null)
    setRows(from ? toDraft(ok[from].spawnDirs) : [])
    setServerErrors({})
  }, [hosts, self, noteChecks])

  const loadedFor = useRef('')
  useEffect(() => {
    if (!hostsKey || loadedFor.current === hostsKey) return
    loadedFor.current = hostsKey
    void load()
  }, [hostsKey, load])

  const draft = useMemo(() => fromDraft(rows), [rows])
  const dirty = base != null && !sameList(draft, base)
  const validation = useMemo(() => validateDraft(rows), [rows])
  const diverged = base ? divergedHosts(base, loaded).filter((h) => h !== baseFrom) : []

  // Unsaved changes: the browser asks before a reload / close.
  useEffect(() => {
    if (!dirty) return
    const onBeforeUnload = (e: BeforeUnloadEvent) => e.preventDefault()
    window.addEventListener('beforeunload', onBeforeUnload)
    return () => window.removeEventListener('beforeunload', onBeforeUnload)
  }, [dirty])

  /** Dry-run `list` on every reachable host: records checks + path errors; true when all accept it. */
  const dryRun = useCallback(
    async (list: SpawnDirEntry[], signal?: AbortSignal) => {
      const answers = await Promise.all(
        reachable.map(async (h) => {
          try {
            const r = await api.saveSpawnDirs(h, list, { dryRun: true, signal })
            noteChecks(h, r, list)
            return [h, {}] as const
          } catch (err) {
            if (isAbortError(err)) throw err
            const data = err instanceof ApiError && err.status === 400 ? (err.data as SpawnDirsError) : null
            if (data) noteChecks(h, data, list)
            return [h, data ? hostPathErrors(data, list, h) : null] as const
          }
        }),
      )
      setServerErrors(Object.fromEntries(answers.map(([h, e]) => [h, e ?? {}])))
      return answers.every(([, e]) => e && !Object.keys(e).length)
    },
    [reachable, noteChecks],
  )

  // Live check of the draft on each host (debounced): ✓ / ✗ per path cell. Also right after
  // loading when the hosts' lists differ (a host has not checked the other list's paths).
  const needsCheck = dirty || diverged.length > 0
  useEffect(() => {
    if (!needsCheck || !reachable.length || draft.length > SPAWN_DIR_LIMITS.maxEntries) return
    const ctl = new AbortController()
    const t = setTimeout(() => {
      setChecking(true)
      dryRun(draft, ctl.signal)
        .catch(() => {})
        .finally(() => !ctl.signal.aborted && setChecking(false))
    }, CHECK_DEBOUNCE_MS)
    return () => {
      clearTimeout(t)
      ctl.abort()
    }
  }, [draft, needsCheck, dryRun, reachable.length])

  const save = async () => {
    if (!validation.ok || saving) return
    setSaving(true)
    setResults(null)
    const list = draft
    try {
      // 1. every host must accept it (its own paths exist) — else nothing is written anywhere.
      if (!(await dryRun(list))) {
        toast.error('Not saved', { description: 'Fix the directories marked below — no host was changed.' })
        return
      }
      // 2. write the same list to every reachable host.
      const saved: Record<string, SpawnDirsResponse> = {}
      const out: Result[] = await Promise.all(
        hosts.map(async (h): Promise<Result> => {
          if (loads[h]?.state !== 'ok') return { host: h, ok: false, message: `skipped — ${loads[h]?.state === 'error' ? (loads[h] as { error: string }).error : 'unreachable'}` }
          try {
            const r = await api.saveSpawnDirs(h, list)
            saved[h] = r
            noteChecks(h, r)
            return { host: h, ok: true, message: `saved · offers ${r.offered.map((d) => d.label).join(', ') || 'nothing'}` }
          } catch (err) {
            return { host: h, ok: false, message: (err as Error)?.message || 'failed' }
          }
        }),
      )
      setResults(out)
      setLoads((l) => ({ ...l, ...Object.fromEntries(Object.entries(saved).map(([h, data]) => [h, { state: 'ok', data } as Load])) }))
      const first = saved[baseFrom ?? ''] ?? Object.values(saved)[0]
      if (first) {
        setBase(first.spawnDirs)
        setRows(toDraft(first.spawnDirs))
      }
      // The New session form reads the fleet: show the new lists now, then re-poll.
      if (fleet && Object.keys(saved).length) applyFleet(withOffered(fleet, saved))
      refresh()
      const failed = out.filter((r) => !r.ok)
      if (!failed.length) toast.success('Start directories saved', { description: `On ${out.map((r) => r.host).join(' and ')}` })
      else if (Object.keys(saved).length) toast.warning('Saved on some hosts only', { description: failed.map((r) => `${r.host}: ${r.message}`).join('\n') })
      else toast.error('Not saved', { description: failed.map((r) => `${r.host}: ${r.message}`).join('\n') })
    } catch (err) {
      toast.error('Not saved', { description: (err as Error)?.message || 'request failed' })
    } finally {
      setSaving(false)
    }
  }

  const revert = () => {
    setRows(toDraft(base))
    setServerErrors({})
    setResults(null)
  }

  const update = (key: string, fn: (r: DraftRow) => DraftRow) => setRows((rs) => rs.map((r) => (r.key === key ? fn(r) : r)))

  if (!fleet) return <p className="py-3 text-sm text-dimmer">Loading hosts…</p>

  const allFailed = hosts.length > 0 && hosts.every((h) => loads[h]?.state === 'error')
  const cols = hosts.length

  return (
    <div className="space-y-3 py-3">
      <p className="text-xs text-dimmer">
        Where a new session can start, per host (empty = not offered there). One list for the whole fleet: Save writes it to every host.
        A hidden directory is still allowed (e.g. for an app’s assistant panel) but not offered in New session.
      </p>

      {hosts
        .filter((h) => loads[h]?.state === 'error')
        .map((h) => (
          <p key={h} role="status" className="flex items-start gap-1.5 text-xs text-status-waiting">
            <CircleAlertIcon className="mt-px size-3.5 shrink-0" />
            <span>
              {h}: {(loads[h] as { error: string }).error} — it is skipped on save.
            </span>
          </p>
        ))}
      {diverged.length ? (
        <p role="status" className="flex items-start gap-1.5 text-xs text-status-waiting">
          <CircleAlertIcon className="mt-px size-3.5 shrink-0" />
          <span>
            {diverged.join(', ')} {diverged.length > 1 ? 'have' : 'has'} a different list; this one is {baseFrom}’s. Saving writes it everywhere.
          </span>
        </p>
      ) : null}

      {allFailed ? (
        <Button variant="outline" size="sm" onClick={() => void load()}>
          Retry
        </Button>
      ) : base == null ? (
        <p className="flex items-center gap-2 text-sm text-dimmer">
          <Loader2Icon className="size-4 animate-spin" /> Loading…
        </p>
      ) : (
        <>
          <div
            className="space-y-2 md:space-y-1.5"
            style={{ ['--cols' as string]: String(cols) }}
          >
            <div aria-hidden className="hidden gap-2 px-0.5 text-[0.6875rem] font-medium text-dimmer md:grid md:grid-cols-[9rem_repeat(var(--cols),minmax(0,1fr))_auto]">
              <span>Label</span>
              {hosts.map((h) => (
                <span key={h} className="truncate">
                  {h}
                </span>
              ))}
              <span className="w-[8.375rem]" />
            </div>
            {rows.length === 0 ? <p className="py-2 text-sm text-dimmer">None — each host offers its home directory.</p> : null}
            {rows.map((r, i) => {
              const errs = validation.errors[r.key]
              const labelId = `sd-${r.key}-label`
              return (
                <div
                  key={r.key}
                  role="group"
                  aria-label={r.label.trim() || `Entry ${i + 1}`}
                  className="grid grid-cols-[minmax(0,1fr)_auto] items-start gap-2 rounded-xl border bg-card p-3 md:grid-cols-[9rem_repeat(var(--cols),minmax(0,1fr))_auto] md:rounded-none md:border-0 md:bg-transparent md:p-0"
                >
                  <div className="col-start-1 row-start-1 min-w-0">
                    <label htmlFor={labelId} className="sr-only">
                      Label
                    </label>
                    <Input
                      id={labelId}
                      value={r.label}
                      maxLength={SPAWN_DIR_LIMITS.maxLabel + 10}
                      placeholder="Label"
                      aria-invalid={errs?.label ? true : undefined}
                      aria-describedby={errs?.label ? `${labelId}-err` : undefined}
                      onChange={(e) => update(r.key, (x) => ({ ...x, label: e.target.value }))}
                      className={cn('h-9 font-medium md:h-8', r.hidden && 'text-muted-foreground italic')}
                    />
                    {errs?.label ? (
                      <p id={`${labelId}-err`} className="pt-1 text-xs text-destructive">
                        {errs.label}
                      </p>
                    ) : null}
                  </div>
                  <div className="col-start-2 row-start-1 flex items-center gap-0.5 md:col-[-2/-1]">
                    <Button
                      variant="ghost"
                      size="icon"
                      aria-label={`Hide ${r.label || 'entry'} from New session`}
                      aria-pressed={r.hidden}
                      title={r.hidden ? 'Hidden: allowed, not offered in New session (click to show)' : 'Offered in New session (click to hide)'}
                      onClick={() => update(r.key, (x) => ({ ...x, hidden: !x.hidden }))}
                      className={cn('size-9 md:size-8', r.hidden ? 'text-status-waiting' : 'text-muted-foreground')}
                    >
                      {r.hidden ? <EyeOffIcon /> : <EyeIcon />}
                    </Button>
                    <Button variant="ghost" size="icon" aria-label={`Move ${r.label || 'entry'} up`} title="Move up" disabled={i === 0} onClick={() => setRows((rs) => moveRow(rs, i, -1))} className="size-9 md:size-8">
                      <ArrowUpIcon />
                    </Button>
                    <Button variant="ghost" size="icon" aria-label={`Move ${r.label || 'entry'} down`} title="Move down" disabled={i === rows.length - 1} onClick={() => setRows((rs) => moveRow(rs, i, 1))} className="size-9 md:size-8">
                      <ArrowDownIcon />
                    </Button>
                    <Button variant="ghost" size="icon" aria-label={`Delete ${r.label || 'entry'}`} title="Delete" onClick={() => setRows((rs) => rs.filter((x) => x.key !== r.key))} className="size-9 text-muted-foreground hover:text-destructive md:size-8">
                      <Trash2Icon />
                    </Button>
                  </div>
                  {hosts.map((h) => {
                    const id = `sd-${r.key}-${h}`
                    const value = r.paths[h] ?? ''
                    const v = value.trim()
                    const syntax = errs?.paths[h]
                    const check = v ? checks[h]?.[v] : undefined
                    const serverErr = v ? serverErrors[h]?.[v] : undefined
                    const unreachable = loads[h]?.state !== 'ok'
                    const problem = syntax ?? (check && !check.isDir ? (check.exists ? 'Not a directory' : 'No such directory') : serverErr)
                    return (
                      <div key={h} className="col-span-2 min-w-0 md:col-span-1 md:row-start-1">
                        <label htmlFor={id} className="block pb-1 text-xs text-muted-foreground md:sr-only">
                          {h}
                        </label>
                        <div className="relative">
                          <Input
                            id={id}
                            value={value}
                            placeholder="not offered"
                            spellCheck={false}
                            autoCapitalize="off"
                            autoCorrect="off"
                            aria-invalid={problem ? true : undefined}
                            aria-describedby={problem ? `${id}-err` : undefined}
                            onChange={(e) => update(r.key, (x) => ({ ...x, paths: { ...x.paths, [h]: e.target.value } }))}
                            className="h-9 pr-8 font-mono text-[0.8125rem] md:h-8"
                          />
                          <CellStatus value={v} problem={problem} check={check} pending={checking} unreachable={unreachable} host={h} />
                        </div>
                        {problem ? (
                          <p id={`${id}-err`} className="pt-1 text-xs text-destructive">
                            {problem}
                          </p>
                        ) : null}
                      </div>
                    )
                  })}
                </div>
              )
            })}
          </div>
          {validation.listError ? <p className="text-xs text-destructive">{validation.listError}</p> : null}

          <div className="flex flex-wrap items-center gap-2 pt-1">
            <Button variant="outline" size="sm" disabled={rows.length >= SPAWN_DIR_LIMITS.maxEntries} onClick={() => setRows((rs) => [...rs, emptyRow()])}>
              <PlusIcon /> Add directory
            </Button>
            <span className="flex-1" />
            {dirty ? <span className="text-xs text-status-waiting">Unsaved changes</span> : null}
            <Button variant="ghost" size="sm" disabled={!dirty || saving} onClick={revert}>
              Revert
            </Button>
            <Button size="sm" disabled={!dirty || saving || !validation.ok || !reachable.length} onClick={() => void save()}>
              {saving ? <Loader2Icon className="animate-spin" /> : null}
              Save{reachable.length > 1 ? ` to ${reachable.length} hosts` : ''}
            </Button>
          </div>

          {results ? (
            <ul aria-label="Save results" className="space-y-1 text-xs">
              {results.map((r) => (
                <li key={r.host} className={cn('flex items-start gap-1.5', r.ok ? 'text-muted-foreground' : 'text-destructive')}>
                  {r.ok ? <CheckIcon className="mt-px size-3.5 shrink-0 text-status-idle" /> : <CircleAlertIcon className="mt-px size-3.5 shrink-0" />}
                  <span className="min-w-0">
                    <span className="font-medium">{r.host}</span> {r.message}
                  </span>
                </li>
              ))}
            </ul>
          ) : null}
        </>
      )}
    </div>
  )
}

/** The ✓ / ✗ inside a path field: that host's own check of the path. */
function CellStatus({
  value,
  problem,
  check,
  pending,
  unreachable,
  host,
}: {
  value: string
  problem?: string
  check?: SpawnDirCheck
  pending: boolean
  unreachable: boolean
  host: string
}) {
  if (!value) return null
  const cls = 'pointer-events-none absolute top-1/2 right-2.5 size-4 -translate-y-1/2'
  if (problem) return <CircleAlertIcon aria-hidden className={cn(cls, 'text-destructive')} />
  if (check?.isDir) return <CheckIcon role="img" aria-label={`exists on ${host}`} className={cn(cls, 'text-status-idle')} />
  if (unreachable) return null
  if (pending) return <Loader2Icon aria-hidden className={cn(cls, 'animate-spin text-dimmer')} />
  return null
}
