// Pure helpers for the inbox screen (no React, no DOM) — unit-tested in inbox.test.ts.
import type { InboxItem, InboxResponse, InboxSection, InboxSource } from '@/api/types'
import { clockTime, relTime } from '@/lib/format'

export const INBOX_SECTIONS: { id: InboxSection; title: string }[] = [
  { id: 'quick', title: 'Quick wins' },
  { id: 'waiting', title: 'Waiting on you' },
  { id: 'mine', title: 'Your work' },
]

/** How long an item can carry the "new" dot. */
export const NEW_MS = 60 * 60 * 1000

/** The items by section, in section order (the server's order kept within each); empty sections dropped. */
export function inboxSections(items: InboxItem[]): { id: InboxSection; title: string; items: InboxItem[] }[] {
  return INBOX_SECTIONS.map((s) => ({ ...s, items: items.filter((i) => i.section === s.id) })).filter((s) => s.items.length)
}

/** First seen after the viewer's previous visit, and less than an hour ago. */
export function isNewItem(item: Pick<InboxItem, 'firstSeen'>, lastVisit: number, now = Date.now()): boolean {
  return item.firstSeen > lastVisit && now - item.firstSeen < NEW_MS
}

/** Where a row goes: the in-app route first, else the external link. */
export function itemTarget(item: Pick<InboxItem, 'href' | 'url'>): { href: string; external: boolean } | null {
  if (item.href) return { href: item.href, external: false }
  if (item.url) return { href: item.url, external: true }
  return null
}

const SOURCE_LABELS: Record<string, string> = { github: 'GitHub', productive: 'Productive', sessions: 'Sessions', slack: 'Slack', comms: 'Slack & email' }

export interface SourceLine {
  key: string
  label: string
  text: string
  warn: boolean
}

/** One short status per source: "GitHub 2m", "Slack & email scanning…", "Productive — error". */
export function sourceLines(sources: InboxResponse['sources'] | undefined, now = Date.now()): SourceLine[] {
  if (!sources) return []
  return (Object.entries(sources) as [string, InboxSource | undefined][])
    .filter((e): e is [string, InboxSource] => e[1] != null)
    .map(([key, s]) => {
      // With the Slack poller the connector scan reads mail only.
      const label = key === 'comms' && sources.slack ? 'Email' : (SOURCE_LABELS[key] ?? key)
      if (s.running && key !== 'slack') return { key, label, text: 'scanning…', warn: false }
      if (!s.ok || s.error) return { key, label, text: s.error || 'failed', warn: true }
      const ago = s.at ? relTime(s.at, now) : ''
      const next = s.nextAt && s.nextAt > now ? ` · next ${clockTime(s.nextAt, now)}` : ''
      return { key, label, text: `${ago ? `${ago} ago` : 'not read yet'}${next}`, warn: false }
    })
}

/** Days until a `YYYY-MM-DD` date (negative: past), local time. */
function daysUntil(day: string, now: number): number | null {
  const m = /^(\d{4})-(\d{2})-(\d{2})$/.exec(day)
  if (!m) return null
  const due = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3])).getTime()
  const d = new Date(now)
  const today = new Date(d.getFullYear(), d.getMonth(), d.getDate()).getTime()
  return Math.round((due - today) / 86_400_000)
}

/** "overdue 2d" / "due today" / "due tomorrow" / "due in 3d"; tone for colouring. */
export function dueChip(meta: InboxItem['meta'], now = Date.now()): { label: string; overdue: boolean } | null {
  if (!meta.due) return null
  const n = daysUntil(meta.due, now)
  if (n == null) return { label: `due ${meta.due}`, overdue: Boolean(meta.overdue) }
  if (n < 0 || meta.overdue) return { label: n < 0 ? `overdue ${-n}d` : 'overdue', overdue: true }
  if (n === 0) return { label: 'due today', overdue: false }
  if (n === 1) return { label: 'due tomorrow', overdue: false }
  return { label: `due in ${n}d`, overdue: false }
}

/** The brief's in-app links (`](#/s/…)`) as absolute URLs, the only kind Markdown links; it opens them in place. */
export function briefMarkdown(text: string, origin: string): string {
  return text.replace(/\]\(#\//g, `](${origin}/#/`)
}

export const WEEKDAYS = ['Sun', 'Mon', 'Tue', 'Wed', 'Thu', 'Fri', 'Sat'] as const
