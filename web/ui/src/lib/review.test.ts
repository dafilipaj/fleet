import { describe, expect, it } from 'vitest'

import type { DiffFile, DiffHunk, DiffRepo, ReviewThread } from '@/api/types'

import { diffPlain } from './plain'
import { changeBlocks, descriptionExcerpt, githubPr, parsePrReviewLocation, prReviewHref, fileKind, kindCounts, allClauses, clauseAt, placeOnClauses, diffBar, fileKey, rowsToRange, fileSignature, fileTree, orphanThreads, orphanThreadsByKey, repoViews, threadKey, repoKey, entrySections, entryBucket, parseReviewLocation, placeThreads, reviewHref, selectLine, selectionCode, splitRows, treeOrder } from './review'

const file = (path: string, o: Partial<DiffFile> = {}): DiffFile => ({
  path,
  oldPath: path,
  status: 'modified',
  binary: false,
  additions: 1,
  deletions: 0,
  truncated: false,
  hunks: [],
  ...o,
})

const hunk = (lines: DiffHunk['lines']): DiffHunk => ({ header: '@@ -1 +1 @@', oldStart: 1, oldLines: 1, newStart: 1, newLines: 1, section: '', lines })

describe('review routes', () => {
  it('round-trips host and id', () => {
    const href = reviewHref('work station', 'abc-123')
    expect(href).toBe('/review/work%20station/abc-123')
    expect(parseReviewLocation(href)).toEqual({ host: 'work station', id: 'abc-123' })
    expect(parseReviewLocation('/s/a/b')).toBeNull()
    expect(parseReviewLocation('/review/a')).toBeNull()
  })
  it('round-trips a review scoped to one checkout', () => {
    const href = reviewHref('ws', 'abc', '/home/u/Code/app-fix')
    expect(href).toBe('/review/ws/abc/root/%2Fhome%2Fu%2FCode%2Fapp-fix')
    expect(parseReviewLocation(href)).toEqual({ host: 'ws', id: 'abc', root: '/home/u/Code/app-fix' })
    expect(parseReviewLocation(reviewHref('ws', 'abc', null))).toEqual({ host: 'ws', id: 'abc' })
  })
})

describe('fileTree', () => {
  it('folders first, single-child chains merged, the diff list follows the tree', () => {
    const tree = fileTree([file('src/lib/a.ts'), file('README.md'), file('src/lib/b.ts'), file('docs/x/y/z.md'), file('src/main.ts')])
    expect(tree.map((n) => n.name)).toEqual(['docs/x/y', 'src', 'README.md'])
    const src = tree[1]
    expect(src.kind === 'dir' && src.children.map((n) => n.name)).toEqual(['lib', 'main.ts'])
    expect(treeOrder(tree).map((f) => f.path)).toEqual(['docs/x/y/z.md', 'src/lib/a.ts', 'src/lib/b.ts', 'src/main.ts', 'README.md'])
  })
})

describe('splitRows', () => {
  it('pairs a removal run with the additions after it; context on both sides', () => {
    const rows = splitRows(
      hunk([
        { t: ' ', o: 1, n: 1, text: 'a' },
        { t: '-', o: 2, n: null, text: 'b' },
        { t: '-', o: 3, n: null, text: 'c' },
        { t: '+', o: null, n: 2, text: 'B' },
        { t: ' ', o: 4, n: 3, text: 'd' },
        { t: '+', o: null, n: 4, text: 'e' },
      ]),
    )
    expect(rows.map((r) => [r.left?.text ?? null, r.right?.text ?? null])).toEqual([
      ['a', 'a'],
      ['b', 'B'],
      ['c', null],
      ['d', 'd'],
      [null, 'e'],
    ])
  })
})

describe('fileSignature / diffBar', () => {
  it('changes with the content only', () => {
    const a = file('x', { hunks: [hunk([{ t: '+', o: null, n: 1, text: 'x' }])] })
    expect(fileSignature(a)).toBe(fileSignature({ ...a }))
    expect(fileSignature(a)).not.toBe(fileSignature(file('x', { hunks: [hunk([{ t: '+', o: null, n: 1, text: 'y' }])] })))
  })
  it('splits five squares', () => {
    expect(diffBar(0, 0)).toBe(0)
    expect(diffBar(6, 4)).toBe(3)
    expect(diffBar(1, 0)).toBe(5)
  })
})

const thread = (o: Partial<ReviewThread>): ReviewThread => ({
  id: 'c-000001',
  path: 'a.ts',
  side: 'new',
  start: 2,
  end: 2,
  code: ['B'],
  base: null,
  createdAt: 0,
  status: 'sent',
  resolved: false,
  reviewId: null,
  entries: [],
  ...o,
})

