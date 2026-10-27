// Scheduled sessions (`fleet schedule`, docs/cli.md → Scheduled sessions) — pure helpers.
import type { ScheduledEntry } from '@/api/types'
import { whenLabel } from '@/lib/jobs'

export type HostedEntry = ScheduledEntry & { host: string }

/** When it starts: "tomorrow 08:00", "Thu 29.10. 08:00 · task start", "08:00 on the start date (none yet)". */
export function scheduleWhen(e: ScheduledEntry, now = Date.now()): string {
  if (e.at == null) return e.task ? `${e.time ?? '08:00'} on its start date (none yet)` : 'never'
  const when = whenLabel(e.at, now)
  if (e.state === 'pending' && e.at <= now) return `next tick (was ${when})`
  return e.task ? `${when} · task start` : when
}

/** What it starts on: the task, else the prompt. */
export function scheduleWhat(e: ScheduledEntry): string {
  if (e.task) return `#${e.task.number} ${e.task.title}`
  return e.prompt || '(no prompt)'
}

/** Pending entries of every host, soonest first (no date yet last). */
export function pendingEntries(results: { host: string; entries: ScheduledEntry[] }[]): HostedEntry[] {
  return results
    .flatMap((r) => r.entries.filter((e) => e.state === 'pending').map((e) => ({ ...e, host: r.host })))
    .sort((a, b) => (a.at ?? Infinity) - (b.at ?? Infinity))
}

/** `<input type="datetime-local">` value for `ms` (local time, minutes). */
export function localInputValue(ms: number): string {
  const d = new Date(ms)
  const p = (n: number) => String(n).padStart(2, '0')
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}T${p(d.getHours())}:${p(d.getMinutes())}`
}
