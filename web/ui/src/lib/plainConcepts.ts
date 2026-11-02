/**
 * Where a PlainLang concept (`:Widget:`) is defined, across modules: the module itself, then the
 * modules it imports / requires (transitively, nearest first — what the language makes visible),
 * then any other module of the checkout (a concept the module uses without importing it).
 * Loading is injected (the file API in the app, fixtures in tests); every module is read once.
 */
import { parsePlain, type Clause, type PlainModule } from '@/lib/plain'

export type ConceptScope = 'here' | 'imported' | 'elsewhere'

export interface ConceptHit {
  name: string
  /** the definition clause (with its sub-clauses and notes) */
  clause: Clause
  /** the defining module's path, and the module */
  path: string
  mod: PlainModule
  scope: ConceptScope
  /** the import / requires chain from the module to the defining one (`['core', 'money']`) */
  via: string[]
}

export interface ConceptResolver {
  lookup: (name: string) => Promise<ConceptHit | null>
}

export interface ResolverSource {
  /** the module being read, and where it lives (absolute) */
  path: string
  mod: PlainModule
  /** every .plain module path of its checkout (absolute); null when unknown */
  list: () => Promise<string[] | null>
  /** a module's text */
  load: (path: string) => Promise<string>
}

const dirOf = (p: string) => p.slice(0, Math.max(0, p.lastIndexOf('/')))

/** The definition of `name` among a module's definitions, or null. */
export function definitionIn(mod: PlainModule, name: string): Clause | null {
  const head = `:${name}:`
  return (mod.sections['definitions'] ?? []).find((c) => c.text.startsWith(head) && c.state !== 'removed') ?? null
}

/** A module name from `import:` / `requires:` → its path: beside the importer, else the closest same-named module. */
export function resolveModule(from: string, name: string, list: string[] | null): string {
  const beside = `${dirOf(from)}/${name.replace(/\.plain$/, '')}.plain`
  if (!list || list.includes(beside)) return beside
  const tail = `/${name.replace(/\.plain$/, '')}.plain`
  const hits = list.filter((p) => p.endsWith(tail))
  if (!hits.length) return beside
  const common = (a: string, b: string) => {
    let i = 0
    while (i < a.length && a[i] === b[i]) i++
    return i
  }
  return hits.sort((a, b) => common(b, from) - common(a, from) || a.length - b.length)[0]
}

export function createConceptResolver(src: ResolverSource, { scanBatch = 8 } = {}): ConceptResolver {
  const mods = new Map<string, Promise<PlainModule | null>>([[src.path, Promise.resolve(src.mod)]])
  const read = (p: string) => {
    let m = mods.get(p)
    if (!m) {
      m = src.load(p).then(
        (t) => parsePlain(t),
        () => null,
      )
      mods.set(p, m)
    }
    return m
  }
  let listP: Promise<string[] | null> | null = null
  const list = () => (listP ??= src.list().catch(() => null))
  const memo = new Map<string, Promise<ConceptHit | null>>()

  async function find(name: string): Promise<ConceptHit | null> {
    const here = definitionIn(src.mod, name)
    if (here) return { name, clause: here, path: src.path, mod: src.mod, scope: 'here', via: [] }
    // Breadth-first through import + requires: the nearest definition wins.
    const all = await list()
    const seen = new Set([src.path])
    let level: { path: string; mod: PlainModule; via: string[] }[] = [{ path: src.path, mod: src.mod, via: [] }]
    while (level.length) {
      const next: { path: string; via: string[] }[] = []
      for (const { path, mod, via } of level)
        for (const dep of [...mod.import, ...mod.requires]) {
          const p = resolveModule(path, dep, all)
          if (seen.has(p)) continue
          seen.add(p)
          next.push({ path: p, via: [...via, dep] })
        }
      const loaded = await Promise.all(next.map(async (n) => ({ ...n, mod: await read(n.path) })))
      level = []
      for (const { path, mod, via } of loaded) {
        if (!mod) continue
        const c = definitionIn(mod, name)
        if (c) return { name, clause: c, path, mod, scope: 'imported', via }
        level.push({ path, mod, via })
      }
    }
    // Anywhere else in the checkout.
    const rest = (all ?? []).filter((p) => !seen.has(p))
    for (let i = 0; i < rest.length; i += scanBatch) {
      const batch = rest.slice(i, i + scanBatch)
      const loaded = await Promise.all(batch.map(async (p) => ({ p, mod: await read(p) })))
      for (const { p, mod } of loaded) {
        const c = mod && definitionIn(mod, name)
        if (c) return { name, clause: c, path: p, mod: mod!, scope: 'elsewhere', via: [] }
      }
    }
    return null
  }

  return {
    lookup: (name) => {
      let hit = memo.get(name)
      if (!hit) {
        hit = find(name)
        memo.set(name, hit)
      }
      return hit
    },
  }
}

/** Only the module itself (no file access). */
export const localResolver = (mod: PlainModule, path = ''): ConceptResolver => ({
  lookup: async (name) => {
    const c = definitionIn(mod, name)
    return c ? { name, clause: c, path, mod, scope: 'here', via: [] } : null
  },
})