describe('line selection and threads', () => {
  const f = file('a.ts', {
    hunks: [
      hunk([
        { t: ' ', o: 1, n: 1, text: 'a' },
        { t: '-', o: 2, n: null, text: 'b' },
        { t: '+', o: null, n: 2, text: 'B' },
        { t: '+', o: null, n: 3, text: 'C' },
        { t: ' ', o: 3, n: 4, text: 'd' },
      ]),
    ],
  })
  it('shift extends on the same file and side only', () => {
    const one = selectLine(null, 'a.ts', 'new', 3, false)
    expect(selectLine(one, 'a.ts', 'new', 1, true)).toEqual({ path: 'a.ts', side: 'new', start: 1, end: 3 })
    expect(selectLine(one, 'a.ts', 'old', 1, true)).toEqual({ path: 'a.ts', side: 'old', start: 1, end: 1 })
  })
  it('code of a selection skips the other side', () => {
    expect(selectionCode(f, { path: 'a.ts', side: 'new', start: 1, end: 4 })).toEqual(['a', 'B', 'C', 'd'])
    expect(selectionCode(f, { path: 'a.ts', side: 'old', start: 1, end: 3 })).toEqual(['a', 'b', 'd'])
  })
  it('places threads after their last line; moved or changed lines are outdated', () => {
    const ok = thread({})
    const changed = thread({ id: 'c-000002', code: ['old text'] })
    const gone = thread({ id: 'c-000003', end: 40, start: 40 })
    const other = thread({ id: 'c-000004', path: 'b.ts' })
    const p = placeThreads(f, [ok, changed, gone, other])
    expect(p.at.get('new:2')?.map((t) => t.id)).toEqual(['c-000001', 'c-000002'])
    expect(p.unplaced.map((t) => t.id)).toEqual(['c-000003'])
    expect([...p.outdated].sort()).toEqual(['c-000002', 'c-000003'])
    expect(orphanThreads([f], [ok, other]).map((t) => t.id)).toEqual(['c-000004'])
  })
})

describe('several checkouts', () => {
  it('repoViews keeps checkouts with changes or errors, keys files by root + path', () => {
    const views = repoViews([
      { root: '/w', label: '~/w', files: [] },
      { root: '/w/wt/a', label: 'wt/a', files: [file('x.ts'), file('src/y.ts')] },
      { root: '/w/wt/gone', label: 'wt/gone', error: 'not a git repository' },
    ])
    expect(views.map((v) => v.repo.label)).toEqual(['wt/a', 'wt/gone'])
    expect(views[0].files.map((f) => f.key)).toEqual([fileKey('/w/wt/a', 'src/y.ts'), fileKey('/w/wt/a', 'x.ts')])
    expect(fileKey('/w/wt/a', 'x.ts')).not.toBe(fileKey('/w/wt/b', 'x.ts'))
  })
  it('a thread without a root belongs to the default checkout; orphans by key', () => {
    const legacy = thread({ root: null, path: 'a.ts' })
    const other = thread({ id: 'c-000002', root: '/w/wt/b', path: 'a.ts' })
    expect(threadKey(legacy, '/w')).toBe(fileKey('/w', 'a.ts'))
    expect(orphanThreadsByKey(new Set([fileKey('/w', 'a.ts')]), [legacy, other], '/w').map((t) => t.id)).toEqual(['c-000002'])
  })
})

describe('rowsToRange', () => {
  it('unified: the new side unless only removed lines are selected', () => {
    const rows = [
      { o: 4, n: 4, t: ' ' },
      { o: 5, n: null, t: '-' },
      { o: null, n: 5, t: '+' },
      { o: null, n: 6, t: '+' },
    ]
    expect(rowsToRange(rows)).toEqual({ side: 'new', start: 4, end: 6 })
    expect(rowsToRange([rows[1]])).toEqual({ side: 'old', start: 5, end: 5 })
    expect(rowsToRange([])).toBeNull()
  })
  it('split: the side the selection ends on, other-side rows ignored', () => {
    const rows = [
      { side: 'old' as const, o: 7, n: 7, t: ' ' },
      { side: 'new' as const, o: 7, n: 7, t: ' ' },
      { side: 'old' as const, o: 8, n: null, t: '-' },
      { side: 'new' as const, o: null, n: 8, t: '+' },
    ]
    expect(rowsToRange(rows, 'new')).toEqual({ side: 'new', start: 7, end: 8 })
    expect(rowsToRange(rows, 'old')).toEqual({ side: 'old', start: 7, end: 8 })
  })
})

