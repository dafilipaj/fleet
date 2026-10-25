// Viewer preferences shown on the Settings screen (`#/settings`) — pure helpers, unit-tested in
// prefs.test.ts. Stored per viewer in localStorage.
import { CHAT_FONT_SIZES, parseSize } from '@/lib/chat'

export const TEXT_SIZE_KEY = 'fleet.chatFont'
export const HIDE_NOTES_KEY = 'fleet.chatHideNotes'
/** Composer send delay (ms: 0 / 3000 / 5000, lib/outbox.ts SEND_DELAYS) — the undo window. */
export const SEND_DELAY_KEY = 'fleet.sendDelay'

/**
 * Text size steps, named by the chat text size they give (the stored `fleet.chatFont`
 * values). The whole UI scales with it: the root font-size is set so the chat's 0.9375rem
 * comes out at exactly this many px, and every rem-based size scales along.
 */
export const TEXT_SIZES = CHAT_FONT_SIZES
export const DEFAULT_TEXT_SIZE = 15
export const TEXT_SIZE_LABELS: Record<number, string> = { 13: 'Small', 15: 'Default', 17: 'Large' }

export const parseTextSize = parseSize(TEXT_SIZES)

/** The chat text size at the default root (16px), in rem: scales with the root. */
export const CHAT_FONT_REM = `${DEFAULT_TEXT_SIZE / 16}rem`

/**
 * The <html> font-size for a text size, as a percentage of the browser default (so a reader's
 * own browser setting still applies). 15 (Default) → '' (no override).
 */
export function rootFontSize(size: number): string {
  if (!TEXT_SIZES.includes(size as (typeof TEXT_SIZES)[number]) || size === DEFAULT_TEXT_SIZE) return ''
  return `${+((size / DEFAULT_TEXT_SIZE) * 100).toFixed(3)}%`
}
