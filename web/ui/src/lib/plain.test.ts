import { describe, expect, it } from 'vitest'

import { diffPlain, flatClauses, gist, inlineParts, parsePlain, wordDiff } from './plain'

const MOD = `---
description: Widgets — how they are made.
import:
  - core
requires:
  - service
exported_concepts: [":Widget:"]
---

> Root module.

***definitions***

- :Widget: is a thing.
  - It has a name.
> code: widgets.model

- :Gadget: is another thing.

***implementation reqs***

- Use the queue.

***functional specs***

- A :Widget: is made on request.
  - Its name is unique; a duplicate answers 409.

  ***acceptance tests***
  - Making two widgets named "a" answers 409 the second time.

- A :Gadget: is listed by name,
  then by date.
`

describe('parsePlain', () => {
  const m = parsePlain(MOD)
  it('reads the frontmatter and notes', () => {
    expect(m.description).toBe('Widgets — how they are made.')
    expect(m.import).toEqual(['core'])
    expect(m.requires).toEqual(['service'])
    expect(m.exported).toEqual(['Widget'])
    expect(m.notes).toEqual([{ text: 'Root module.', line: 10 }])
  })
  it('keeps each clause with its lines and notes', () => {
    const defs = m.sections['definitions']
    expect(defs.map((c) => c.text)).toEqual([':Widget: is a thing.', ':Gadget: is another thing.'])
    expect(defs[0].children[0]).toMatchObject({ text: 'It has a name.', start: 15, end: 16, notes: ['code: widgets.model'] })
    expect(defs[0]).toMatchObject({ start: 14, end: 14 })
    expect(m.sections['implementation reqs'].map((c) => c.text)).toEqual(['Use the queue.'])
  })
  it('numbers specs by position, with their acceptance tests', () => {
    expect(m.specs.map((s) => s.id)).toEqual(['#1', '#2'])
    expect(m.specs[0].clauses[0].children[0].text).toBe('Its name is unique; a duplicate answers 409.')
    expect(m.specs[0].tests.map((c) => c.text)).toEqual(['Making two widgets named "a" answers 409 the second time.'])
    expect(m.specs[1].clauses[0]).toMatchObject({ text: 'A :Gadget: is listed by name, then by date.', start: 32, end: 33 })
    expect(m.specs[0]).toMatchObject({ start: 26, end: 30 })
  })
  it('keeps explicit spec ids', () => {
    const e = parsePlain('***functional specs***\n\n> spec: §1.2\n- One.\n- Two.\n')
    expect(e.explicitIds).toBe(true)
    expect(e.specs.map((s) => [s.id, s.clauses.length])).toEqual([['§1.2', 2]])
  })
  it('reads a multi-line description and inline lists', () => {
    const e = parsePlain('---\ndescription: >\n  One\n  two.\nimport: [a, "b"]\n---\n')
    expect(e.description).toBe('One two.')
    expect(e.import).toEqual(['a', 'b'])
  })
})

describe('diffPlain', () => {
  it('a new file: every clause is added', () => {
    const d = diffPlain(null, MOD)
    expect(d.mod.specs.every((s) => s.state === 'added')).toBe(true)
    expect(d.stats.removed).toBe(0)
    expect(d.stats.added).toBeGreaterThan(5)
  })
  it('the same text: nothing changes', () => {
    const d = diffPlain(MOD, MOD)
    expect(d.stats).toEqual({ added: 0, changed: 0, removed: 0 })
    expect(d.touched).toEqual([])
  })
  it('marks changed, added and removed clauses in place', () => {
    const next = MOD.replace('a duplicate answers 409', 'a duplicate answers 422')
      .replace('- Use the queue.', '- Use the queue.\n- Retry three times.')
      .replace('- :Gadget: is another thing.\n', '')
    const d = diffPlain(MOD, next)
    const s1 = d.mod.specs[0]
    expect(s1.state).toBe('changed')
    expect(s1.clauses[0].children[0]).toMatchObject({ state: 'changed', oldText: 'Its name is unique; a duplicate answers 409.' })
    expect(d.mod.sections['implementation reqs'].map((c) => [c.text, c.state])).toEqual([
      ['Use the queue.', 'same'],
      ['Retry three times.', 'added'],
    ])
    const defs = d.mod.sections['definitions']
    expect(defs.map((c) => [c.text, c.state])).toEqual([
      [':Widget: is a thing.', 'same'],
      [':Gadget: is another thing.', 'removed'],
    ])
    expect(defs[1].side).toBe('old')
    expect(d.changedTerms).toEqual(new Set(['Gadget']))
    // #2 did not change, but the definition it uses did.
    expect(d.mod.specs[1]).toMatchObject({ state: 'same', affectedBy: ['Gadget'] })
    expect(d.touched.map((t) => t.where)).toEqual(['definitions', 'implementation reqs', '#1'])
  })
  it('a spec removed between two goes back where it stood; ids do not shift the pairing', () => {
    const three = '***functional specs***\n\n- Alpha one two.\n- Beta three four.\n- Gamma five six.\n'
    const d = diffPlain(three, '***functional specs***\n\n- Alpha one two.\n- Gamma five six.\n')
    expect(d.mod.specs.map((s) => [s.id, s.state])).toEqual([
      ['#1', 'same'],
      ['old #2', 'removed'],
      ['#2', 'same'],
    ])
    expect(d.touched).toEqual([{ where: 'old #2', added: 0, changed: 0, removed: 1, removedSpec: true }])
  })
  it('a changed description', () => {
    const d = diffPlain(MOD, MOD.replace('how they are made', 'how they are built'))
    expect(d.oldDescription).toBe('Widgets — how they are made.')
  })
})

describe('helpers', () => {
  it('wordDiff', () => {
    expect(wordDiff('a b c', 'a x c')).toEqual([
      { k: 'eq', t: 'a' },
      { k: 'del', t: 'b' },
      { k: 'ins', t: 'x' },
      { k: 'eq', t: 'c' },
    ])
  })
  it('inlineParts', () => {
    expect(inlineParts('A :Widget: per `x:y:` see §1.2.')).toEqual([
      { k: 'text', t: 'A ' },
      { k: 'term', name: 'Widget' },
      { k: 'text', t: ' per ' },
      { k: 'code', t: 'x:y:' },
      { k: 'text', t: ' see ' },
      { k: 'ref', id: '§1.2' },
      { k: 'text', t: '.' },
    ])
  })
  it('gist', () => {
    expect(gist(parsePlain(MOD).specs[0])).toBe('A Widget is made on request')
  })
  it('flatClauses walks depth-first', () => {
    expect(flatClauses(parsePlain(MOD).specs[0].clauses).map((x) => x.depth)).toEqual([0, 1])
  })
})
