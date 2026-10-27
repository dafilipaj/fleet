import { describe, expect, it } from 'vitest'

import type { ScheduledEntry } from '@/api/types'
import { localInputValue, pendingEntries, scheduleWhat, scheduleWhen } from './schedule'

const entry = (o: Partial<ScheduledEntry>): ScheduledEntry => ({
  id: 's1',
  at: null,
  dir: '~/x',
  prompt: '',
  name: null,
  model: null,
  harness: 'claude',
  ghIdentity: null,
  createdAt: 0,
  state: 'pending',
  reason: null,
  firedAt: null,
  tmuxSession: null,
  ...o,
})
const task = { id: '9', number: '33', title: 'Setup', url: 'u', startDate: null, checkedAt: 0 }

describe('schedule', () => {
  const now = new Date(2026, 9, 27, 10, 0).getTime()
  it('says when, task-linked or not', () => {
    expect(scheduleWhen(entry({ at: new Date(2026, 9, 28, 8, 0).getTime() }), now)).toBe('tomorrow 08:00')
    expect(scheduleWhen(entry({ at: new Date(2026, 9, 27, 11, 30).getTime(), task }), now)).toBe('today 11:30 · task start')
    expect(scheduleWhen(entry({ task, time: '09:15' }), now)).toBe('09:15 on its start date (none yet)')
    expect(scheduleWhen(entry({ at: new Date(2026, 9, 26, 8, 0).getTime(), task }), now)).toBe('next tick (was yesterday 08:00)')
  })
  it('names the task, else the prompt', () => {
    expect(scheduleWhat(entry({ task }))).toBe('#33 Setup')
    expect(scheduleWhat(entry({ prompt: 'Triage' }))).toBe('Triage')
    expect(scheduleWhat(entry({}))).toBe('(no prompt)')
  })
  it('lists pending entries of every host, soonest first, undated last', () => {
    const got = pendingEntries([
      { host: 'a', entries: [entry({ id: 'x', at: 30 }), entry({ id: 'done', at: 1, state: 'started' })] },
      { host: 'b', entries: [entry({ id: 'y', task }), entry({ id: 'z', at: 20 })] },
    ])
    expect(got.map((e) => `${e.host}/${e.id}`)).toEqual(['b/z', 'a/x', 'b/y'])
  })
  it('formats a datetime-local value', () => {
    expect(localInputValue(new Date(2026, 0, 5, 7, 3).getTime())).toBe('2026-01-05T07:03')
  })
})
