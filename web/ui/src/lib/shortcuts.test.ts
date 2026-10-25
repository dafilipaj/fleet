import { describe, expect, it, test } from 'vitest'

import {
  ariaShortcut,
  BINDINGS,
  comboLabel,
  combosFor,
  helpKeys,
  isDesktopApp,
  isMacPlatform,
  isPlainEscape,
  isUndoBackspace,
  isSubmitChord,
  isTypingTarget,
  keyLabel,
  matchShortcut,
  sessionKey,
  SHORTCUT_HELP,
  shortcutHint,
  stepCursor,
  type KeyLike,
  type ShortcutContext,
  type ShortcutEnv,
} from '@/lib/shortcuts'

const idle: ShortcutContext = { mac: true, typing: false, arrowsFree: true }
const act = (key: string, ctx: Partial<ShortcutContext> = {}, mods: Partial<KeyLike> = {}) => matchShortcut({ key, ...mods }, { ...idle, ...ctx })
const cmd = (key: string, mods: Partial<KeyLike> = {}, ctx: Partial<ShortcutContext> = {}) => act(key, ctx, { metaKey: true, ...mods })
const browser: ShortcutEnv = { mac: true, app: false }
const app: ShortcutEnv = { mac: true, app: true }
const pc: ShortcutEnv = { mac: false, app: false }

describe('matchShortcut', () => {
  it('maps ⌘ combos', () => {
    expect(cmd('k')).toBe('palette')
    expect(cmd('/')).toBe('help')
    expect(cmd('?', { shiftKey: true })).toBe('help')
    expect(cmd('/', { shiftKey: true })).toBe('help')
    expect(cmd('O', { shiftKey: true })).toBe('new')
    expect(cmd('o', { shiftKey: true })).toBe('new')
    expect(cmd('f')).toBe('search')
    expect(cmd('b')).toBe('view')
    expect(cmd('i')).toBe('inspector')
    expect(cmd('j')).toBe('mode')
    expect(cmd('e')).toBe('rename')
    expect(cmd('E', { shiftKey: true })).toBe('notes')
    expect(cmd('S', { shiftKey: true })).toBe('scratchpad')
    expect(cmd('K', { shiftKey: true })).toBe('operator')
    expect(cmd('P', { shiftKey: true })).toBe('prs')
    expect(cmd('Y', { shiftKey: true })).toBe('tasks')
    expect(cmd('L', { shiftKey: true })).toBe('jobs')
    expect(cmd('U', { shiftKey: true })).toBe('usage')
    expect(cmd(',')).toBe('settings')
  })

  it('uses ⌘ on macOS and Ctrl elsewhere, never both', () => {
    expect(act('k', { mac: false }, { ctrlKey: true })).toBe('palette')
    expect(act('k', { mac: false }, { metaKey: true })).toBeNull()
    // macOS Ctrl keeps its Emacs-style field bindings (Ctrl+K kills to the end of the line).
    expect(act('k', { mac: true }, { ctrlKey: true })).toBeNull()
    expect(act('k', { typing: true }, { ctrlKey: true })).toBeNull()
    expect(cmd('k', { ctrlKey: true })).toBeNull()
    expect(act('b', { mac: false }, { ctrlKey: true })).toBe('view')
  })

  it('needs the exact modifiers', () => {
    expect(cmd('x', { shiftKey: true })).toBeNull()
    expect(cmd('k', { altKey: true })).toBeNull()
    expect(cmd('o')).toBeNull() // ⌘O: the browser's open file
    expect(cmd('e', { altKey: true })).toBeNull()
  })

  it('never matches browser-reserved or Fleet.app menu combos', () => {
    // ⌘N is Fleet.app's native menu item (it dispatches fleet:command), never the page's keydown.
    for (const k of ['n', 't', 'w', 'q', 'l', 'r', '1', '9', '[', ']']) expect(cmd(k)).toBeNull()
    expect(cmd('N', { shiftKey: true })).toBeNull()
    expect(cmd('T', { shiftKey: true })).toBeNull()
    expect(cmd('R', { shiftKey: true })).toBeNull()
  })

  it('leaves editing combos to the field', () => {
    for (const k of ['a', 'c', 'v', 'x', 'z', 'y', 'ArrowLeft', 'ArrowRight', 'ArrowUp', 'ArrowDown', 'Backspace', 'Enter'])
      expect(cmd(k, {}, { typing: true })).toBeNull()
    expect(cmd('Z', { shiftKey: true }, { typing: true })).toBeNull()
    for (const k of ['ArrowUp', 'ArrowDown', 'ArrowLeft', 'ArrowRight', 'Backspace']) expect(act(k, { typing: true }, { altKey: true })).toBeNull()
    expect(act('n', { typing: true }, { altKey: true })).toBeNull() // ⌥N: the ˜ dead key
  })

  it('closes a session with ⌘⌫, but never inside a field (delete to line start)', () => {
    expect(cmd('Backspace')).toBe('close')
    expect(cmd('Backspace', {}, { typing: true })).toBeNull()
    expect(act('Backspace', { mac: false }, { ctrlKey: true })).toBe('close')
    expect(act('Backspace')).toBeNull()
    expect(comboLabel(combosFor('close', browser)[0], true)).toBe('⌘⌫')
    expect(comboLabel(combosFor('close', pc)[0], false)).toBe('Ctrl+Backspace')
  })

  it('fires action combos while typing', () => {
    for (const [k, a] of [['k', 'palette'], ['/', 'help'], ['b', 'view'], ['f', 'search'], ['j', 'mode']] as const)
      expect(cmd(k, {}, { typing: true })).toBe(a)
    expect(cmd('O', { shiftKey: true }, { typing: true })).toBe('new')
  })

  it('maps plain navigation keys outside fields only', () => {
    expect(act('ArrowDown')).toBe('next')
    expect(act('ArrowUp')).toBe('prev')
    expect(act('Enter')).toBe('openAndReply')
    expect(act('Escape')).toBe('back')
    expect(act('F2')).toBe('rename')
    for (const k of ['ArrowDown', 'Enter', 'F2']) expect(act(k, { typing: true })).toBeNull()
    expect(act('Escape', { typing: true })).toBe('blur')
    expect(act('ArrowDown', {}, { shiftKey: true })).toBeNull()
  })

  it('no single-letter bindings', () => {
    for (const k of 'abcdefghijklmnopqrstuvwxyz/?[G') expect(act(k)).toBeNull()
  })

  it('arrows move the cursor only outside a scrollable pane; ⌥ + arrows from anywhere but a field', () => {
    expect(act('ArrowDown', { arrowsFree: false })).toBeNull()
    expect(act('ArrowDown', { arrowsFree: false }, { altKey: true })).toBe('next')
    expect(act('ArrowUp', { arrowsFree: false }, { altKey: true })).toBe('prev')
  })

  it('falls back to the physical key on other layouts', () => {
    expect(cmd('л', { code: 'KeyK' })).toBe('palette') // Cyrillic
    expect(cmd('-', { code: 'Slash' })).toBe('help') // German: the Slash position types '-'
    expect(cmd('k', { code: 'KeyL' })).toBe('palette') // Dvorak and co. follow the printed letter
  })

  it('ignores IME composition', () => {
    expect(cmd('k', { isComposing: true })).toBeNull()
    expect(act('Enter', {}, { isComposing: true })).toBeNull()
  })
})

