// Desktop keyboard shortcuts — pure key → action mapping (unit-tested in shortcuts.test.ts).
// The desktop shell owns the single window keydown listener and dispatches these actions.
//
// Every action is `mod`+key: ⌘ on macOS, Ctrl elsewhere (never both, so macOS keeps Ctrl's
// Emacs-style editing keys in fields). Only navigation stays unmodified: ↑/↓ and Enter in
// the list, Esc. None of the combos is a text-editing one (⌘A/C/V/X/Z, ⌘ + arrows, ⌘Enter),
// so they may fire while typing without taking anything away from the field.

export type ShortcutAction =
  | 'next'
  | 'prev'
  | 'openAndReply'
  | 'search'
  | 'new'
  | 'back'
  | 'blur'
  | 'palette'
  | 'help'
  | 'view'
  | 'rename'
  | 'notes'
  | 'scratchpad'
  | 'inbox'
  | 'operator'
  | 'memos'
  | 'prs'
  | 'tasks'
  | 'jobs'
  | 'usage'
  | 'settings'
  | 'close'

export interface KeyLike {
  key: string
  code?: string
  metaKey?: boolean
  ctrlKey?: boolean
  altKey?: boolean
  shiftKey?: boolean
  isComposing?: boolean
}

export interface ShortcutContext {
  /** macOS: `mod` is ⌘ (metaKey), elsewhere Ctrl. */
  mac: boolean
  /** Focus is in a text field (input / textarea / select / contenteditable). */
  typing: boolean
  /** Arrow keys are free to move the list cursor (focus is not in a scrollable pane). */
  arrowsFree: boolean
}

/** Where the page runs: the OS (glyphs, which key is `mod`) and browser tab vs Fleet.app. */
export interface ShortcutEnv {
  mac: boolean
  /** Inside Fleet.app (its initialization script sets `data-shell="desktop"`). */
  app: boolean
}

/** A key combination: `mod` / `shift` / `alt` tokens, then the key (letters upper-case). */
export type Combo = readonly string[]

interface Binding {
  action: ShortcutAction
  combo: Combo
  /** Fleet.app only: its native menu owns the combo (browsers reserve it) and dispatches a
   *  `fleet:command` event; the page's keydown never matches it (no double fire). */
  app?: true
}

/**
 * The `mod` bindings, in display order (the first one an environment has is the hint).
 * Browser-reserved combos are avoided: ⌘N/⌘T/⌘W/⌘Q/⌘⇧N/⌘⇧T cannot be caught by a page,
 * ⌘L/⌘R/⌘1–9/⌘[/⌘] are the address bar, reload, tabs and history (Fleet.app's menu keeps
 * ⌘R, ⌘⇧R, ⌘W, ⌘[, ⌘]). New session is ⌘N in Fleet.app, ⌘⇧O (ChatGPT's "new chat") anywhere.
 */
export const BINDINGS: readonly Binding[] = [
  { action: 'palette', combo: ['mod', 'K'] },
  { action: 'help', combo: ['mod', '?'] },
  { action: 'help', combo: ['mod', '/'] },
  { action: 'new', combo: ['mod', 'N'], app: true },
  { action: 'new', combo: ['mod', 'shift', 'O'] },
  { action: 'search', combo: ['mod', 'F'] },
  { action: 'view', combo: ['mod', 'B'] },
  { action: 'rename', combo: ['mod', 'E'] },
  { action: 'notes', combo: ['mod', 'shift', 'E'] },
  { action: 'scratchpad', combo: ['mod', 'shift', 'S'] },
  // The modules (again → back to the sessions). ⌘, is the macOS settings key: Fleet.app and
  // Safari pass it to the page, Chrome keeps it for its own settings.
  { action: 'inbox', combo: ['mod', 'shift', 'I'] },
  { action: 'operator', combo: ['mod', 'shift', 'K'] },
  { action: 'memos', combo: ['mod', 'shift', 'M'] },
  { action: 'prs', combo: ['mod', 'shift', 'P'] },
  { action: 'tasks', combo: ['mod', 'shift', 'Y'] },
  { action: 'jobs', combo: ['mod', 'shift', 'L'] },
  { action: 'usage', combo: ['mod', 'shift', 'U'] },
  { action: 'settings', combo: ['mod', ','] },
  { action: 'close', combo: ['mod', 'Backspace'] },
]

