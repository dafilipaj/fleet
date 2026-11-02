// Pure display helpers (no React, no DOM) — unit-tested in format.test.ts.
import type { ContextUsage, Session } from '@/api/types'

/** "12s", "5m", "3h", "4d" since `ms` (epoch ms); '' for missing/invalid. */
export function relTime(ms: number | null | undefined, now = Date.now()): string {
  const t = Number(ms)
  if (!Number.isFinite(t) || t <= 0) return ''
  const s = Math.max(0, Math.round((now - t) / 1000))
  if (s < 60) return `${s}s`
  const m = Math.round(s / 60)
  if (m < 60) return `${m}m`
  const hr = Math.round(m / 60)
  if (hr < 48) return `${hr}h`
  return `${Math.round(hr / 24)}d`
}

const pad2 = (n: number) => String(n).padStart(2, '0')

/** "14:03" today, else "3.7. 14:03". */
export function clockTime(ms: number | null | undefined, now = Date.now()): string {
  const t = Number(ms)
  if (!Number.isFinite(t) || t <= 0) return ''
  const d = new Date(t)
  const today = new Date(now)
  const hhmm = `${pad2(d.getHours())}:${pad2(d.getMinutes())}`
  const sameDay =
    d.getFullYear() === today.getFullYear() && d.getMonth() === today.getMonth() && d.getDate() === today.getDate()
  return sameDay ? hhmm : `${d.getDate()}.${d.getMonth() + 1}. ${hhmm}`
}

/** Home-dir prefix → `~`, head-truncated to `max` chars (the tail of a path is the informative part). */
export function shortCwd(cwd: string | null | undefined, max = 46): string {
  const p = String(cwd || '').replace(/^\/(?:Users|home)\/[^/]+/, '~')
  return p.length <= max ? p : `…${p.slice(-(max - 1))}`
}

export function firstLine(text: string | null | undefined): string {
  if (typeof text !== 'string') return ''
  for (const line of text.split('\n')) {
    const t = line.trim()
    if (t) return t
  }
  return ''
}

/**
 * The one-line description under a session's title: its first prompt. The title itself
 * (`sessionTitle`, lib/title.ts) already carries the generated title, so it is not repeated.
 */
export function sessionSubtitle(s: Pick<Session, 'title'>): string {
  const line = firstLine(s.title)
  return line.length > 240 ? `${line.slice(0, 240)}…` : line
}

export const HOST_COLOR_SLOTS = 4

// The configured hosts as the last /api/fleet listed them (FleetProvider keeps it current), sorted
// by name so every server's UI agrees: their order gives each one its own colour while there are no more hosts than slots, and `shared`
// marks a shared shed wherever a host name is drawn.
let hostOrder: string[] = []
let sharedHosts = new Set<string>()

export function setKnownHosts(hosts: { name: string; shared?: boolean }[] | null | undefined): void {
  hostOrder = (hosts ?? []).map((h) => h.name).sort()
  sharedHosts = new Set((hosts ?? []).filter((h) => h.shared === true).map((h) => h.name))
}

/** Whether the fleet marks `host` as a shared shed (`hosts.<name>.shared`). */
export function isSharedHost(host: string | null | undefined): boolean {
  return host != null && sharedHosts.has(host)
}

/**
 * Colour slot 0..3 per host: by its place among the configured hosts while they fit the slots
 * (no two alike), else a stable hash of the name.
 */
export function hostColorSlot(host: string | null | undefined): number {
  const i = host != null && hostOrder.length <= HOST_COLOR_SLOTS ? hostOrder.indexOf(host) : -1
  if (i >= 0) return i
  let hash = 0
  for (const ch of String(host || '')) hash = (hash * 31 + (ch.codePointAt(0) ?? 0)) >>> 0
  return hash % HOST_COLOR_SLOTS
}

export type CtxLevel = 'low' | 'warn' | 'hot'

/** Same bands as the CLI: <60 calm, 60–85 warn, >85 hot. */
export function ctxLevel(pct: number): CtxLevel {
  if (pct > 85) return 'hot'
  if (pct >= 60) return 'warn'
  return 'low'
}

/** 950 → "950", 124_300 → "124k", 1_000_000 → "1M", 1_250_000 → "1.3M". */
export function fmtTokens(n: number): string {
  if (!Number.isFinite(n) || n < 0) return '?'
  if (n < 1000) return String(Math.round(n))
  if (n < 999_500) return `${Math.round(n / 1000)}k`
  const m = n / 1_000_000
  return `${Number.isInteger(Math.round(m * 10) / 10) ? Math.round(m) : m.toFixed(1)}M`
}

/** "124k / 200k · 62%" — '' for a missing or malformed context. */
/** Short human model name: `claude-opus-5-5` → `Opus 5.5`, `claude-haiku-4-5-20251001` → `Haiku 4.5`, `…[1m]` → `… 1M`. */
export function modelLabel(id: string | null | undefined): string {
  if (!id) return ''
  const oneM = /\[1m\]$/i.test(id)
  const m = id.replace(/\[1m\]$/i, '').match(/^claude-([a-z]+)-(\d+)(?:-(\d{1,2}))?(?:-\d{8})?$/i)
  if (!m) return id
  const family = m[1].charAt(0).toUpperCase() + m[1].slice(1)
  return `${family} ${m[2]}${m[3] ? `.${m[3]}` : ''}${oneM ? ' 1M' : ''}`
}

export function ctxSummary(c: ContextUsage | null | undefined): string {
  if (!c || !Number.isFinite(c.used) || !Number.isFinite(c.window)) return ''
  return `${fmtTokens(c.used)} / ${fmtTokens(c.window)} · ${Math.round(c.pct)}%`
}