describe('a .plain spec: threads on clauses', () => {
  const oldText = '***functional specs***\n\n- Alpha one.\n- Beta two\n  continued.\n'
  const newText = '***functional specs***\n\n- Alpha one.\n- Beta two\n  continued.\n- Gamma three.\n'
  const file: DiffFile = {
    path: 's.plain',
    oldPath: 's.plain',
    status: 'modified',
    binary: false,
    additions: 1,
    deletions: 0,
    truncated: false,
    hunks: [{ header: '@@ -5 +5,2 @@', oldStart: 5, oldLines: 1, newStart: 5, newLines: 2, section: '', lines: [{ t: ' ', o: 5, n: 5, text: '  continued.' }, { t: '+', o: null, n: 6, text: '- Gamma three.' }] }],
    plain: { old: oldText, new: newText },
  } as DiffFile
  const thread = (id: string, start: number, end: number, code: string[]): ReviewThread => ({ id, path: 's.plain', side: 'new', start, end, code, base: null, createdAt: 0, status: 'draft', resolved: false, reviewId: null, entries: [] })
  const clauses = allClauses(diffPlain(oldText, newText))

  it('the whole text answers for lines outside the hunks', () => {
    expect(selectionCode(file, { path: 's.plain', side: 'new', start: 3, end: 3 })).toEqual(['- Alpha one.'])
  })
  it('a thread goes under the clause holding its last line', () => {
    expect(clauseAt(clauses, 'new', 5)).toBe('new:4')
    const p = placeOnClauses(file, clauses, [thread('a', 4, 5, ['- Beta two', '  continued.']), thread('b', 3, 3, ['- Alpha zero.']), thread('c', 1, 1, ['***functional specs***'])])
    expect([...p.at.keys()]).toEqual(['new:4', 'new:3'])
    expect(p.unplaced.map((t) => t.id)).toEqual(['c'])
    expect([...p.outdated]).toEqual(['b'])
  })
  it('the diff view keeps a thread outside the hunks current while its lines read the same', () => {
    const p = placeThreads(file, [thread('a', 3, 3, ['- Alpha one.'])])
    expect(p.unplaced.map((t) => t.id)).toEqual(['a'])
    expect(p.outdated.size).toBe(0)
  })
})

describe('fileKind', () => {
  it('sorts changed files into specs, docs, tests and code', () => {
    expect(fileKind('specs/review.plain')).toBe('spec')
    expect(fileKind('README.md')).toBe('docs')
    expect(fileKind('docs/guide.mdx')).toBe('docs')
    expect(fileKind('src/lib/review.ts')).toBe('code')
    expect(fileKind('app/models/user.rb')).toBe('code')
    expect(fileKind('Cargo.toml')).toBe('code')
    for (const t of ['src/lib/review.test.ts', 'web/tests/api.test.mjs', 'spec/models/user_spec.rb', 'pkg/diff_test.go', 'tests/test_api.py', 'src/__tests__/x.tsx', 'crates/fleet/tests/cli.rs', 'e2e/login.spec.ts', 'tests/fixtures/README.md'])
      expect(fileKind(t), t).toBe('tests')
  })

  it('counts kinds across checkouts', () => {
    const f = (path: string) => ({ path, oldPath: null, status: 'modified' as const, additions: 1, deletions: 0, hunks: [] })
    expect(kindCounts([{ root: '/a', label: 'a', files: [f('a.plain'), f('x.ts')] }, { root: '/b', label: 'b', files: [f('x.test.ts'), f('y.md')] }] as never)).toEqual({ spec: 1, docs: 1, code: 1, tests: 1 })
  })
})

describe('PR review locations', () => {
  it('round-trips host, owner, repo and number; a session review is not one', () => {
    const href = prReviewHref('work station', { owner: 'acme', repo: 'api.js', number: 12 })
    expect(href).toBe('/review/pr/work%20station/acme/api.js/12')
    expect(parsePrReviewLocation(href)).toEqual({ host: 'work station', owner: 'acme', repo: 'api.js', number: 12 })
    expect(parsePrReviewLocation('/review/laptop/abc')).toBeNull()
    expect(parsePrReviewLocation('/review/pr/h/o/r/x')).toBeNull()
    const fromSession = prReviewHref('ws', { owner: 'acme', repo: 'api', number: 3 }, { host: 'work station', id: 'abc-1' })
    expect(fromSession).toBe('/review/pr/ws/acme/api/3/from/work%20station/abc-1')
    expect(parsePrReviewLocation(fromSession)).toEqual({ host: 'ws', owner: 'acme', repo: 'api', number: 3, from: { host: 'work station', id: 'abc-1' } })
  })
})