/** The combos an environment offers for an action (the first is the one to show). */
export function combosFor(action: ShortcutAction, env: ShortcutEnv): Combo[] {
  return BINDINGS.filter((b) => b.action === action && (!b.app || env.app)).map((b) => b.combo)
}

/** The key a combo compares: lower-case letters, `?` as shift + `/`, and — for layouts whose
 *  key does not print the Latin letter / `/` / `\` — the physical key (`e.code`). */
function normalizedKey(e: KeyLike): { key: string; shift: boolean } {
  let key = e.key.length === 1 ? e.key.toLowerCase() : e.key
  let shift = !!e.shiftKey
  if (key === '?') {
    key = '/'
    shift = true
  }
  if (key.length === 1 && !/[a-z0-9/\\]/.test(key) && e.code) {
    const letter = /^Key([A-Z])$/.exec(e.code)
    if (letter) key = letter[1].toLowerCase()
    else if (e.code === 'Slash') key = '/'
    else if (e.code === 'Backslash') key = '\\'
  }
  return { key, shift }
}

function comboParts(combo: Combo): { key: string; shift: boolean; alt: boolean } {
  const last = combo[combo.length - 1]
  const q = last === '?'
  return { key: q ? '/' : last.length === 1 ? last.toLowerCase() : last, shift: q || combo.includes('shift'), alt: combo.includes('alt') }
}

/**
 * Map a keydown to an action. `mod` combos fire everywhere (fields included — none is an
 * editing combo, except ⌘⌫ which only fires outside fields); plain keys (↑/↓, Enter, F2) only outside fields, arrows only when no
 * scrollable pane has focus; ⌥↑/⌥↓ move the cursor from anywhere but a field (where they
 * are the field's own). Esc in a field leaves it.
 */
export function matchShortcut(e: KeyLike, ctx: ShortcutContext): ShortcutAction | null {
  if (e.isComposing) return null
  const meta = !!e.metaKey
  const ctrl = !!e.ctrlKey
  const mod = ctx.mac ? meta && !ctrl : ctrl && !meta
  const key = e.key

  if (mod) {
    if (e.altKey) return null
    const k = normalizedKey(e)
    for (const b of BINDINGS) {
      if (b.app) continue
      const c = comboParts(b.combo)
      // ⌘⌫ is a field's own "delete to line start": close only fires outside fields.
      if (c.key === k.key && c.shift === k.shift && !c.alt) return b.action === 'close' && ctx.typing ? null : b.action
    }
    return null
  }
  if (meta || ctrl) return null

  if (ctx.typing) return key === 'Escape' && !e.altKey && !e.shiftKey ? 'blur' : null

  if (key === 'ArrowDown' || key === 'ArrowUp') {
    if (e.shiftKey) return null
    if (!e.altKey && !ctx.arrowsFree) return null
    return key === 'ArrowDown' ? 'next' : 'prev'
  }
  if (e.altKey || e.shiftKey) return null
  if (key === 'Enter') return 'openAndReply'
  if (key === 'Escape') return 'back'
  if (key === 'F2') return 'rename'
  return null
}

/** Is this element a text-entry control (where single-key shortcuts must not fire)? */
export function isTypingTarget(el: { tagName?: string; isContentEditable?: boolean; type?: string } | null | undefined): boolean {
  if (!el || typeof el.tagName !== 'string') return false
  if (el.isContentEditable) return true
  const tag = el.tagName.toUpperCase()
  if (tag === 'TEXTAREA' || tag === 'SELECT') return true
  if (tag !== 'INPUT') return false
  const type = (el.type || 'text').toLowerCase()
  return !['button', 'checkbox', 'radio', 'range', 'submit', 'reset', 'color', 'file', 'image'].includes(type)
}

