import type { LinkKind } from '@/lib/sessions'

/** The link kinds in the order Details → Links shows them. */
export const LINK_KINDS: readonly LinkKind[] = ['slack', 'github', 'productive', 'veranda']

export const LINK_LABEL: Record<LinkKind, string> = { slack: 'Slack', github: 'GitHub', productive: 'Productive', veranda: 'Veranda' }

/** As the bridges check them (web/lib/github.mjs `parsePrUrl`, productive.mjs `parseTaskRef`, veranda.mjs `validRoom`). */
const PR_URL_RE = /^https?:\/\/github\.com\/[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+\/pull\/\d+(?:[/?#].*)?$/
const PR_SHORT_RE = /^[A-Za-z0-9_.-]+\/[A-Za-z0-9_.-]+#\d+$/
const TASK_URL_RE = /^https?:\/\/app\.productive\.io\/[^/]+\/(?:[^/]+\/)*tasks?\/\d+(?:[/?#].*)?$/
const SLACK_URL_RE = /^https?:\/\/[a-z0-9-]+\.slack\.com\//i
export const ROOM_RE = /^[a-z0-9](?:[a-z0-9-]{0,46}[a-z0-9])?$/

/**
 * What a pasted link points at, among the kinds this session can be linked to: a Slack thread or
 * channel (or `@person`, a DM), a GitHub pull request, a Productive task, or a Veranda room name.
 * Null when it is none of them (or that kind is not available here).
 */
export function detectLinkKind(input: string, available: readonly LinkKind[]): LinkKind | null {
  const s = input.trim()
  if (!s) return null
  const kind: LinkKind | null =
    PR_URL_RE.test(s) || PR_SHORT_RE.test(s)
      ? 'github'
      : TASK_URL_RE.test(s)
        ? 'productive'
        : SLACK_URL_RE.test(s) || /^@\S/.test(s)
          ? 'slack'
          : ROOM_RE.test(s)
            ? 'veranda'
            : null
  return kind && available.includes(kind) ? kind : null
}

/** "Sleep after idle" for GitHub and Productive links (the bridges' `keep`, in minutes; null: never). */
export const KEEP_OPTIONS: { label: string; value: string | null }[] = [
  { label: 'Never', value: null },
  { label: '12h', value: '12h' },
  { label: '1d', value: '1d' },
  { label: '1w', value: '7d' },
]

/** A `keep` in minutes → the option it matches ('1d' for 1440), or the minutes as a duration. */
export function keepValue(minutes: number | null): string | null {
  if (minutes == null) return null
  const known: Record<number, string> = { 720: '12h', 1440: '1d', 10080: '7d' }
  return known[minutes] ?? `${minutes}m`
}

export function keepLabel(minutes: number | null): string {
  if (minutes == null) return 'never sleeps'
  if (minutes % 1440 === 0) return `sleeps after ${minutes / 1440}d idle`
  if (minutes % 60 === 0) return `sleeps after ${minutes / 60}h idle`
  return `sleeps after ${minutes}m idle`
}
