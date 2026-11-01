import type { HarnessInfo, ModelOption, Session } from '@/api/types'
import { normalizeModels } from '@/lib/models'

/** Short names for chips and copy. Unknown ids show as themselves. */
export const HARNESS_LABELS: Record<string, string> = { claude: 'Claude', codex: 'Codex', pi: 'pi' }

export function harnessLabel(id: string | null | undefined): string {
  const h = id || 'claude'
  return HARNESS_LABELS[h] ?? h
}

/** A session's harness; rows from older CLIs carry none and are Claude's. */
export function sessionHarness(s: Pick<Session, 'harness'>): string {
  return s.harness || 'claude'
}

/** What a host offers before (or without) GET …/harnesses: Claude alone, with the configured models. */
export function claudeOnly(models: ModelOption[]): HarnessInfo[] {
  return [{ id: 'claude', label: 'Claude Code', command: 'claude', installed: true, models }]
}

/**
 * The harnesses the New session form offers: installed ones with a usable model list (a
 * harness the host reports without models still gets its default). Claude is always first.
 */
export function offeredHarnesses(rows: HarnessInfo[] | null | undefined, claudeModels: ModelOption[]): HarnessInfo[] {
  const list = (rows ?? []).filter((h) => h && typeof h.id === 'string' && h.installed)
  const out = list.map((h) => {
    const models = h.id === 'claude' ? claudeModels : h.models?.length ? normalizeModels(h.models) : [{ id: '', label: 'Default' }]
    return { ...h, models: withFleetDefault(models, h) }
  })
  if (!out.some((h) => h.id === 'claude')) out.unshift(...claudeOnly(claudeModels))
  return out.sort((a, b) => Number(b.id === 'claude') - Number(a.id === 'claude'))
}

/** "Opus 5.5 · high": the host's fleet default for a harness (Settings → Models), or null when none. */
export function fleetDefaultLabel(h: Pick<HarnessInfo, 'defaultModel' | 'defaultEffort'>, models: ModelOption[]): string | null {
  const model = h.defaultModel ? (models.find((m) => m.id === h.defaultModel)?.label ?? h.defaultModel) : null
  const parts = [model, h.defaultEffort].filter(Boolean)
  return parts.length ? parts.join(' · ') : null
}

/** The picker's default entry (`id: ''`) names the fleet default it starts, when one is set. */
export function withFleetDefault(models: ModelOption[], h: Pick<HarnessInfo, 'defaultModel' | 'defaultEffort'>): ModelOption[] {
  const label = fleetDefaultLabel(h, models)
  return label ? models.map((m) => (m.id === '' ? { ...m, label: `Default · ${label}` } : m)) : models
}

/** The remembered harness when still offered, else Claude. */
export function pickHarness(offered: HarnessInfo[], remembered: string | null): string {
  if (remembered && offered.some((h) => h.id === remembered)) return remembered
  return offered[0]?.id ?? 'claude'
}

/** Where the picked model is remembered: Claude keeps the old key. */
export function modelKey(harness: string): string {
  return harness === 'claude' ? 'fleet.spawnModel' : `fleet.spawnModel.${harness}`
}
