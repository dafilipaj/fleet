// Chat view logic — pure, unit-tested in chat.test.ts.
import type { Message } from '@/api/types'

export const CHAT_FONT_SIZES = [13, 15, 17] as const
/** "Load older" steps through these message limits. */
export const CHAT_LIMITS = [60, 200, 500] as const
/** Captions only on the last message of a same-speaker burst this close in time. */
export const GROUP_GAP_MS = 2 * 60 * 1000

export const CHAT_POLL_MS = 3000
/** Composer text limit (server: 1..8000 chars). */
export const MAX_SEND_CHARS = 8000

/** A stored size must be one of `sizes`; anything else is the middle one. */
export function parseSize(sizes: readonly number[]) {
  return (raw: string): number | undefined => {
    const n = Number(raw)
    return sizes.includes(n) ? n : undefined
  }
}

/** Next size up/down, clamped. An unknown current size counts as the middle one. */
export function stepSize(sizes: readonly number[], current: number, dir: 1 | -1): number {
  const i = sizes.indexOf(current)
  return sizes[Math.min(sizes.length - 1, Math.max(0, (i === -1 ? 1 : i) + dir))]
}

export function nextChatLimit(current: number): number | null {
  return CHAT_LIMITS.find((n) => n > current) ?? null
}

export const msgKind = (m: Message | null | undefined): string => String(m?.kind || m?.role || 'assistant')

/** Interim = assistant narration between tool calls (`final: false`). */
export const isInterim = (m: Message) => m.role === 'assistant' && m.final === false

/** Same speaker, close in time → one caption for the whole burst. */
export function sameGroup(a: Message | undefined, b: Message | undefined): boolean {
  if (!a || !b) return false
  const ka = msgKind(a)
  if (ka !== msgKind(b)) return false
  if (ka === 'command' || ka === 'system') return false
  const ta = Number(a.ts)
  const tb = Number(b.ts)
  if (!Number.isFinite(ta) || !Number.isFinite(tb) || ta <= 0 || tb <= 0) return false
  return Math.abs(tb - ta) <= GROUP_GAP_MS
}

export function visibleMessages(messages: Message[] | null | undefined, hideInterim: boolean): Message[] {
  const list = Array.isArray(messages) ? messages : []
  return hideInterim ? list.filter((m) => !isInterim(m)) : list
}

/** The follow-scroll rule: "at the bottom" = within `slack` px of it. */
export function nearBottom(el: { scrollHeight: number; scrollTop: number; clientHeight: number }, slack: number): boolean {
  return el.scrollHeight - el.scrollTop - el.clientHeight <= slack
}

export type TextPart = { pasted: false; text: string } | { pasted: true; text: string }

// Claude Code repeats the id on the closing tag: `<pasted_content id="ab12">…</pasted_content id="ab12">`.
const PASTED_RE = /<pasted_content(?:\s[^>]*)?>([\s\S]*?)<\/pasted_content(?:\s[^>]*)?>/g

/** Split a user prompt into typed text and `<pasted_content>` blocks (tags dropped, blank edges trimmed). */
export function splitPasted(text: string): TextPart[] {
  const parts: TextPart[] = []
  let last = 0
  for (const m of text.matchAll(PASTED_RE)) {
    const before = text.slice(last, m.index).trim()
    if (before) parts.push({ pasted: false, text: before })
    const body = m[1].replace(/^\n+|\s+$/g, '')
    if (body) parts.push({ pasted: true, text: body })
    last = m.index + m[0].length
  }
  const rest = text.slice(last).trim()
  if (rest || !parts.length) parts.push({ pasted: false, text: last ? rest : text })
  return parts.filter((p) => p.text)
}
