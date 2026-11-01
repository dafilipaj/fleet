import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { CircleAlertIcon, Loader2Icon } from 'lucide-react'

import { ApiError, api } from '@/api/client'
import type { HarnessId, HarnessInfo, ModelOption } from '@/api/types'
import { useFleet } from '@/hooks/useFleet'
import { invalidateHarnesses } from '@/hooks/useHarnesses'
import { useSettings } from '@/hooks/useSettings'
import { cn } from '@/lib/utils'

type Load = { state: 'loading' } | { state: 'ok'; rows: HarnessInfo[] } | { state: 'error'; error: string }
type Defaults = { model: string | null; effort: string | null }

const HARNESS_ORDER: HarnessId[] = ['claude', 'codex', 'pi']
const select =
  'h-9 w-full min-w-0 rounded-lg border bg-card px-2 text-sm outline-none focus-visible:ring-2 focus-visible:ring-ring/50 disabled:opacity-60'

/**
 * Settings → Models: the default model and reasoning effort each harness (Claude Code, Codex,
 * pi) starts with when a spawn picks none — web, CLI, bots and scheduled sessions alike. Stored
 * per host (`harnesses.<id>.model` / `.effort`); a change is written to every reachable host so
 * the fleet stays in step. "Harness default" clears it: the harness's own config decides.
 */
