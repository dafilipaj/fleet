import { describe, expect, it } from 'vitest'

import { parsePlain } from './plain'
import { createConceptResolver, resolveModule } from './plainConcepts'

const FILES: Record<string, string> = {
  '/r/specs/app.plain': '---\nimport:\n  - core\nrequires:\n  - auth\n---\n\n***definitions***\n\n- :App: is the app.\n\n***functional specs***\n\n- An :App: has a :User: and a :Money: and an :Orphan:.\n',
  '/r/specs/core.plain': '---\nimport:\n  - money\n---\n\n***definitions***\n\n- :Core: is shared.\n',
  '/r/shared/money.plain': '***definitions***\n\n- :Money: is an amount.\n  - In cents.\n> code: money.Money\n',
  '/r/specs/auth.plain': '***definitions***\n\n- :User: is a person who signs in.\n',
  '/r/other/far.plain': '***definitions***\n\n- :Orphan: is defined but never imported.\n',
}

const make = () => {
  const loads: string[] = []
  const r = createConceptResolver({
    path: '/r/specs/app.plain',
    mod: parsePlain(FILES['/r/specs/app.plain']),
    list: async () => Object.keys(FILES),
    load: async (p) => {
      loads.push(p)
      if (!(p in FILES)) throw new Error('404')
      return FILES[p]
    },
  })
  return { r, loads }
}

describe('createConceptResolver', () => {
  it('here, through import / requires (transitively), elsewhere, or not at all', async () => {
    const { r } = make()
    expect(await r.lookup('App')).toMatchObject({ scope: 'here', path: '/r/specs/app.plain' })
    expect(await r.lookup('User')).toMatchObject({ scope: 'imported', path: '/r/specs/auth.plain', via: ['auth'] })
    const money = await r.lookup('Money')
    expect(money).toMatchObject({ scope: 'imported', path: '/r/shared/money.plain', via: ['core', 'money'] })
    expect(money?.clause.children[0].text).toBe('In cents.')
    expect(money?.clause.children[0].notes).toEqual(['code: money.Money'])
    expect(await r.lookup('Orphan')).toMatchObject({ scope: 'elsewhere', path: '/r/other/far.plain', via: [] })
    expect(await r.lookup('Nope')).toBeNull()
  })
  it('reads each module once', async () => {
    const { r, loads } = make()
    await Promise.all([r.lookup('Money'), r.lookup('User'), r.lookup('Nope'), r.lookup('Nope')])
    expect(new Set(loads).size).toBe(loads.length)
  })
})

describe('resolveModule', () => {
  const list = ['/r/a/x.plain', '/r/b/c/x.plain', '/r/b/y.plain']
  it('beside the importer first, else the closest same-named module', () => {
    expect(resolveModule('/r/b/y.plain', 'y', list)).toBe('/r/b/y.plain')
    expect(resolveModule('/r/b/y.plain', 'x', list)).toBe('/r/b/c/x.plain')
    expect(resolveModule('/r/b/y.plain', 'z', list)).toBe('/r/b/z.plain')
    expect(resolveModule('/r/b/y.plain', 'x', null)).toBe('/r/b/x.plain')
  })
})
