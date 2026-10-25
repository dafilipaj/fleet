import { describe, expect, it } from 'vitest'

import type { PullRequest, Session } from '@/api/types'

import { checksTone, groupPrs, isOthersDraft, linkSessions, parsePrLocation, prHref, promptChips, prRef, reviewLabel, waitingSince } from './prs'

const pr = (o: Partial<PullRequest>): PullRequest => ({
  column: 'waitingOnMe',
  owner: 'acme',
  repo: 'app',
  number: 1,
  title: 'Fix login',
  url: 'https://github.com/acme/app/pull/1',
  author: 'ana',
  draft: false,
  createdAt: 0,
  updatedAt: 0,
  additions: 1,
  deletions: 0,
  changedFiles: 1,
  size: 'XS',
  baseBranch: 'main',
  headBranch: 'fix-login',
  headOid: null,
  reviewDecision: null,
  checks: null,
  comments: 0,
  requestedReviewers: [],
  myReview: null,
  ...o,
})

describe('groupPrs', () => {
  const columns = [
    { id: 'waitingOnMe' as const, title: 'Waiting on me', count: 1 },
    { id: 'draftMine' as const, title: 'My drafts', count: 1 },
  ]
  const prs = [pr({ number: 1 }), pr({ number: 2, column: 'draftMine', title: 'Refactor', headBranch: 'r' }), pr({ number: 3, column: 'futureColumn' as never })]

  it('keeps the server order, empty columns and unknown ones last', () => {
    const g = groupPrs({ columns, prs })
    expect(g.map((c) => [c.id, c.prs.map((p) => p.number)])).toEqual([
      ['waitingOnMe', [1]],
      ['draftMine', [2]],
      ['futureColumn', [3]],
    ])
  })

  it('filters by title, repo#number and author', () => {
    expect(groupPrs({ columns, prs }, 'refac').flatMap((c) => c.prs.map((p) => p.number))).toEqual([2])
    expect(groupPrs({ columns, prs }, 'app#3').flatMap((c) => c.prs.map((p) => p.number))).toEqual([3])
    expect(groupPrs({ columns, prs }, 'ANA').flatMap((c) => c.prs.map((p) => p.number))).toEqual([1, 2, 3])
  })

  it('falls back to the default columns and copes with nothing', () => {
    expect(groupPrs(null)).toEqual([])
    expect(groupPrs({ columns: [], prs: [] }).map((c) => c.id)).toEqual([
      'unseenMentions',
      'waitingOnMe',
      'reviewMine',
      'readyToMergeMine',
      'draftMine',
      'waitingOnAuthor',
    ])
  })
})

describe('labels', () => {
  it('checks', () => {
    expect([checksTone('success'), checksTone('error'), checksTone('expected'), checksTone(null)]).toEqual(['ok', 'bad', 'running', 'none'])
  })
  it('review', () => {
    expect(reviewLabel(pr({ myReview: { state: 'changesRequested', at: null, newCommits: true } }))).toBe('new commits since your review')
    expect(reviewLabel(pr({ myReview: { state: 'changesRequested', at: null, newCommits: false } }))).toBe('you requested changes')
    expect(reviewLabel(pr({ reviewDecision: 'approved' }))).toBe('approved')
    expect(reviewLabel(pr({}))).toBeNull()
  })
})

describe('drafts', () => {
  it("hides other people's drafts, never yours or a mention", () => {
    const mention = { at: 1, author: 'bo', url: '', excerpt: '', answered: false, seen: false }
    const prs = [
      pr({ number: 1, draft: true }),
      pr({ number: 2, draft: true, author: 'Me', column: 'draftMine' }),
      pr({ number: 3 }),
      pr({ number: 4, draft: true, column: 'unseenMentions', mention }),
    ]
    const nums = (hide: boolean) =>
      groupPrs({ columns: [], prs, user: 'me' }, '', { hideOthersDrafts: hide }).flatMap((g) => g.prs.map((p) => p.number))
    expect(nums(false).sort()).toEqual([1, 2, 3, 4])
    expect(nums(true).sort()).toEqual([2, 3, 4])
    expect(isOthersDraft(prs[0], 'me')).toBe(true)
  })
})