describe('review entries: worktrees and PRs', () => {
  const pr = (number: number, state: 'open' | 'draft' | 'merged' | 'closed') => ({ owner: 'acme', repo: 'api', number, title: `t${number}`, url: '', state, headBranch: `b${number}`, baseBranch: 'main', headOid: null })
  const view = (repo: Partial<DiffRepo>) => ({ repo: { root: '/r', label: 'r', ...repo } as DiffRepo, tree: [], files: [] })

  it('repoKey: a PR entry by its key, a checkout by its root', () => {
    expect(repoKey({ root: '/clone', key: 'pr:acme/api#3' })).toBe('pr:acme/api#3')
    expect(repoKey({ root: '/wt' })).toBe('/wt')
  })

  it('threadKey: a comment on a PR without a worktree lands on the PR entry, not the clone', () => {
    expect(threadKey({ root: '/clone', path: 'a.rb', pr: 'acme/API#3' }, '/cwd')).toBe(fileKey('pr:acme/api#3', 'a.rb'))
    expect(threadKey({ root: '/clone', path: 'a.rb' }, '/cwd')).toBe(fileKey('/clone', 'a.rb'))
  })

  it('entrySections: PRs first, then merged / closed, then without a PR, each grouped by repository', () => {
    const s = entrySections([
      view({ root: '/home', label: '.', remote: 'me/home', pr: null }),
      view({ root: '/wt1', remote: 'acme/api', pr: pr(1, 'open') }),
      view({ root: '/fl', remote: 'me/fleet', pr: pr(2, 'merged') }),
      view({ root: '', key: 'pr:acme/api#4', source: 'pr', remote: 'acme/api', pr: pr(4, 'draft') }),
      view({ root: '/x', label: 'x', remote: null }),
    ])
    expect(s.map((x) => x.bucket)).toEqual(['pr', 'done', 'local'])
    expect(s[0].groups.map((g) => [g.name, g.views.length])).toEqual([['acme/api', 2]])
    expect(s[2].groups.map((g) => g.name)).toEqual(['me/home', 'x'])
    expect(entryBucket(s[1].groups[0].views[0].repo)).toBe('done')
  })

  it('entrySections: the same order whatever order the entries come in', () => {
    const vs = [
      view({ root: '/b', remote: 'acme/web', pr: pr(9, 'open') }),
      view({ root: '/a', remote: 'acme/api', pr: pr(7, 'open') }),
      view({ root: '', key: 'pr:acme/api#3', source: 'pr', remote: 'acme/api', pr: pr(3, 'draft') }),
    ]
    const order = (list: typeof vs) => entrySections(list).flatMap((x) => x.groups.flatMap((g) => g.views.map((v) => v.repo.pr?.number)))
    expect(order(vs)).toEqual([3, 7, 9])
    expect(order([...vs].reverse())).toEqual([3, 7, 9])
  })
})

describe('githubPr', () => {
  it('reads a GitHub PR link', () => {
    expect(githubPr('https://github.com/acme/api.js/pull/12')).toEqual({ owner: 'acme', repo: 'api.js', number: 12 })
    expect(githubPr('https://github.com/acme/api/pull/12/files#diff')).toEqual({ owner: 'acme', repo: 'api', number: 12 })
  })
  it('is null for anything else', () => {
    expect(githubPr('https://github.com/acme/api/issues/12')).toBeNull()
    expect(githubPr('https://gitlab.com/acme/api/pull/12')).toBeNull()
    expect(githubPr('https://github.com/acme/api/pull/12x')).toBeNull()
  })
})

describe('changeBlocks', () => {
  it('groups runs of changed lines; a run with both kinds is ±', () => {
    const l = (t: ' ' | '+' | '-') => ({ t, o: null, n: null, text: '' })
    const f = { hunks: [hunk([l(' '), l('-'), l('+'), l(' '), l(' '), l('+'), l('+'), l(' '), l('-')])] }
    expect(changeBlocks(f)).toEqual([
      { start: 1, end: 2, kind: '±' },
      { start: 5, end: 6, kind: '+' },
      { start: 8, end: 8, kind: '-' },
    ])
    expect(changeBlocks({ hunks: [] })).toEqual([])
  })
})

describe('descriptionExcerpt', () => {
  it('keeps the text and drops markdown marks, links, images, code and comments', () => {
    const body = '<!-- template -->\n## Why\n\nThe **review** shows [links](https://x.y) too.\n\n![shot](https://x.y/a.png)\n```sh\nrm -rf\n```\n- one\n- `two`'
    expect(descriptionExcerpt(body)).toBe('Why The review shows links too. one two')
  })
  it('is empty for an empty description', () => {
    expect(descriptionExcerpt('  \n ')).toBe('')
  })
})
