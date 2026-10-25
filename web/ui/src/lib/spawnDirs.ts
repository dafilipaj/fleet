// Settings → Start directories: the editable draft of the shared `spawnDirs` list (one
// row per label, a directory per host) and its pure helpers. The server re-validates
// everything (web/lib/spawn-dirs.mjs); these give instant feedback and the diff/dirty state.
import type { FleetResponse, SpawnDirCheck, SpawnDirEntry, SpawnDirsError, SpawnDirsResponse } from '@/api/types'

export const SPAWN_DIR_LIMITS = { maxEntries: 30, maxLabel: 40, maxPath: 1024 } as const

export interface DraftRow {
  /** Stable React key (survives renames and reorders). */
  key: string
  label: string
  /** host → path as typed ('' = not offered there). Hosts without a column are kept as-is. */
  paths: Record<string, string>
  /** Allowed for spawns but not offered in the New session menu. */
  hidden: boolean
  /** The entry's keys this UI does not know, sent back unchanged. */
  extra: Record<string, unknown>
}

/** Per row key: the label's problem and each host cell's problem. */
export type DraftErrors = Record<string, { label?: string; paths: Record<string, string> }>

let seq = 0
export const newRowKey = () => `r${++seq}`

export function toDraft(entries: SpawnDirEntry[] | null | undefined): DraftRow[] {
  return (entries ?? []).map((e) => {
    const { label, paths, hidden, ...extra } = e
    return { key: newRowKey(), label, paths: { ...paths }, hidden: hidden === true, extra }
  })
}

export function emptyRow(): DraftRow {
  return { key: newRowKey(), label: '', paths: {}, hidden: false, extra: {} }
}

/** The list to PUT: trimmed labels and paths, empty paths dropped, `hidden` only when set, other keys kept. */
export function fromDraft(rows: DraftRow[]): SpawnDirEntry[] {
  return rows.map((r) => {
    const paths: Record<string, string> = {}
    for (const [h, p] of Object.entries(r.paths)) if (p.trim()) paths[h] = p.trim()
    return { ...r.extra, label: r.label.trim(), paths, ...(r.hidden ? { hidden: true } : {}) }
  })
}

/** Same entries in the same order (host order inside `paths` does not matter). */
export function sameList(a: SpawnDirEntry[], b: SpawnDirEntry[]): boolean {
  if (a.length !== b.length) return false
  return a.every((e, i) => {
    const o = b[i]
    if (e.label.trim() !== o.label.trim()) return false
    if ((e.hidden === true) !== (o.hidden === true)) return false
    const ka = Object.keys(e.paths).filter((h) => e.paths[h]?.trim())
    const kb = Object.keys(o.paths).filter((h) => o.paths[h]?.trim())
    return ka.length === kb.length && ka.every((h) => e.paths[h].trim() === (o.paths[h] ?? '').trim())
  })
}

/** A control character (tab, newline, NUL, DEL…) anywhere in `s`. */
const hasControl = (s: string) => [...s].some((c) => c.charCodeAt(0) < 32 || c.charCodeAt(0) === 127)

/** A path's syntax problem, or null. `~`-relative or absolute; the server checks it exists. */
export function pathProblem(p: string): string | null {
  const v = p.trim()
  if (!v) return null
  if (v.length > SPAWN_DIR_LIMITS.maxPath) return `Longer than ${SPAWN_DIR_LIMITS.maxPath} characters`
  if (hasControl(v)) return 'Contains a control character'
  if (!(v === '~' || v.startsWith('~/') || v.startsWith('/'))) return 'Must start with / or ~/'
  return null
}

