import { expect, test } from 'vitest'

import { ApiError } from '@/api/client'
import type { StackMember } from '@/api/types'
import {
  conflictUpdated,
  linkPlan,
  memberCounts,
  memberCountsText,
  memberLive,
  shownStackLabel,
  sortedMembers,
  STACK_LABEL_TTL_MS,
  stackErrorMessage,
  stackHome,
  stackLabelChanged,
  stacksKnown,
  stacksMissing,
} from './stacks'

const m = (session: string, over: Partial<StackMember> = {}): StackMember => ({
  session,
  host: 'laptop',
  name: session,
  added: '2026-09-29T10:00:00.000Z',
  closed: null,
  ...over,
})

test('memberLive: the server’s live flag wins, else closed == null', () => {
  expect(memberLive(m('a'))).toBe(true)
  expect(memberLive(m('a', { closed: '2026-09-29T11:00:00.000Z' }))).toBe(false)
  expect(memberLive(m('a', { live: false }))).toBe(false)
})

test('memberCounts / memberCountsText', () => {
  const members = [m('a'), m('b', { closed: '2026-09-29T11:00:00.000Z', live: false }), m('c', { live: true })]
  expect(memberCounts({ members })).toEqual({ live: 2, closed: 1 })
  expect(memberCountsText({ members })).toBe('2 live · 1 closed')
  expect(memberCountsText({ members: [m('a')] })).toBe('1 live')
  expect(memberCounts(null)).toEqual({ live: 0, closed: 0 })
})

test('sortedMembers: live first in stack order, then the most recently closed', () => {
  const members = [
    m('old', { closed: '2026-09-29T09:00:00.000Z' }),
    m('a'),
    m('new', { closed: '2026-09-29T12:00:00.000Z' }),
    m('b'),
  ]
  expect(sortedMembers(members).map((x) => x.session)).toEqual(['a', 'b', 'new', 'old'])
  expect(sortedMembers(members, 'b').map((x) => x.session)).toEqual(['b', 'a', 'new', 'old'])
})

test('stacksMissing: 501 or the router’s generic 404, not an unknown stack', () => {
  expect(stacksMissing(new ApiError('not found', 404))).toBe(true)
  expect(stacksMissing(new ApiError('stacks disabled', 501))).toBe(true)
  expect(stacksMissing(new ApiError('unknown stack: st-1', 404))).toBe(false)
  expect(stacksMissing(new Error('x'))).toBe(false)
  expect(stackErrorMessage(new ApiError('not found', 404))).toMatch(/predates session stacks/)
  expect(stackErrorMessage(new ApiError('unknown stack: st-1', 404))).toBe('unknown stack: st-1')
})

test('conflictUpdated reads the 409 body', () => {
  expect(conflictUpdated(new ApiError('conflict', 409, { error: 'conflict', updated: '2026-09-29T12:00:00.000Z' }))).toBe('2026-09-29T12:00:00.000Z')
  expect(conflictUpdated(new ApiError('x', 400, { updated: 'y' }))).toBeNull()
})

test('stackHome: the stack’s home host, else the session’s own (older servers)', () => {
  expect(stackHome({ host: 'workstation', stack: { id: 'st-1', label: 'x', host: 'laptop' } })).toBe('laptop')
  expect(stackHome({ host: 'workstation', stack: { id: 'st-1', label: 'x' } })).toBe('workstation')
  expect(stackHome({ host: 'workstation', stack: null })).toBe('workstation')
})

test('stacksKnown: rows from newer CLIs carry stack (null included)', () => {
  expect(stacksKnown({ stack: null })).toBe(true)
  expect(stacksKnown({ stack: { id: 'st-1', label: 'x' } })).toBe(true)
  expect(stacksKnown({})).toBe(false)
  expect(stacksKnown(null)).toBe(false)
})

test('stackLabelChanged: trimmed, non-empty and different', () => {
  expect(stackLabelChanged('  New name ', 'Old')).toBe(true)
  expect(stackLabelChanged('Old', 'Old')).toBe(false)
  expect(stackLabelChanged(' Old  ', 'Old')).toBe(false)
  expect(stackLabelChanged('   ', 'Old')).toBe(false)
  expect(stackLabelChanged('', null)).toBe(false)
  expect(stackLabelChanged('First', null)).toBe(true)
})

test('shownStackLabel: pending wins while saving, then until the server catches up or the TTL ends', () => {
  const at = 1_000_000
  expect(shownStackLabel('Old', null, at)).toBe('Old')
  expect(shownStackLabel('Old', { label: 'New', at, saving: true }, at + STACK_LABEL_TTL_MS * 2)).toBe('New')
  expect(shownStackLabel('Old', { label: 'New', at, saving: false }, at + 1000)).toBe('New')
  expect(shownStackLabel('New', { label: 'New', at, saving: false }, at + 1000)).toBe('New')
  expect(shownStackLabel('Old', { label: 'New', at, saving: false }, at + STACK_LABEL_TTL_MS)).toBe('Old')
})

test('linkPlan: create around a, join whichever has a stack, same / conflict otherwise', () => {
  const st = { id: 'st-1', label: 'Login fix', host: 'laptop' }
  const a = { host: 'laptop', session_id: 'a', stack: null }
  const b = { host: 'workstation', session_id: 'b', stack: null }
  expect(linkPlan(a, b)).toEqual({ kind: 'create', host: 'laptop', sessionId: 'a', other: { session: 'b', host: 'workstation' } })
  expect(linkPlan(a, { ...b, stack: st })).toEqual({ kind: 'join', host: 'laptop', stackId: 'st-1', label: 'Login fix', other: { session: 'a', host: 'laptop' } })
  expect(linkPlan({ ...b, stack: st }, a)).toEqual({ kind: 'join', host: 'laptop', stackId: 'st-1', label: 'Login fix', other: { session: 'a', host: 'laptop' } })
  expect(linkPlan({ ...a, stack: st }, { ...b, stack: st })).toEqual({ kind: 'same' })
  expect(linkPlan({ ...a, stack: st }, { ...b, stack: { id: 'st-2', label: 'Other' } })).toEqual({ kind: 'conflict', label: 'Other' })
})
