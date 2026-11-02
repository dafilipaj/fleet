/**
 * PlainLang (.plain) specs: a parser that keeps every clause's lines, and a diff by clauses
 * (not lines) — what the file preview's Rendered view and the review's Spec view draw.
 *
 * A module: optional `---` frontmatter (description, import, requires, exported_concepts …),
 * `>` notes before the first section, then `***definitions***`, `***implementation reqs***`,
 * `***test reqs***` and `***functional specs***`. Each top-level bullet of functional specs is
 * one spec (numbered #1, #2 … by position) unless the module marks its specs with `> spec: §x`;
 * a spec's `***acceptance tests***` go with it. A `> …` line inside a section (`> code: …`)
 * belongs to the clause above it.
 *
 * The diff: clauses pair by text, near
 * matches pair as changes (word diff), removed clauses come back where they stood.
 */

export type ClauseState = 'same' | 'added' | 'changed' | 'removed'
export type Side = 'new' | 'old'

export interface Clause {
  text: string
  /** `> …` lines under it (`code: x.y`) */
  notes: string[]
  children: Clause[]
  state: ClauseState
  /** a changed clause's text before */
  oldText?: string
  /** its own lines (bullet, continuations, notes — not its children), 1-based, in `side`'s file */
  start: number
  end: number
  side: Side
}

export interface Spec {
  /** `§12.3` when the module marks its specs, else `#n` by position (`old #n` for a removed one) */
  id: string
  clauses: Clause[]
  tests: Clause[]
  state: ClauseState
  /** definitions it uses whose clause changed, though its own text did not */
  affectedBy: string[]
  start: number
  end: number
  side: Side
}

export interface PlainNote {
  text: string
  line: number
}

export interface PlainModule {
  description: string
  /** frontmatter lists */
  import: string[]
  requires: string[]
  exported: string[]
  notes: PlainNote[]
  /** definitions, implementation reqs, test reqs (and any other non-spec section), by name */
  sections: Record<string, Clause[]>
  specs: Spec[]
  explicitIds: boolean
}

export const SPEC_SECTION = 'functional specs'
export const SECTIONS: { key: string; title: string }[] = [
  { key: 'definitions', title: 'Definitions' },
  { key: 'implementation reqs', title: 'Implementation requirements' },
  { key: 'test reqs', title: 'Test requirements' },
]