/** Client-side checks, mirroring the server: labels required, ≤ 40, unique (case-insensitive); path syntax; ≥ 1 path. */
export function validateDraft(rows: DraftRow[]): { ok: boolean; errors: DraftErrors; listError: string | null } {
  const errors: DraftErrors = {}
  let ok = true
  const seen = new Set<string>()
  for (const r of rows) {
    const e: DraftErrors[string] = { paths: {} }
    const label = r.label.trim()
    if (!label) e.label = 'Give it a name'
    else if (label.length > SPAWN_DIR_LIMITS.maxLabel) e.label = `At most ${SPAWN_DIR_LIMITS.maxLabel} characters`
    else if (hasControl(label)) e.label = 'One line of text'
    else if (seen.has(label.toLowerCase())) e.label = 'Another entry has this name'
    if (label) seen.add(label.toLowerCase())
    let any = false
    for (const [h, p] of Object.entries(r.paths)) {
      if (p.trim()) any = true
      const bad = pathProblem(p)
      if (bad) e.paths[h] = bad
    }
    if (!any && !e.label) e.label = 'Set a directory for at least one host'
    if (e.label || Object.keys(e.paths).length) ok = false
    errors[r.key] = e
  }
  const listError = rows.length > SPAWN_DIR_LIMITS.maxEntries ? `At most ${SPAWN_DIR_LIMITS.maxEntries} directories` : null
  return { ok: ok && !listError, errors, listError }
}

/** Move row `i` by `delta` (−1 up, +1 down); out-of-range moves return the same array. */
export function moveRow<T>(rows: T[], i: number, delta: number): T[] {
  const j = i + delta
  if (i < 0 || i >= rows.length || j < 0 || j >= rows.length) return rows
  const next = rows.slice()
  ;[next[i], next[j]] = [next[j], next[i]]
  return next
}

/**
 * A host's 400 as path → message for its own column (labels are checked client-side).
 * Keyed by the path as sent, so it survives edits to other rows and reorders.
 */
export function hostPathErrors(err: SpawnDirsError | null | undefined, sent: SpawnDirEntry[], host: string): Record<string, string> {
  const out: Record<string, string> = {}
  for (const e of err?.errors ?? []) {
    if (e.field !== 'paths' || e.host !== host) continue
    const p = sent[e.index]?.paths[host]
    if (p && !out[p]) out[p] = e.error
  }
  return out
}

/** A host's checks (GET, dry run, save) as path → check, for its own column. */
export function checksByPath(r: { spawnDirs?: SpawnDirEntry[]; checks?: (SpawnDirCheck | null)[] }, sent: SpawnDirEntry[] | undefined, host: string): Record<string, SpawnDirCheck> {
  const out: Record<string, SpawnDirCheck> = {}
  const list = sent ?? r.spawnDirs ?? []
  ;(r.checks ?? []).forEach((c, i) => {
    const p = list[i]?.paths[host]
    if (c && p) out[p] = c
  })
  return out
}

/** The list the editor starts from: this server's host when it answered, else the first that did. */
export function baseHost(self: string | undefined, loaded: Record<string, SpawnDirsResponse | undefined>, order: string[]): string | null {
  if (self && loaded[self]) return self
  return order.find((h) => loaded[h]) ?? null
}

/** Hosts whose stored list differs from `base` (the shared-list model expects none). */
export function divergedHosts(base: SpawnDirEntry[], loaded: Record<string, SpawnDirsResponse | undefined>): string[] {
  return Object.entries(loaded)
    .filter(([, r]) => r && !sameList(r.spawnDirs, base))
    .map(([h]) => h)
}

/**
 * The fleet with each saved host's `spawnDirs` replaced by what it now offers (the New session
 * form reads it) and its `hiddenSpawnDirs` by its hidden ones (the embed view allows them).
 */
export function withOffered(fleet: FleetResponse, saved: Record<string, SpawnDirsResponse>): FleetResponse {
  return {
    ...fleet,
    hosts: fleet.hosts.map((h) => {
      const r = saved[h.name]
      if (!r) return h
      const { hiddenSpawnDirs: _old, ...rest } = h
      const hidden = r.hiddenSpawnDirs ?? []
      return { ...rest, spawnDirs: r.offered, ...(hidden.length ? { hiddenSpawnDirs: hidden } : {}) }
    }),
  }
}