describe('rotting', () => {
  it('counts from the mention in Unseen mentions, else from opening', () => {
    const mention = { at: 500, author: 'ana', url: '', excerpt: '', answered: false, seen: false }
    expect(waitingSince(pr({ createdAt: 100 }))).toBe(100)
    expect(waitingSince(pr({ createdAt: 100, column: 'unseenMentions', mention }))).toBe(500)
    expect(waitingSince(pr({ createdAt: 100, column: 'waitingOnMe', mention: { ...mention, seen: true } }))).toBe(100)
  })
})

describe('actions', () => {
  it('refs and prompt chips', () => {
    expect(prRef(pr({ number: 12 }))).toBe('acme/app#12')
    expect(promptChips(pr({})).map((c) => c.label)).toEqual(['Review', 'Fix CI'])
    expect(promptChips(pr({ column: 'draftMine' })).map((c) => c.label)).toEqual(['Continue', 'Address review', 'Fix CI'])
    expect(promptChips(pr({ column: 'draftMine', checks: 'failure' }))[0].label).toBe('Fix CI')
  })
})

describe('links', () => {
  it('hrefs round-trip', () => {
    const p = pr({ owner: 'acme', repo: 'my.app', number: 7 })
    expect(prHref(p)).toBe('/prs/acme/my.app/7')
    expect(parsePrLocation(prHref(p))).toEqual({ owner: 'acme', repo: 'my.app', number: 7 })
    expect(parsePrLocation('/prs')).toBeNull()
    expect(parsePrLocation('/prs/acme/app/0')).toBeNull()
  })

  it('a session in a PR worktree (or below it) on the PR host links; the deepest worktree wins', () => {
    const main = pr({ number: 1, url: 'u1', local: { repoPath: '/code/main', worktree: '/code/app' } })
    const nested = pr({ number: 2, url: 'u2', local: { repoPath: '/code/app', worktree: '/code/app/.worktrees/feat' } })
    const none = pr({ number: 3, url: 'u3', local: { repoPath: '/code/app', worktree: null } })
    const s = (id: string, cwd: string, host = 'ws') => ({ host, session_id: id, cwd }) as Session
    const sessions = [s('a', '/code/app'), s('b', '/code/app/.worktrees/feat/src'), s('c', '/code/application'), s('d', '/code/app', 'laptop')]
    const { bySession, byPr } = linkSessions([main, nested, none], 'ws', sessions)
    expect([...bySession].map(([k, p]) => [k, p.number])).toEqual([
      ['ws/a', 1],
      ['ws/b', 2],
    ])
    expect(byPr.get('u2')?.map((x) => x.session_id)).toEqual(['b'])
    expect(linkSessions([main], null, sessions).bySession.size).toBe(0)
  })

  it('a session in a linked worktree on a PR branch links, on any host', () => {
    const p = pr({ owner: 'Acme', repo: 'app', number: 4, url: 'u4', headBranch: 'feat-x' })
    const fork = pr({ owner: 'acme', repo: 'app', number: 9, url: 'u9', headBranch: 'main' })
    const git = (branch: string | null, linked = true, repo: string | null = 'acme/app') => ({ branch, root: '/x', linked, repo })
    const s = (id: string, g: Session['git'], host = 'laptop') => ({ host, session_id: id, cwd: '/x', git: g }) as Session
    const sessions = [s('a', git('feat-x')), s('b', git('pr-9'), 'ws'), s('c', git('feat-x', false)), s('d', git('feat-x', true, 'acme/other')), s('e', null), s('f', git(null))]
    const { bySession } = linkSessions([p, fork], 'ws', sessions)
    expect([...bySession].map(([k, x]) => [k, x.number])).toEqual([
      ['laptop/a', 4],
      ['ws/b', 9],
    ])
  })

  it('a main checkout on a PR branch links nothing', () => {
    const p = pr({ url: 'u', local: { repoPath: '/code/app', worktree: '/code/app' } })
    expect(linkSessions([p], 'ws', [{ host: 'ws', session_id: 'a', cwd: '/code/app/src' } as Session]).bySession.size).toBe(0)
  })
})