/**
 * Move a list cursor by `delta` over `keys`, clamped at both ends. A cursor that is not
 * in the list starts at the first (moving down) or last (moving up) entry.
 */
export function stepCursor(keys: readonly string[], current: string | null, delta: number): string | null {
  if (keys.length === 0) return null
  const i = current == null ? -1 : keys.indexOf(current)
  if (i < 0) return delta >= 0 ? keys[0] : keys[keys.length - 1]
  return keys[Math.max(0, Math.min(keys.length - 1, i + delta))]
}

/** `laptop/abc…` — the cursor / selection identity of a session. */
export const sessionKey = (s: { host: string; session_id: string }) => `${s.host}/${s.session_id}`

export function isMacPlatform(nav: { platform?: string; userAgent?: string } | undefined = globalThis.navigator): boolean {
  const p = `${nav?.platform ?? ''} ${nav?.userAgent ?? ''}`
  return /Mac|iPhone|iPad|iPod/.test(p)
}

/** ⌘+Enter (macOS) / Ctrl+Enter: submit a form from any of its fields (not while an IME composes). */
export function isSubmitChord(e: KeyLike): boolean {
  return e.key === 'Enter' && !!(e.metaKey || e.ctrlKey) && !e.altKey && !e.shiftKey && !e.isComposing
}

/** A bare Esc (no modifiers, not composing). */
export function isPlainEscape(e: KeyLike): boolean {
  return e.key === 'Escape' && !e.metaKey && !e.ctrlKey && !e.altKey && !e.shiftKey && !e.isComposing
}

/**
 * Backspace that cancels a pending message: unmodified, and not one that edits text — outside a
 * text field, or in an empty one (the composer right after a send).
 */
export function isUndoBackspace(e: KeyLike, target: EventTarget | null): boolean {
  if (e.key !== 'Backspace' || e.metaKey || e.ctrlKey || e.altKey || e.shiftKey || e.isComposing) return false
  const el = target as { tagName?: string; value?: string; isContentEditable?: boolean; textContent?: string | null } | null
  if (el?.tagName === 'INPUT' || el?.tagName === 'TEXTAREA') return !el.value
  if (el?.isContentEditable) return !el.textContent
  return true
}

export interface ShortcutHelp {
  /** The keys (alternatives); or `action`: its combos for the environment. */
  keys?: Combo[]
  action?: ShortcutAction
  label: string
}

/** What the ⌘? dialog shows, by area. */
export const SHORTCUT_HELP: { title: string; items: ShortcutHelp[] }[] = [
  {
    title: 'Sessions',
    items: [
      { keys: [['↓'], ['alt', '↓']], label: 'Next session (⌥ also from the chat)' },
      { keys: [['↑'], ['alt', '↑']], label: 'Previous session' },
      { keys: [['Enter']], label: 'Open and focus the composer' },
      { action: 'search', label: 'Search (↑ ↓ Enter work in the field; again: find in page)' },
      { action: 'new', label: 'New session (mod+Enter starts it)' },
      { action: 'palette', label: 'Command palette: jump to a session, run an action' },
    ],
  },
  {
    title: 'Session',
    items: [
      { action: 'rename', label: 'Rename (the open session, else the cursor row)' },
      { keys: [['F2']], label: 'Rename (outside a field)' },
      { action: 'close', label: 'Close session (press twice; the open one, else the cursor row)' },
      { keys: [['Enter'], ['mod', 'Enter']], label: 'Send (in the composer)' },
      { keys: [['shift', 'Enter']], label: 'New line' },
      { keys: [['Backspace']], label: 'Cancel a pending send (back into the composer)' },
      { keys: [['Esc']], label: 'Leave the field / close the session pane' },
    ],
  },
  {
    title: 'Layout',
    items: [
      { action: 'view', label: 'Switch List / Board (sessions grouped by work)' },
      { action: 'notes', label: 'Notes explorer (Esc back to the sessions)' },
      { action: 'scratchpad', label: 'Scratchpad: a floating, autosaved text buffer (again or Esc closes it)' },
      { action: 'help', label: 'This help' },
    ],
  },
  {
    title: 'Modules (again: back to the sessions)',
    items: [
      { action: 'inbox', label: 'Inbox: everything waiting on you' },
      { action: 'operator', label: 'Operator: talk to the fleet' },
      { action: 'memos', label: 'Voice memos: record a conversation' },
      { action: 'prs', label: 'Pull requests' },
      { action: 'tasks', label: 'Tasks (Productive)' },
      { action: 'jobs', label: 'Bots' },
      { action: 'usage', label: 'Usage limits' },
      { action: 'settings', label: 'Settings' },
    ],
  },
]