describe('hints', () => {
  it('shows the environment\'s combo', () => {
    expect(shortcutHint('new', browser)).toBe('⌘⇧O')
    expect(shortcutHint('new', app)).toBe('⌘N')
    expect(shortcutHint('new', pc)).toBe('Ctrl+Shift+O')
    expect(shortcutHint('help', browser)).toBe('⌘?')
    expect(shortcutHint('palette', pc)).toBe('Ctrl+K')
    expect(shortcutHint('back', browser)).toBe('')
  })
  it('lists the app-only combo only in the app', () => {
    expect(combosFor('new', browser)).toEqual([['mod', 'shift', 'O']])
    expect(combosFor('new', app)).toEqual([
      ['mod', 'N'],
      ['mod', 'shift', 'O'],
    ])
    expect(combosFor('help', browser)).toHaveLength(2)
  })
  it('labels keys per platform', () => {
    expect(comboLabel(['mod', 'alt', '↓'], true)).toBe('⌘⌥↓')
    expect(comboLabel(['mod', 'alt', '↓'], false)).toBe('Ctrl+Alt+↓')
    expect(keyLabel('shift', true)).toBe('⇧')
    expect(keyLabel('shift', false)).toBe('Shift')
  })
  it('aria-keyshortcuts', () => {
    expect(ariaShortcut('new', browser)).toBe('Meta+Shift+O')
    expect(ariaShortcut('search', pc)).toBe('Control+F')
  })
  it('every help row has keys, and every action combo is reachable', () => {
    for (const env of [browser, app, pc])
      for (const sec of SHORTCUT_HELP) for (const item of sec.items) expect(helpKeys(item, env).length).toBeGreaterThan(0)
    // Each non-app binding matches its own combo.
    for (const b of BINDINGS.filter((x) => !x.app)) {
      const key = b.combo[b.combo.length - 1]
      const shiftKey = b.combo.includes('shift') || key === '?'
      expect(matchShortcut({ key: shiftKey && key.length === 1 ? key.toUpperCase() : key, metaKey: true, shiftKey }, idle)).toBe(b.action)
    }
  })
  it('isDesktopApp reads data-shell', () => {
    expect(isDesktopApp({ documentElement: { dataset: { shell: 'desktop' } } })).toBe(true)
    expect(isDesktopApp({ documentElement: { dataset: {} } })).toBe(false)
    expect(isDesktopApp(undefined)).toBe(false)
  })
})