const listValue = (v: string) =>
  v
    .replace(/^\[|\]$/g, '')
    .split(',')
    .map((x) => x.trim().replace(/^["']|["']$/g, ''))
    .filter(Boolean)

/** Parse a module; `side` tags its clauses' line numbers (old file for the diff's base). */
export function parsePlain(text: string, side: Side = 'new'): PlainModule {
  const lines = text.replace(/\r\n?/g, '\n').split('\n')
  const mod: PlainModule = { description: '', import: [], requires: [], exported: [], notes: [], sections: {}, specs: [], explicitIds: false }
  let i = 0
  if (lines[0]?.trim() === '---') {
    const fm: Record<string, string | string[]> = {}
    let key: string | null = null
    for (i = 1; i < lines.length && lines[i].trim() !== '---'; i++) {
      const l = lines[i]
      const m = l.match(/^([\w-]+):\s*(.*)$/)
      if (m) {
        key = m[1]
        const v = m[2].trim()
        fm[key] = v.startsWith('[') ? listValue(v) : v === '>' || v === '|' || v === '>-' || v === '|-' ? '' : v.replace(/^["']|["']$/g, '')
      } else if (key && /^\s+-\s+/.test(l)) {
        const prev = fm[key]
        fm[key] = [...(Array.isArray(prev) ? prev : prev ? [prev] : []), l.replace(/^\s+-\s+/, '').replace(/^["']|["']$/g, '')]
      } else if (key && l.trim() && typeof fm[key] === 'string') {
        fm[key] = `${fm[key]} ${l.trim()}`.trim()
      }
    }
    i++
    const str = (k: string) => (typeof fm[k] === 'string' ? (fm[k] as string) : Array.isArray(fm[k]) ? (fm[k] as string[]).join(' ') : '')
    const arr = (k: string) => (Array.isArray(fm[k]) ? (fm[k] as string[]) : fm[k] ? listValue(fm[k] as string) : [])
    mod.description = str('description')
    mod.import = arr('import')
    mod.requires = arr('requires')
    mod.exported = arr('exported_concepts').map((c) => c.replace(/^:|:$/g, ''))
  }
  mod.explicitIds = /^>\s?spec:\s*\S+\s*$/m.test(text)

  let section: string | null = null
  let sub: 'acceptance tests' | null = null
  let spec = null as Spec | null
  let stack: { indent: number; list: Clause[] }[] = []
  let last = null as Clause | null
  let note = null as PlainNote | null
  let position = 0
  const target = (): Clause[] | null => {
    if (section === SPEC_SECTION) return spec ? (sub ? spec.tests : spec.clauses) : null
    if (section) return (mod.sections[section] ??= [])
    return null
  }
  const reset = () => {
    const t = target()
    stack = t ? [{ indent: -1, list: t }] : []
    last = null
  }
  const newSpec = (id: string, line: number) => {
    spec = { id, clauses: [], tests: [], state: 'same', affectedBy: [], start: line, end: line, side }
    mod.specs.push(spec)
    sub = null
    reset()
  }
  const flushNote = () => {
    if (note) mod.notes.push(note)
    note = null
  }

  for (; i < lines.length; i++) {
    const line = lines[i]
    const n = i + 1
    const sec = line.match(/^\s*\*\*\*(.+?)\*\*\*\s*$/)
    if (sec) {
      flushNote()
      const name = sec[1].trim().toLowerCase()
      if (name === 'acceptance tests') sub = 'acceptance tests'
      else {
        section = name
        sub = null
        spec = null
      }
      reset()
      continue
    }
    const q = line.match(/^\s*>\s?(.*)$/)
    if (q) {
      const s = q[1].match(/^spec:\s*(\S+)\s*$/)
      if (s && section === SPEC_SECTION) newSpec(s[1], n)
      else if (!section) note = note ? { ...note, text: `${note.text} ${q[1]}`.trim() } : { text: q[1], line: n }
      else if (last) {
        last.notes.push(q[1])
        last.end = n
      }
      if (spec) spec.end = n
      continue
    }
    if (!section) {
      if (!line.trim()) flushNote()
      continue
    }
    const b = line.match(/^(\s*)-\s+(.*)$/)
    if (b && section === SPEC_SECTION && !mod.explicitIds && b[1].length === 0) newSpec(`#${++position}`, n)
    if (b) {
      const indent = b[1].length
      while (stack.length > 1 && stack[stack.length - 1].indent >= indent) stack.pop()
      if (!stack.length) continue
      const c: Clause = { text: b[2].trim(), notes: [], children: [], state: 'same', start: n, end: n, side }
      stack[stack.length - 1].list.push(c)
      stack.push({ indent, list: c.children })
      last = c
      if (spec) spec.end = n
    } else if (line.trim() && last) {
      last.text += ` ${line.trim()}`
      last.end = n
      if (spec) spec.end = n
    }
  }
  flushNote()
  return mod
}

// ---- diff --------------------------------------------------------------------

interface Flat {
  c: Clause
  depth: number
  parent: Clause | null
}

export const flatClauses = (nodes: Clause[], out: Flat[] = [], depth = 0, parent: Clause | null = null): Flat[] => {
  for (const c of nodes) {
    out.push({ c, depth, parent })
    flatClauses(c.children, out, depth + 1, c)
  }
  return out
}

const words = (s: string) => s.split(/\s+/).filter(Boolean)

/** Word overlap of two texts, 0–1. */
export function similarity(a: string, b: string): number {
  const A = new Set(words(a))
  const B = new Set(words(b))
  let common = 0
  for (const w of A) if (B.has(w)) common++
  return common / Math.max(1, A.size, B.size)
}

function lcs<A, B>(a: A[], b: B[], eq: (x: A, y: B) => boolean): [number, number][] {
  const m = a.length
  const n = b.length
  const T = Array.from({ length: m + 1 }, () => new Uint32Array(n + 1))
  for (let i = m - 1; i >= 0; i--) for (let j = n - 1; j >= 0; j--) T[i][j] = eq(a[i], b[j]) ? T[i + 1][j + 1] + 1 : Math.max(T[i + 1][j], T[i][j + 1])
  const pairs: [number, number][] = []
  let i = 0
  let j = 0
  while (i < m && j < n) {
    if (eq(a[i], b[j])) {
      pairs.push([i, j])
      i++
      j++
    } else if (T[i + 1][j] >= T[i][j + 1]) i++
    else j++
  }
  return pairs
}

export interface WordPart {
  k: 'eq' | 'ins' | 'del'
  t: string
}

/** A changed clause's words: kept, inserted, deleted runs. */
export function wordDiff(a: string, b: string): WordPart[] {
  const A = words(a)
  const B = words(b)
  const out: WordPart[] = []
  let i = 0
  let j = 0
  const push = (k: WordPart['k'], t: string) => {
    const prev = out[out.length - 1]
    if (prev && prev.k === k) prev.t += ` ${t}`
    else out.push({ k, t })
  }
  for (const [pi, pj] of lcs(A, B, (x, y) => x === y)) {
    if (i < pi) push('del', A.slice(i, pi).join(' '))
    if (j < pj) push('ins', B.slice(j, pj).join(' '))
    push('eq', A[pi])
    i = pi + 1
    j = pj + 1
  }
  if (i < A.length) push('del', A.slice(i).join(' '))
  if (j < B.length) push('ins', B.slice(j).join(' '))
  return out
}

export interface Touched {
  /** a spec id, a section key, or `description` */
  where: string
  tests?: boolean
  added: number
  changed: number
  removed: number
  newSpec?: boolean
  removedSpec?: boolean
}

export interface PlainDiff {
  /** the new module with states set and removed clauses / specs put back in */
  mod: PlainModule
  oldDescription: string | null
  stats: { added: number; changed: number; removed: number }
  touched: Touched[]
  /** section keys with any change */
  touchedSections: Set<string>
  /** definitions (names) whose clauses changed */
  changedTerms: Set<string>
}

const markAll = (nodes: Clause[], state: ClauseState) => flatClauses(nodes).forEach((x) => (x.c.state = state))
const DEF_RE = /^:([A-Za-z0-9+.\-_]+):/

/**
 * The new module diffed against the old one, clause by clause. `old` null: the file is new —
 * every clause is added.
 */
export function diffPlain(oldText: string | null, newText: string): PlainDiff {
  const mod = parsePlain(newText, 'new')
  const stats = { added: 0, changed: 0, removed: 0 }
  const touched: Touched[] = []
  const touchedSections = new Set<string>()
  const changedTerms = new Set<string>()
  const old = parsePlain(oldText ?? '', 'old')

  function container(oldNodes: Clause[], newNodes: Clause[], where: string, tests = false): boolean {
    const O = flatClauses(oldNodes)
    const N = flatClauses(newNodes)
    const pairs = lcs(O, N, (x, y) => x.c.text === y.c.text)
    const oToN = new Map<number, number>(pairs)
    const matchedN = new Set(pairs.map((p) => p[1]))
    const unO = O.map((_, k) => k).filter((k) => !oToN.has(k))
    const unN = N.map((_, k) => k).filter((k) => !matchedN.has(k))
    const used = new Set<number>()
    let added = 0
    let changed = 0
    let removed = 0
    for (const oi of unO) {
      let best: number | null = null
      let bestS = 0.45
      for (const ni of unN) {
        if (used.has(ni)) continue
        const s = similarity(O[oi].c.text, N[ni].c.text)
        if (s > bestS) {
          bestS = s
          best = ni
        }
      }
      if (best != null) {
        used.add(best)
        N[best].c.state = 'changed'
        N[best].c.oldText = O[oi].c.text
        oToN.set(oi, best)
        changed++
      }
    }
    for (const ni of unN) if (!used.has(ni)) {
      N[ni].c.state = 'added'
      added++
    }
    // A removed clause goes back where it stood: after its previous sibling (survivor or ghost),
    // else first under its parent, else first in the container.
    const placed = new Map<number, { c: Clause; list: Clause[] }>()
    for (const [oi, ni] of oToN) placed.set(oi, { c: N[ni].c, list: N[ni].parent ? N[ni].parent!.children : newNodes })
    for (const oi of unO) {
      if (oToN.has(oi)) continue
      const ghost: Clause = { ...O[oi].c, children: [], state: 'removed' }
      removed++
      const d = O[oi].depth
      let a = oi - 1
      while (a >= 0 && (O[a].depth > d || !placed.has(a))) a--
      let list = newNodes
      if (a < 0) list.unshift(ghost)
      else if (O[a].depth === d) {
        const p = placed.get(a)!
        list = p.list
        list.splice(list.indexOf(p.c) + 1, 0, ghost)
      } else {
        list = placed.get(a)!.c.children
        list.unshift(ghost)
      }
      placed.set(oi, { c: ghost, list })
    }
    stats.added += added
    stats.changed += changed
    stats.removed += removed
    const any = added + changed + removed > 0
    if (any) touched.push({ where, tests, added, changed, removed })
    return any
  }

  const oldDescription = oldText != null && old.description !== mod.description ? old.description : null
  if (oldDescription != null) {
    touched.push({ where: 'description', added: 0, changed: 1, removed: 0 })
    stats.changed++
  }

  const keys = new Set([...Object.keys(old.sections), ...Object.keys(mod.sections)])
  for (const key of keys) {
    mod.sections[key] ??= []
    const before = old.sections[key] ?? []
    // Which definitions changed: compare each term's whole text (clause + sub-clauses).
    if (key === 'definitions') {
      const whole = (c: Clause) => flatClauses([c]).map((x) => x.c.text).join('\n')
      const byName = (cs: Clause[]) => new Map(cs.flatMap((c) => (DEF_RE.test(c.text) ? [[c.text.match(DEF_RE)![1], whole(c)] as const] : [])))
      const o = byName(before)
      const nw = byName(mod.sections[key])
      for (const name of new Set([...o.keys(), ...nw.keys()])) if (o.get(name) !== nw.get(name)) changedTerms.add(name)
    }
    if (container(before, mod.sections[key], key)) touchedSections.add(key)
  }

  // Specs pair by id when the module marks them; by text otherwise (position ids shift).
  const textOf = (s: Spec) => [...flatClauses(s.clauses), ...flatClauses(s.tests)].map((x) => x.c.text).join(' ')
  const newFor = new Map<number, number>()
  if (mod.explicitIds) {
    old.specs.forEach((o, oi) => {
      const ni = mod.specs.findIndex((s) => s.id === o.id)
      if (ni >= 0) newFor.set(oi, ni)
    })
  } else {
    for (const [oi, ni] of lcs(old.specs, mod.specs, (a, b) => textOf(a) === textOf(b))) newFor.set(oi, ni)
    const used = new Set(newFor.values())
    old.specs.forEach((o, oi) => {
      if (newFor.has(oi)) return
      let best: number | null = null
      let bestS = 0.3
      mod.specs.forEach((s, ni) => {
        if (used.has(ni)) return
        const v = similarity(textOf(o), textOf(s))
        if (v > bestS) {
          bestS = v
          best = ni
        }
      })
      if (best != null) {
        newFor.set(oi, best)
        used.add(best)
      }
    })
  }
  const pairedNew = new Map([...newFor].map(([o, n]) => [n, o]))
  const live = mod.specs.slice()
  live.forEach((s, ni) => {
    const oi = pairedNew.get(ni)
    if (oi == null) {
      s.state = 'added'
      markAll(s.clauses, 'added')
      markAll(s.tests, 'added')
      const count = flatClauses(s.clauses).length + flatClauses(s.tests).length
      stats.added += count
      touched.push({ where: s.id, added: count, changed: 0, removed: 0, newSpec: true })
      return
    }
    const o = old.specs[oi]
    const a = container(o.clauses, s.clauses, s.id)
    const b = container(o.tests, s.tests, s.id, true)
    if (a || b) s.state = 'changed'
  })
  // A removed spec goes back after the spec its nearest surviving predecessor became.
  old.specs.forEach((o, oi) => {
    if (newFor.has(oi)) return
    const ghost: Spec = { ...o, id: mod.explicitIds ? o.id : `old ${o.id}`, state: 'removed' }
    markAll(ghost.clauses, 'removed')
    markAll(ghost.tests, 'removed')
    const count = flatClauses(o.clauses).length + flatClauses(o.tests).length
    stats.removed += count
    touched.push({ where: ghost.id, added: 0, changed: 0, removed: count, removedSpec: true })
    let k = oi - 1
    while (k >= 0 && !newFor.has(k)) k--
    mod.specs.splice(k < 0 ? 0 : mod.specs.indexOf(live[newFor.get(k)!]) + 1, 0, ghost)
  })

  // A spec whose text did not change but uses a changed definition: its meaning may have.
  if (changedTerms.size)
    for (const s of mod.specs) {
      if (s.state === 'removed') continue
      const all = [...flatClauses(s.clauses), ...flatClauses(s.tests)].map((x) => x.c.text).join(' ')
      s.affectedBy = [...changedTerms].filter((t) => all.includes(`:${t}:`))
    }

  const order = (t: Touched) => {
    if (t.where === 'description') return -3
    const sk = SECTIONS.findIndex((x) => x.key === t.where)
    if (sk >= 0) return -2 + sk / 10
    return mod.specs.findIndex((s) => s.id === t.where)
  }
  touched.sort((a, b) => order(a) - order(b))
  return { mod, oldDescription, stats, touched, touchedSections, changedTerms }
}

// ---- helpers for the views ---------------------------------------------------

/** Definitions by name → their text (for term chips). */
export function definitions(mod: PlainModule): Map<string, string> {
  const out = new Map<string, string>()
  for (const c of mod.sections['definitions'] ?? []) {
    const m = c.text.match(/^:([A-Za-z0-9+.\-_]+):\s*(.*)$/)
    if (m && c.state !== 'removed') out.set(m[1], m[2])
  }
  return out
}

/** A one-line gist of a spec for the index: its first clause up to the first stop. */
export function gist(s: Spec, max = 80): string {
  const first = s.clauses[0]?.text ?? s.tests[0]?.text ?? ''
  const t = first.replace(/:([A-Za-z0-9+.\-_]+):/g, '$1').replace(/`/g, '')
  const cut = t.search(/[:;.—](\s|$)|\s-\s/)
  const g = cut > 20 ? t.slice(0, cut) : t
  return g.length > max ? `${g.slice(0, max - 1)}…` : g
}

/** An anchor id for a spec. */
export const specAnchor = (id: string) => `plain-${id.replace(/[^A-Za-z0-9]+/g, '-').replace(/^-|-$/g, '')}`

/** Is this path a PlainLang module? */
export const isPlainPath = (p: string) => /\.plain$/i.test(p)

export type InlinePart =
  | { k: 'text'; t: string }
  | { k: 'code'; t: string }
  | { k: 'term'; name: string }
  | { k: 'ref'; id: string }

/** A clause's text → plain text, `code`, :Term: and §ref parts. */
export function inlineParts(text: string): InlinePart[] {
  const out: InlinePart[] = []
  for (const piece of text.split(/(`[^`]*`)/)) {
    if (!piece) continue
    if (piece.length > 1 && piece.startsWith('`') && piece.endsWith('`')) {
      out.push({ k: 'code', t: piece.slice(1, -1) })
      continue
    }
    const re = /:([A-Z][A-Za-z0-9+.\-_]*):|(§\d+(?:\.\d+)*)/g
    let at = 0
    for (const m of piece.matchAll(re)) {
      if (m.index! > at) out.push({ k: 'text', t: piece.slice(at, m.index) })
      out.push(m[1] ? { k: 'term', name: m[1] } : { k: 'ref', id: m[2] })
      at = m.index! + m[0].length
    }
    if (at < piece.length) out.push({ k: 'text', t: piece.slice(at) })
  }
  return out
}

/** A clause's identity in its file: side and first line (stable across re-renders and copies). */
export const clauseKey = (c: Pick<Clause, 'side' | 'start'>) => `${c.side}:${c.start}`

/** A module drawn without a diff (the file preview): every clause `same`. */
export function undiffed(text: string): PlainDiff {
  return { mod: parsePlain(text), oldDescription: null, stats: { added: 0, changed: 0, removed: 0 }, touched: [], touchedSections: new Set(), changedTerms: new Set() }
}