/** The keys a help row shows in this environment. */
export function helpKeys(item: ShortcutHelp, env: ShortcutEnv): Combo[] {
  return item.action ? combosFor(item.action, env) : (item.keys ?? [])
}

const MAC_GLYPH: Record<string, string> = { mod: '⌘', shift: '⇧', alt: '⌥', Backspace: '⌫' }
const PC_GLYPH: Record<string, string> = { mod: 'Ctrl', shift: 'Shift', alt: 'Alt', Backspace: 'Backspace' }

/** One key of a combo as shown: ⌘ ⇧ ⌥ on macOS, Ctrl / Shift / Alt elsewhere. */
export function keyLabel(token: string, mac: boolean): string {
  return (mac ? MAC_GLYPH : PC_GLYPH)[token] ?? token
}

/** A combo as one string: `⌘⇧O` (macOS), `Ctrl+Shift+O`. */
export function comboLabel(combo: Combo, mac: boolean): string {
  return combo.map((t) => keyLabel(t, mac)).join(mac ? '' : '+')
}

/** The hint for an action here (`⌘N` in Fleet.app, `⌘⇧O` in a browser), or '' if it has none. */
export function shortcutHint(action: ShortcutAction, env: ShortcutEnv = shortcutEnv()): string {
  const c = combosFor(action, env)[0]
  return c ? comboLabel(c, env.mac) : ''
}

/** `title` text with the hint: `New session (⌘⇧O)`. */
export const withHint = (label: string, action: ShortcutAction) => {
  const h = shortcutHint(action)
  return h ? `${label} (${h})` : label
}

/** The hint as an `aria-keyshortcuts` value (`Meta+Shift+O`, `Control+F`). */
export function ariaShortcut(action: ShortcutAction, env: ShortcutEnv = shortcutEnv()): string {
  const c = combosFor(action, env)[0]
  if (!c) return ''
  const names: Record<string, string> = { mod: env.mac ? 'Meta' : 'Control', shift: 'Shift', alt: 'Alt' }
  return c.map((t) => names[t] ?? t).join('+')
}

export function isDesktopApp(doc: { documentElement?: { dataset?: Record<string, string | undefined> } | null } | undefined = globalThis.document): boolean {
  return doc?.documentElement?.dataset?.shell === 'desktop'
}

export function shortcutEnv(): ShortcutEnv {
  return { mac: isMacPlatform(), app: isDesktopApp() }
}

/** Fleet.app's native menu → the page: `window.dispatchEvent(new CustomEvent(…, { detail: action }))`. */
export const APP_COMMAND_EVENT = 'fleet:command'
/** Actions the app's menu may ask for. */
export const APP_COMMANDS: readonly ShortcutAction[] = ['new']

/** Fleet.app → the page: a native action failed (opening iTerm, …). `command`: what to copy instead. */
export const APP_ERROR_EVENT = 'fleet:error'
export interface AppError {
  title: string
  message: string
  command?: string | null
}

/** Opens the keyboard shortcuts dialog from anywhere (the Settings screen); the desktop shell listens. */
export const SHOW_SHORTCUTS_EVENT = 'fleet:show-shortcuts'
export const showShortcuts = () => window.dispatchEvent(new Event(SHOW_SHORTCUTS_EVENT))
