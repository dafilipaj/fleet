import { describe, expect, it } from 'vitest'

import { detectLinkKind, keepLabel, keepValue, LINK_KINDS } from '@/lib/links'

describe('detectLinkKind', () => {
  it('tells the kind from what was pasted', () => {
    expect(detectLinkKind('https://acme.slack.com/archives/C0123/p1700000000000100', LINK_KINDS)).toBe('slack')
    expect(detectLinkKind('@Ana Example', LINK_KINDS)).toBe('slack')
    expect(detectLinkKind('https://github.com/acme/app/pull/12', LINK_KINDS)).toBe('github')
    expect(detectLinkKind('https://github.com/acme/app/pull/12/files#diff', LINK_KINDS)).toBe('github')
    expect(detectLinkKind('acme/app#12', LINK_KINDS)).toBe('github')
    expect(detectLinkKind('https://app.productive.io/1-acme/tasks/1001', LINK_KINDS)).toBe('productive')
    expect(detectLinkKind('https://app.productive.io/1-acme/projects/5/tasks/task/1001?x=1', LINK_KINDS)).toBe('productive')
    expect(detectLinkKind('team-demo', LINK_KINDS)).toBe('veranda')
  })

  it('null for anything else, and for a kind this session cannot have', () => {
    expect(detectLinkKind('', LINK_KINDS)).toBeNull()
    expect(detectLinkKind('https://example.com/x', LINK_KINDS)).toBeNull()
    expect(detectLinkKind('Team Demo', LINK_KINDS)).toBeNull()
    expect(detectLinkKind('https://github.com/acme/app/issues/3', LINK_KINDS)).toBeNull()
    expect(detectLinkKind('https://github.com/acme/app/pull/12', ['slack', 'veranda'])).toBeNull()
  })
})

describe('keep', () => {
  it('minutes to the option and the words', () => {
    expect(keepValue(null)).toBeNull()
    expect(keepValue(1440)).toBe('1d')
    expect(keepValue(90)).toBe('90m')
    expect(keepLabel(null)).toBe('never sleeps')
    expect(keepLabel(720)).toBe('sleeps after 12h idle')
    expect(keepLabel(10080)).toBe('sleeps after 7d idle')
  })
})