describe('isTypingTarget', () => {
  it('detects text fields', () => {
    expect(isTypingTarget({ tagName: 'INPUT' })).toBe(true)
    expect(isTypingTarget({ tagName: 'INPUT', type: 'search' })).toBe(true)
    expect(isTypingTarget({ tagName: 'TEXTAREA' })).toBe(true)
    expect(isTypingTarget({ tagName: 'SELECT' })).toBe(true)
    expect(isTypingTarget({ tagName: 'DIV', isContentEditable: true })).toBe(true)
    expect(isTypingTarget({ tagName: 'INPUT', type: 'checkbox' })).toBe(false)
    expect(isTypingTarget({ tagName: 'BUTTON' })).toBe(false)
    expect(isTypingTarget({ tagName: 'DIV' })).toBe(false)
    expect(isTypingTarget(null)).toBe(false)
  })
})

describe('stepCursor', () => {
  const keys = ['a', 'b', 'c']
  it('moves and clamps', () => {
    expect(stepCursor(keys, 'a', 1)).toBe('b')
    expect(stepCursor(keys, 'c', 1)).toBe('c')
    expect(stepCursor(keys, 'a', -1)).toBe('a')
    expect(stepCursor(keys, 'b', -1)).toBe('a')
    expect(stepCursor(keys, 'a', 99)).toBe('c')
  })
  it('starts at an end when the cursor is not in the list', () => {
    expect(stepCursor(keys, null, 1)).toBe('a')
    expect(stepCursor(keys, 'zz', -1)).toBe('c')
    expect(stepCursor([], 'a', 1)).toBeNull()
  })
})

describe('helpers', () => {
  it('sessionKey', () => {
    expect(sessionKey({ host: 'laptop', session_id: 'abc' })).toBe('laptop/abc')
  })
  it('isMacPlatform', () => {
    expect(isMacPlatform({ platform: 'MacIntel' })).toBe(true)
    expect(isMacPlatform({ platform: 'Linux x86_64', userAgent: 'X11' })).toBe(false)
  })
})

test('isSubmitChord: ⌘/Ctrl+Enter only', () => {
  expect(isSubmitChord({ key: 'Enter', metaKey: true })).toBe(true)
  expect(isSubmitChord({ key: 'Enter', ctrlKey: true })).toBe(true)
  expect(isSubmitChord({ key: 'Enter' })).toBe(false)
  expect(isSubmitChord({ key: 'Enter', metaKey: true, shiftKey: true })).toBe(false)
  expect(isSubmitChord({ key: 'Enter', metaKey: true, isComposing: true })).toBe(false)
})

test('isPlainEscape: bare Esc only', () => {
  expect(isPlainEscape({ key: 'Escape' })).toBe(true)
  expect(isPlainEscape({ key: 'Escape', metaKey: true })).toBe(false)
  expect(isPlainEscape({ key: 'Escape', isComposing: true })).toBe(false)
  expect(isPlainEscape({ key: 'Enter' })).toBe(false)
})

test('isUndoBackspace: bare Backspace outside text fields or in an empty one', () => {
  const field = (tagName: string, value: string) => ({ tagName, value }) as unknown as EventTarget
  expect(isUndoBackspace({ key: 'Backspace' }, null)).toBe(true)
  expect(isUndoBackspace({ key: 'Backspace' }, field('TEXTAREA', ''))).toBe(true)
  expect(isUndoBackspace({ key: 'Backspace' }, field('TEXTAREA', 'typing'))).toBe(false)
  expect(isUndoBackspace({ key: 'Backspace' }, field('INPUT', 'x'))).toBe(false)
  expect(isUndoBackspace({ key: 'Backspace' }, { isContentEditable: true, textContent: 'x' } as unknown as EventTarget)).toBe(false)
  expect(isUndoBackspace({ key: 'Backspace', metaKey: true }, null)).toBe(false)
  expect(isUndoBackspace({ key: 'Backspace', isComposing: true }, null)).toBe(false)
  expect(isUndoBackspace({ key: 'Escape' }, null)).toBe(false)
})