export function ModelsEditor() {
  const { fleet } = useFleet()
  const { models: claudeModels } = useSettings()
  const hostsKey = (fleet?.hosts ?? []).map((h) => h.name).join('\n')
  const hosts = useMemo(() => (hostsKey ? hostsKey.split('\n') : []), [hostsKey])
  const self = fleet?.self ?? hosts[0]

  const [loads, setLoads] = useState<Record<string, Load>>({})
  const [saving, setSaving] = useState<HarnessId | null>(null)
  const [results, setResults] = useState<Partial<Record<HarnessId, { ok: string[]; failed: string[] }>>>({})

  const load = useCallback(async () => {
    if (!hosts.length) return
    // No synchronous "loading" state: a host without an entry yet reads as loading.
    const got = await Promise.all(
      hosts.map(async (h): Promise<[string, Load]> => {
        try {
          const r = await api.harnesses(h)
          return [h, { state: 'ok', rows: r.harnesses ?? [] }]
        } catch (err) {
          return [h, { state: 'error', error: (err as Error)?.message || 'unreachable' }]
        }
      }),
    )
    setLoads(Object.fromEntries(got))
  }, [hosts])

  const loadedFor = useRef<string | null>(null)
  useEffect(() => {
    if (!hostsKey || loadedFor.current === hostsKey) return
    loadedFor.current = hostsKey
    void load()
  }, [hostsKey, load])

  const reachable = hosts.filter((h) => loads[h]?.state === 'ok')
  const rowOf = (host: string, id: HarnessId) => {
    const l = loads[host]
    return l?.state === 'ok' ? l.rows.find((r) => r.id === id) : undefined
  }
  // The pickers come from this host's harness list, else the first host that has one installed.
  const reference = (id: HarnessId): HarnessInfo | undefined =>
    [self, ...hosts].map((h) => (h ? rowOf(h, id) : undefined)).find((r) => r?.installed)

  const save = async (id: HarnessId, next: Defaults) => {
    setSaving(id)
    const ok: string[] = []
    const failed: string[] = []
    await Promise.all(
      reachable.map(async (h) => {
        try {
          await api.saveHarnessDefaults(h, { harness: id, model: next.model, effort: next.effort })
          ok.push(h)
        } catch (err) {
          failed.push(`${h}: ${err instanceof ApiError && err.status === 404 ? 'update fleet there' : (err as Error)?.message || 'failed'}`)
        }
      }),
    )
    invalidateHarnesses()
    setResults((r) => ({ ...r, [id]: { ok, failed } }))
    setSaving(null)
    await load()
  }

  if (!hosts.length || hosts.every((h) => loads[h]?.state === 'loading' || !loads[h])) {
    return (
      <div className="flex items-center gap-2 py-4 text-sm text-dimmer">
        <Loader2Icon className="size-4 animate-spin" /> Loading the hosts’ agents…
      </div>
    )
  }

  const unreachable = hosts.filter((h) => loads[h]?.state === 'error')
  const shown = HARNESS_ORDER.filter((id) => reference(id))

  return (
    <div>
      <p className="py-2 text-xs text-dimmer">
        What a new session starts with when you pick no model: from the web, the CLI, bots and scheduled sessions. Saved on every host.
      </p>
      {shown.map((id) => {
        const ref = reference(id)!
        const models: ModelOption[] = (id === 'claude' ? claudeModels : ref.models).filter((m) => m.id !== '')
        const efforts = ref.efforts ?? []
        const cur: Defaults = { model: ref.defaultModel ?? null, effort: ref.defaultEffort ?? null }
        // Hosts whose stored defaults differ from the one shown (an older value, a host that was down).
        const differ = reachable.filter((h) => {
          const r = rowOf(h, id)
          return r && ((r.defaultModel ?? null) !== cur.model || (r.defaultEffort ?? null) !== cur.effort)
        })
        const busy = saving === id
        const result = results[id]
        // A stored model this host's list does not offer still shows (and can be cleared).
        const modelOptions = cur.model && !models.some((m) => m.id === cur.model) ? [{ id: cur.model, label: cur.model }, ...models] : models
        return (
          <div key={id} className="space-y-2 py-3">
            <div className="flex items-center gap-2 text-sm font-medium">
              {ref.label}
              {busy ? <Loader2Icon className="size-3.5 animate-spin text-dimmer" /> : null}
            </div>
            <div className="grid grid-cols-1 gap-2 sm:grid-cols-[1fr_10rem]">
              <label className="min-w-0 space-y-1">
                <span className="text-xs text-dimmer">Model</span>
                <select
                  aria-label={`${ref.label} default model`}
                  className={select}
                  value={cur.model ?? ''}
                  disabled={busy}
                  onChange={(e) => void save(id, { ...cur, model: e.target.value || null })}
                >
                  <option value="">Harness default</option>
                  {modelOptions.map((m) => (
                    <option key={m.id} value={m.id}>
                      {m.label}
                    </option>
                  ))}
                </select>
              </label>
              <label className="min-w-0 space-y-1">
                <span className="text-xs text-dimmer">Effort</span>
                <select
                  aria-label={`${ref.label} default effort`}
                  className={select}
                  value={cur.effort ?? ''}
                  disabled={busy || !efforts.length}
                  onChange={(e) => void save(id, { ...cur, effort: e.target.value || null })}
                >
                  <option value="">Harness default</option>
                  {(cur.effort && !efforts.includes(cur.effort) ? [cur.effort, ...efforts] : efforts).map((l) => (
                    <option key={l} value={l}>
                      {l}
                    </option>
                  ))}
                </select>
              </label>
            </div>
            {differ.length ? (
              <p className="flex items-center gap-1.5 text-xs text-status-waiting">
                <CircleAlertIcon className="size-3.5 shrink-0" /> Different on {differ.join(', ')}: saving sets every host
              </p>
            ) : null}
            {result ? (
              <p className={cn('text-xs', result.failed.length ? 'text-status-error' : 'text-dimmer')}>
                {result.ok.length ? `Saved on ${result.ok.join(', ')}` : null}
                {result.ok.length && result.failed.length ? ' · ' : null}
                {result.failed.length ? `Not saved: ${result.failed.join('; ')}` : null}
              </p>
            ) : null}
          </div>
        )
      })}
      {unreachable.length ? (
        <p className="py-2 text-xs text-status-error">
          Unreachable, not updated: {unreachable.join(', ')}. Save again once they are back.
        </p>
      ) : null}
    </div>
  )
}
