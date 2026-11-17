import { expect, test } from 'vitest'

import type { InboxItem } from '@/api/types'
import { briefMarkdown, dueChip, inboxSections, isNewItem, itemTarget, sourceLines } from './inbox'

const item = (over: Partial<InboxItem>): InboxItem => ({
  id: 'pr:x',
  source: 'github',
  kind: 'review',
  section: 'quick',
  title: 'Fix it',
  ask: null,
  url: null,
  href: null,
  from: null,
  at: 1,
  firstSeen: 1,
  effort: null,
  meta: {},
  ...over,
})

test('inboxSections keeps section order and drops empty ones', () => {
  const s = inboxSections([item({ id: 'a', section: 'mine' }), item({ id: 'b', section: 'quick' }), item({ id: 'c', section: 'mine' })])
  expect(s.map((x) => [x.id, x.items.map((i) => i.id)])).toEqual([
    ['quick', ['b']],
    ['mine', ['a', 'c']],
  ])
})

test('isNewItem: after the last visit and under an hour old', () => {
  const now = 10 * 60 * 60 * 1000
  expect(isNewItem({ firstSeen: now - 1000 }, now - 5000, now)).toBe(true)
  expect(isNewItem({ firstSeen: now - 1000 }, now, now)).toBe(false)
  expect(isNewItem({ firstSeen: now - 2 * 60 * 60 * 1000 }, 0, now)).toBe(false)
})

test('itemTarget prefers the in-app route', () => {
  expect(itemTarget({ href: '#/s/h/1', url: 'https://x' })).toEqual({ href: '#/s/h/1', external: false })
  expect(itemTarget({ href: null, url: 'https://x' })).toEqual({ href: 'https://x', external: true })
  expect(itemTarget({ href: null, url: null })).toBeNull()
})

test('sourceLines: running, error, ok', () => {
  const now = 1_000_000
  const lines = sourceLines(
    {
      github: { at: now - 120_000, ok: true, error: null, count: 3 },
      productive: { at: null, ok: false, error: 'no token', count: 0 },
      comms: { at: null, ok: true, error: null, count: 0, running: true },
    },
    now,
  )
  expect(lines).toEqual([
    { key: 'github', label: 'GitHub', text: '2m ago', warn: false },
    { key: 'productive', label: 'Productive', text: 'no token', warn: true },
    { key: 'comms', label: 'Slack & email', text: 'scanning…', warn: false },
  ])
})

test('dueChip', () => {
  const now = new Date(2026, 9, 20, 12).getTime()
  expect(dueChip({ due: '2026-10-18' }, now)).toEqual({ label: 'overdue 2d', overdue: true })
  expect(dueChip({ due: '2026-10-20' }, now)).toEqual({ label: 'due today', overdue: false })
  expect(dueChip({ due: '2026-10-21' }, now)).toEqual({ label: 'due tomorrow', overdue: false })
  expect(dueChip({}, now)).toBeNull()
})

test('briefMarkdown makes in-app links absolute and leaves the rest', () => {
  expect(briefMarkdown('[a](#/s/laptop/x) [b](https://x.y/1)', 'http://h:7777')).toBe('[a](http://h:7777/#/s/laptop/x) [b](https://x.y/1)')
})

test('sourceLines: with the Slack poller the scan is "Email", and a Slack poll never shows as scanning', () => {
  const lines = sourceLines({ slack: { at: 1000, ok: true, error: null, count: 1, running: true }, comms: { at: 1000, ok: true, error: null, count: 0 } }, 61_000)
  expect(lines.map((l) => l.label)).toEqual(['Slack', 'Email'])
  expect(lines[0].text).toBe('1m ago')
})
