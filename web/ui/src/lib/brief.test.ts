import { describe, expect, test } from 'vitest'

import type { BriefResource } from '@/api/types'

import { briefBody, briefTime, briefTodos, CONTINUE_DEFAULT_TASK, continueDraft, editorLabel, gitLine, groupResources, retryMinutes, setTodoItem, todoProgress, withContinueTask } from './brief'

const md = `---
session: 0000-aaaa
updated: 2026-01-02T03:04:05.000Z
---
## Summary
Fixing the login loop; see - [ ] not a todo here.

## Resources
- Branch: \`fix-login\`
- [ ] odd resource line

## Todos
- [x] reproduce the loop
* [ ] fix the redirect (in progress)
text between
- [X] write a test

## Notes
- [ ] not in the todos either
`

describe('briefBody', () => {
  test('drops the frontmatter, keeps the body', () => {
    expect(briefBody(md).startsWith('## Summary\n')).toBe(true)
    expect(briefBody('## Summary\nx\n')).toBe('## Summary\nx\n')
    expect(briefBody(null)).toBe('')
  })
})

describe('setTodoItem', () => {
  test('toggles the nth checkbox of ## Todos only', () => {
    const out = setTodoItem(md, 1, true)!
    expect(out).toContain('* [x] fix the redirect (in progress)')
    // Everything else is untouched: other sections' checkboxes, other todos, free text.
    expect(out).toContain('- [ ] odd resource line')
    expect(out).toContain('- [ ] not in the todos either')
    expect(out).toContain('- [x] reproduce the loop')
    expect(out).toContain('text between')
    expect(out).not.toContain('session: 0000-aaaa')
  })

  test('unchecks (also an upper-case X) and indexes past free text', () => {
    expect(setTodoItem(md, 2, false)).toContain('- [ ] write a test')
    expect(setTodoItem(md, 0, false)).toContain('- [ ] reproduce the loop')
  })

  test('headings are case-insensitive; a missing item is null', () => {
    expect(setTodoItem('## todos\n- [ ] a\n', 0, true)).toBe('## todos\n- [x] a\n')
    expect(setTodoItem(md, 3, true)).toBeNull()
    expect(setTodoItem('## Summary\n- [ ] a\n', 0, true)).toBeNull()
  })

  test('a legacy ## Plan section counts as Todos, in file order', () => {
    expect(setTodoItem('## Plan\n- [ ] a\n', 0, true)).toBe('## Plan\n- [x] a\n')
    const both = '## Todos\n- [ ] a\n\n## Notes\n- [ ] n\n\n## Plan\n- [ ] b\n'
    expect(setTodoItem(both, 1, true)).toBe('## Todos\n- [ ] a\n\n## Notes\n- [ ] n\n\n## Plan\n- [x] b\n')
    expect(setTodoItem(both, 2, true)).toBeNull()
  })

  test('toggling twice builds on the previous result', () => {
    const once = setTodoItem(md, 1, true)!
    const twice = setTodoItem(once, 0, false)!
    expect(twice).toContain('- [ ] reproduce the loop')
    expect(twice).toContain('* [x] fix the redirect')
  })
})

const res = (kind: string | null, label: string): BriefResource => ({ kind, label, url: null, path: label, text: label })

test('briefTodos prefers todos, falls back to the deprecated plan', () => {
  const a = [{ done: true, text: 'a' }]
  const b = [{ done: false, text: 'b' }]
  expect(briefTodos({ todos: a, plan: b })).toEqual(a)
  expect(briefTodos({ plan: b })).toEqual(b)
  expect(briefTodos(null)).toEqual([])
})

describe('groupResources', () => {
  test('groups by kind in display order, notes last, order kept inside a group', () => {
    const groups = groupResources([
      res('Link', 'l1'),
      res(null, 'note'),
      res('File', 'f1'),
      res('PR', 'p1'),
      res('Future', 'x'),
      res('File', 'f2'),
      res('Spec', 's1'),
      res('Artifact', 'a1'),
      res('Worktree', 'w1'),
      res('Git', 'g1'),
      res('Issue', 'i1'),
    ])
    expect(groups.map((g) => [g.kind, g.label, g.items.map((i) => i.label)])).toEqual([
      ['Git', 'Git', ['g1']],
      ['Worktree', 'Worktree', ['w1']],
      ['PR', 'Pull requests', ['p1']],
      ['Issue', 'Issues', ['i1']],
      ['Artifact', 'Artifacts', ['a1']],
      ['Spec', 'Specs', ['s1']],
      ['Link', 'Links', ['l1']],
      ['File', 'Files', ['f1', 'f2']],
      ['Future', 'Future', ['x']],
      [null, 'Notes', ['note']],
    ])
  })

  test('nothing → no groups', () => {
    expect(groupResources([])).toEqual([])
    expect(groupResources(undefined)).toEqual([])
  })
})

test('todoProgress', () => {
  expect(todoProgress([{ done: true }, { done: false }, { done: true }])).toEqual({ done: 2, total: 3 })
  expect(todoProgress([])).toBeNull()
  expect(todoProgress(null)).toBeNull()
})

test('gitLine: branch + worktree / repo', () => {
  expect(gitLine({ branch: 'fix-login', linked: true, path: '~/Code/project-wt' })).toEqual({ branch: 'fix-login', where: 'worktree ~/Code/project-wt' })
  expect(gitLine({ branch: 'main', linked: false, path: '~/Code/project' })).toEqual({ branch: 'main', where: 'repo ~/Code/project' })
  expect(gitLine({ branch: null, linked: null, path: '~/Code/project' })).toEqual({ branch: null, where: '~/Code/project' })
  expect(gitLine({ path: null })).toEqual({ branch: null, where: null })
})

test('editorLabel', () => {
  expect(editorLabel('vscode', 'vscode://file/x')).toBe('Open in VS Code')
  expect(editorLabel('cursor', 'cursor://file/x')).toBe('Open in Cursor')
  expect(editorLabel(undefined, 'cursor://file/x')).toBe('Open in Cursor')
  expect(editorLabel(undefined, 'vscode://vscode-remote/ssh-remote+workstation/x')).toBe('Open in VS Code')
  expect(editorLabel('vscode', null)).toBeNull()
})

test('continueDraft ends with a blank line for the caret', () => {
  expect(continueDraft('Continue the work.\n')).toBe('Continue the work.\n\n')
  expect(continueDraft('')).toBe('')
})

test('retryMinutes rounds up, at least 1', () => {
  expect(retryMinutes(61_000)).toBe(2)
  expect(retryMinutes(1)).toBe(1)
  expect(retryMinutes(undefined)).toBe(1)
})

test('briefTime', () => {
  expect(briefTime('2026-01-02T03:04:05.000Z')).toBe(Date.UTC(2026, 0, 2, 3, 4, 5))
  expect(briefTime(null)).toBeNull()
  expect(briefTime('nope')).toBeNull()
})

test('withContinueTask: the default task only when nothing follows the separator', () => {
  const p = 'Continue the work of session s on laptop. Its brief:\n\nDid x.\n\n---'
  expect(withContinueTask(`${p}\n\n`)).toBe(`${p}\n\n${CONTINUE_DEFAULT_TASK}`)
  expect(withContinueTask(`${p}\n\nReview the PR.`)).toBe(`${p}\n\nReview the PR.`)
  expect(withContinueTask('a plain prompt\n---')).toBe('a plain prompt\n---')
})
