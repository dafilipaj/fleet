import type { UsageAccount, UsageLimit, UsageRead, UsageResponse } from '@/api/types'
import type { CtxLevel } from '@/lib/format'

/** One host's answer: its usage, or why there is none. */
export interface HostUsage {
  host: string
  usage: UsageResponse | null
  error: string | null
}

export type Provider = 'claude' | 'openai'

export const PROVIDER_NAME: Record<Provider, string> = { claude: 'Claude', openai: 'ChatGPT' }

/** One subscription account, with every host logged in to it (the numbers are the account's). */
export interface AccountUsage {
  key: string
  provider: Provider
  account: UsageAccount | null
  hosts: string[]
  /** The freshest read among those hosts. */
  usage: UsageRead
}

/** A host's reads, one per provider: Claude's, then the ChatGPT one (if Codex is logged in). */
export function hostReads(u: UsageResponse): { provider: Provider; read: UsageRead }[] {
  const out: { provider: Provider; read: UsageRead }[] = [{ provider: 'claude', read: u }]
  if (u.openai) out.push({ provider: 'openai', read: u.openai })
  return out
}

/** A read with nothing to show (its provider's login failed and nothing is cached). */
const failed = (r: UsageRead) => !r.limits.length && !!r.error && !r.stale

/** Per-host errors: unreachable hosts, and a provider whose read failed outright. */
export function usageErrors(results: HostUsage[]): { key: string; host: string; error: string }[] {
  const out: { key: string; host: string; error: string }[] = []
  for (const r of results) {
    if (r.error) out.push({ key: r.host, host: r.host, error: r.error })
    if (!r.usage) continue
    for (const { provider, read } of hostReads(r.usage)) {
      if (failed(read)) out.push({ key: `${r.host}:${provider}`, host: r.host, error: `${PROVIDER_NAME[provider]}: ${read.error}` })
    }
  }
  return out
}

/** Band for a limit: the endpoint's severity wins, else ≥90% hot, ≥70% warn. */
export function usageLevel(l: Pick<UsageLimit, 'percent' | 'severity'>): CtxLevel {
  if (l.percent >= 90 || ['critical', 'exceeded', 'blocked', 'limited'].includes(l.severity)) return 'hot'
  if (l.percent >= 70 || l.severity === 'warning') return 'warn'
  return 'low'
}

/** `in 45m` / `in 2h 20m` within a day, else `Fri 14:00` (local time). */
export function resetsLabel(iso: string | null, now: number): string | null {
  if (!iso) return null
  const t = Date.parse(iso)
  if (!Number.isFinite(t)) return null
  const mins = Math.max(0, Math.round((t - now) / 60_000))
  if (mins < 60) return `in ${mins}m`
  if (mins < 24 * 60) return `in ${Math.floor(mins / 60)}h ${mins % 60}m`
  const d = new Date(t)
  const day = d.toLocaleDateString(undefined, { weekday: 'short' })
  const time = d.toLocaleTimeString(undefined, { hour: '2-digit', minute: '2-digit', hour12: false })
  return `${day} ${time}`
}

/** `12.50 USD` style money, or null. */
export function money(n: number | null, currency: string | null): string | null {
  if (n == null || !Number.isFinite(n)) return null
  try {
    return currency ? new Intl.NumberFormat(undefined, { style: 'currency', currency }).format(n) : n.toFixed(2)
  } catch {
    return `${n.toFixed(2)} ${currency ?? ''}`.trim()
  }
}

/**
 * Group hosts by the account they're logged in to (provider + uuid, else email): two machines
 * on one subscription show one set of bars. Claude accounts come first, then ChatGPT ones;
 * within a provider, accounts keep the order their first host appears in.
 */
export function groupByAccount(results: HostUsage[]): AccountUsage[] {
  const out: AccountUsage[] = []
  for (const want of ['claude', 'openai'] as const) {
    for (const r of results) {
      if (r.usage) for (const { provider, read } of hostReads(r.usage)) if (provider === want && !failed(read)) add(r.host, provider, read)
    }
  }
  return out

  function add(host: string, provider: Provider, u: UsageRead) {
    const key = `${provider}:${u.account?.uuid ?? u.account?.email ?? `host:${host}`}`
    const existing = out.find((a) => a.key === key)
    if (!existing) {
      out.push({ key, provider, account: u.account, hosts: [host], usage: u })
      return
    }
    existing.hosts.push(host)
    const fresher = (!u.stale && existing.usage.stale) || (u.stale === existing.usage.stale && Date.parse(u.fetched_at) > Date.parse(existing.usage.fetched_at))
    if (fresher) existing.usage = u
  }
}
