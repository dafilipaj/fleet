import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import { Loader2Icon } from 'lucide-react'

import { ApiError, api } from '@/api/client'
import type { StackSettings } from '@/api/types'
import { Button } from '@/components/ui/button'
import { Switch } from '@/components/ui/switch'
import { Textarea } from '@/components/ui/textarea'
import { useFleet } from '@/hooks/useFleet'

type Load = { state: 'ok'; settings: StackSettings } | { state: 'error'; error: string }
type Patch = Partial<Pick<StackSettings, 'leadPrompt' | 'siblingReports' | 'reportPrompt'>>

const PLACEHOLDERS = '{label} {id} {context} {fleet} {lead} {leadName} {leadHost} {home}'

/**
 * Settings → Session stacks: what a session is told when it becomes a stack's lead, and whether
 * (and how) siblings spawned into a stack with a lead are told to report to it. `stacks.*` in
 * each host's config (a stack's home host uses its own): shown from this host, saved on every
 * reachable host, like Models.
 */
export function StackSettingsEditor() {
  const { fleet } = useFleet()
  const hostsKey = (fleet?.hosts ?? []).map((h) => h.name).join('\n')
  const hosts = useMemo(() => (hostsKey ? hostsKey.split('\n') : []), [hostsKey])
  const self = fleet?.self ?? hosts[0]
  const [loads, setLoads] = useState<Record<string, Load>>({})
  const [saving, setSaving] = useState(false)
  const [result, setResult] = useState<{ ok: string[]; failed: string[] } | null>(null)

  const load = useCallback(async () => {
    if (!hosts.length) return
    const got = await Promise.all(
      hosts.map(async (h): Promise<[string, Load]> => {
        try {
          return [h, { state: 'ok', settings: await api.stackSettings(h) }]
        } catch (err) {
          return [h, { state: 'error', error: err instanceof ApiError && err.status === 404 ? 'update fleet there' : (err as Error)?.message || 'unreachable' }]
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
  const of = (h: string | undefined) => {
    const l = h ? loads[h] : undefined
    return l?.state === 'ok' ? l.settings : undefined
  }
  const cur = of(self) ?? reachable.map(of).find(Boolean)

  const save = async (patch: Patch) => {
    setSaving(true)
    const ok: string[] = []
    const failed: string[] = []
    await Promise.all(
      reachable.map(async (h) => {
        try {
          await api.setStackSettings(h, patch)
          ok.push(h)
        } catch (err) {
          failed.push(`${h}: ${err instanceof ApiError && err.status === 404 ? 'update fleet there' : (err as Error)?.message || 'failed'}`)
        }
      }),
    )
    setResult({ ok, failed })
    setSaving(false)
    await load()
  }

  if (!cur) {
    const errors = hosts.map((h) => loads[h]).filter((l) => l?.state === 'error')
    return (
      <div className="flex items-center gap-2 py-4 text-sm text-dimmer">
        {hosts.length && errors.length === hosts.length ? (
          'No host answered — update fleet on them.'
        ) : (
          <>
            <Loader2Icon className="size-4 animate-spin" /> Loading…
          </>
        )}
      </div>
    )
  }

  const unreachable = hosts.filter((h) => loads[h]?.state === 'error')
  const differ = reachable.filter((h) => {
    const s = of(h)
    return s && (s.leadPrompt !== cur.leadPrompt || s.siblingReports !== cur.siblingReports || s.reportPrompt !== cur.reportPrompt)
  })

  const status = [
    saving ? 'Saving…' : result?.failed.length ? `Not saved on ${result.failed.join('; ')}.` : differ.length ? `Different on ${differ.join(', ')}: save here to align them.` : null,
    unreachable.length ? `Unreachable: ${unreachable.join(', ')}.` : null,
  ]
    .filter(Boolean)
    .join(' ')

  return (
    <div>
      <p className="py-2 text-xs text-dimmer">
        A stack’s lead (orchestrator) goes first in its group. Make one from the StackBrief sheet (the crown next to a session) or with{' '}
        <code className="font-mono">fleet stack lead</code>. Saved on every host.
      </p>
      <PromptEditor
        key={`lead:${cur.leadPrompt ?? ''}`}
        label="Lead prompt"
        hint="Sent to a session when it becomes the stack’s lead"
        value={cur.leadPrompt}
        fallback={cur.leadPromptDefault}
        disabled={saving}
        onSave={(v) => save({ leadPrompt: v })}
      />
      <div className="flex items-center justify-between gap-4 py-3">
        <div className="min-w-0">
          <div className="text-sm">Siblings report to the lead</div>
          <div className="text-xs text-dimmer">A sibling spawned into a stack with a lead is told to send it its progress</div>
        </div>
        <Switch checked={cur.siblingReports} disabled={saving} onCheckedChange={(v) => void save({ siblingReports: v })} aria-label="Siblings report to the lead" />
      </div>
      {cur.siblingReports ? (
        <PromptEditor
          key={`report:${cur.reportPrompt ?? ''}`}
          label="Report instruction"
          hint="Added after a sibling’s context line, before its prompt"
          value={cur.reportPrompt}
          fallback={cur.reportPromptDefault}
          disabled={saving}
          onSave={(v) => save({ reportPrompt: v })}
        />
      ) : null}
      <p className="py-2 text-xs text-dimmer">
        Placeholders: <code className="font-mono">{PLACEHOLDERS}</code>
      </p>
      {status ? <p className="py-1 text-xs text-dimmer">{status}</p> : null}
    </div>
  )
}

/** A prompt with a default: the stored text (or the default), Save / Cancel / Reset to default. */
function PromptEditor({
  label,
  hint,
  value,
  fallback,
  disabled,
  onSave,
}: {
  label: string
  hint: string
  value: string | null
  fallback: string
  disabled: boolean
  onSave: (v: string | null) => Promise<void>
}) {
  const [draft, setDraft] = useState(value ?? fallback)
  const dirty = draft.trim() !== (value ?? fallback).trim()
  return (
    <div className="space-y-2 py-3">
      <div>
        <div className="text-sm">{label}</div>
        <div className="text-xs text-dimmer">
          {hint} · {value ? 'your own text' : 'the default'}
        </div>
      </div>
      <Textarea value={draft} onChange={(e) => setDraft(e.target.value)} disabled={disabled} className="max-h-96 min-h-28 text-[0.8125rem]" aria-label={label} />
      <div className="flex gap-2">
        <Button size="sm" disabled={disabled || !dirty} onClick={() => void onSave(draft.trim() === fallback.trim() ? null : draft)}>
          Save
        </Button>
        {dirty ? (
          <Button size="sm" variant="ghost" disabled={disabled} onClick={() => setDraft(value ?? fallback)}>
            Cancel
          </Button>
        ) : null}
        {value ? (
          <Button size="sm" variant="ghost" disabled={disabled} onClick={() => void onSave(null)}>
            Reset to default
          </Button>
        ) : null}
      </div>
    </div>
  )
}
