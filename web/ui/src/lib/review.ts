import type { DiffFile, DiffHunk, DiffLine, DiffRepo, ReviewThread } from '@/api/types'
import { clauseKey, flatClauses, type Clause, type PlainDiff } from '@/lib/plain'

/** `#/review/<host>/<id>`: the diff reviewer for that session. */
export const reviewHref = (host: string, id: string) => `/review/${encodeURIComponent(host)}/${encodeURIComponent(id)}`

export function parseReviewLocation(location: string): { host: string; id: string } | null {
  const m = /^\/review\/([^/]+)\/([^/]+)\/?$/.exec(location)
  if (!m) return null
  try {
    return { host: decodeURIComponent(m[1]), id: decodeURIComponent(m[2]) }
  } catch {
    return null
  }
}

export interface TreeDir {
  kind: 'dir'
  /** display name: a chain of single-child folders is one row (`src/lib`) */
  name: string
  path: string
  children: TreeNode[]
}
export interface TreeFile {
  kind: 'file'
  name: string
  /** fileKey(root, path): unique across checkouts */
  key: string
  file: DiffFile
}
export type TreeNode = TreeDir | TreeFile

/** The changed files as a folder tree (folders first, then files, each by name), single-child folder chains merged. */
export function fileTree(files: DiffFile[], checkout = ''): TreeNode[] {
  const root: TreeDir = { kind: 'dir', name: '', path: '', children: [] }
  for (const file of files) {
    const parts = file.path.split('/')
    let dir = root
    for (const part of parts.slice(0, -1)) {
      const path = dir.path ? `${dir.path}/${part}` : part
      let next = dir.children.find((c): c is TreeDir => c.kind === 'dir' && c.path === path)
      if (!next) {
        next = { kind: 'dir', name: part, path, children: [] }
        dir.children.push(next)
      }
      dir = next
    }
    dir.children.push({ kind: 'file', name: parts.at(-1) ?? file.path, key: fileKey(checkout, file.path), file })
  }
  const finish = (nodes: TreeNode[]): TreeNode[] =>
    nodes
      .map((n) => {
        if (n.kind === 'file') return n
        let d = n
        while (d.children.length === 1 && d.children[0].kind === 'dir') {
          const only = d.children[0]
          d = { ...only, name: `${d.name}/${only.name}` }
        }
        return { ...d, children: finish(d.children) }
      })
      .sort((a, b) => (a.kind !== b.kind ? (a.kind === 'dir' ? -1 : 1) : a.name.localeCompare(b.name)))
  return finish(root.children)
}

/** The files in the order the tree shows them (the diff list follows the tree). */
export function treeOrder(nodes: TreeNode[]): DiffFile[] {
  return nodes.flatMap((n) => (n.kind === 'file' ? [n.file] : treeOrder(n.children)))
}

export interface SplitRow {
  left: DiffLine | null
  right: DiffLine | null
}

/** Side-by-side rows: context on both sides; a run of removals paired line by line with the additions after it. */
export function splitRows(hunk: DiffHunk): SplitRow[] {
  const rows: SplitRow[] = []
  const lines = hunk.lines
  for (let i = 0; i < lines.length; ) {
    const l = lines[i]
    if (l.t === ' ') {
      rows.push({ left: l, right: l })
      i++
      continue
    }
    const del: DiffLine[] = []
    const add: DiffLine[] = []
    while (i < lines.length && lines[i].t === '-') del.push(lines[i++])
    while (i < lines.length && lines[i].t === '+') add.push(lines[i++])
    for (let k = 0; k < Math.max(del.length, add.length); k++) rows.push({ left: del[k] ?? null, right: add[k] ?? null })
  }
  return rows
}

/** A short content signature of a file's diff: "Viewed" is dropped when it changes. */
export function fileSignature(file: DiffFile): string {
  let h = 5381
  const feed = (s: string) => {
    for (let i = 0; i < s.length; i++) h = ((h << 5) + h + s.charCodeAt(i)) | 0
  }
  feed(`${file.status}\0${file.oldPath ?? ''}\0${file.binary ? 1 : 0}`)
  for (const hunk of file.hunks) {
    feed(hunk.header)
    for (const l of hunk.lines) feed(`${l.t}${l.text}\n`)
  }
  return (h >>> 0).toString(36)
}

/** GitHub's five-square bar: how many of 5 are green. */
export function diffBar(additions: number, deletions: number): number {
  const total = additions + deletions
  if (!total) return 0
  return Math.round((additions / total) * 5)
}

/** A file card's element id (looked up with getElementById, so any path is fine). */
export const fileAnchor = (path: string) => `diff:${path}`

export type Side = 'new' | 'old'

/** Lines selected for a comment: one side's numbering, start ≤ end. */
export interface LineSelection {
  path: string
  side: Side
  start: number
  end: number
}

/** A line's number on a side (null when the line is not on it). */
export const lineNo = (l: DiffLine | null, side: Side) => (l ? (side === 'new' ? l.n : l.o) : null)

/** The side a unified row's gutter click comments on: the new side unless the line was removed. */
export const unifiedSide = (l: DiffLine): Side => (l.n != null ? 'new' : 'old')

/** Click on a line number: a new single-line selection, or (shift) extend the one on the same file and side. */
export function selectLine(prev: LineSelection | null, path: string, side: Side, n: number, extend: boolean): LineSelection {
  if (extend && prev && prev.path === path && prev.side === side) return { path, side, start: Math.min(prev.start, n), end: Math.max(prev.end, n) }
  return { path, side, start: n, end: n }
}

export const inSelection = (sel: LineSelection | null, path: string, side: Side, n: number | null) =>
  !!sel && n != null && sel.path === path && sel.side === side && n >= sel.start && n <= sel.end

/** The text of the selected lines, as the diff shows them (lines of the other side skipped). */
export function selectionCode(file: DiffFile, sel: LineSelection): string[] {
  // A .plain spec carries its whole text: lines outside the hunks are commentable too.
  const whole = file.plain?.[sel.side]
  if (whole != null) return whole.replace(/\r\n?/g, '\n').split('\n').slice(sel.start - 1, sel.end)
  const out: string[] = []
  for (const h of file.hunks) for (const l of h.lines) {
    const n = lineNo(l, sel.side)
    if (n != null && n >= sel.start && n <= sel.end && (sel.side === 'new' ? l.t !== '-' : l.t !== '+')) out.push(l.text)
  }
  return out
}

/** Where a thread shows: after the row holding its last line. `${side}:${line}` */
export const anchorKey = (side: Side, n: number) => `${side}:${n}`

/**
 * A file's threads by anchor key; a thread whose last line is not in the diff any more
 * (`unplaced`) shows at the top of the file. `outdated`: its lines read differently now.
 */
export function placeThreads(file: DiffFile, threads: ReviewThread[]): { at: Map<string, ReviewThread[]>; unplaced: ReviewThread[]; outdated: Set<string> } {
  const present = new Set<string>()
  for (const h of file.hunks) for (const l of h.lines) {
    if (l.n != null && l.t !== '-') present.add(anchorKey('new', l.n))
    if (l.o != null && l.t !== '+') present.add(anchorKey('old', l.o))
  }
  const at = new Map<string, ReviewThread[]>()
  const unplaced: ReviewThread[] = []
  const outdated = new Set<string>()
  for (const t of threads) {
    if (t.path !== file.path) continue
    const key = anchorKey(t.side, t.end)
    if (!present.has(key)) {
      unplaced.push(t)
      // A .plain spec's thread may sit on a clause the hunks do not show: outdated only if its lines changed.
      if (!file.plain || !sameCode(file, t)) outdated.add(t.id)
      continue
    }
    at.set(key, [...(at.get(key) ?? []), t])
    if (!sameCode(file, t)) outdated.add(t.id)
  }
  return { at, unplaced, outdated }
}

/** Do the thread's lines still read as when it was made? */
export function sameCode(file: DiffFile, t: ReviewThread): boolean {
  const now = selectionCode(file, t)
  return now.length === t.code.length && now.every((x, i) => x === t.code[i])
}

/** Threads on files the diff no longer shows. */
export function orphanThreads(files: DiffFile[], threads: ReviewThread[]): ReviewThread[] {
  const paths = new Set(files.map((f) => f.path))
  return threads.filter((t) => !paths.has(t.path))
}

/** A thread still waiting for the session's answer (poll faster). */
export const threadPending = (t: ReviewThread) => t.entries.some((e) => e.state === 'sent' || e.state === 'queued' || e.state === 'working')

export const rangeText = (t: { start: number; end: number; side: Side }) =>
  `${t.side === 'old' ? 'removed ' : ''}${t.start === t.end ? `line ${t.start}` : `lines ${t.start}–${t.end}`}`

/** A file's identity across checkouts. */
export const fileKey = (root: string, path: string) => (root ? `${root}\u0000${path}` : path)

/** A thread's file key; a thread from before roots belongs to the session's own checkout. */
export const threadKey = (t: Pick<ReviewThread, 'root' | 'path'>, defaultRoot: string) => fileKey(t.root ?? defaultRoot, t.path)

export interface RepoView {
  repo: DiffRepo
  tree: TreeNode[]
  /** in tree order */
  files: { key: string; file: DiffFile }[]
}

/** The checkouts with changes (or an error), each with its tree; clean ones are left out. */
export function repoViews(repos: DiffRepo[]): RepoView[] {
  return repos
    .filter((r) => r.error || r.files?.length)
    .map((repo) => {
      const tree = fileTree(repo.files ?? [], repo.root)
      const files: { key: string; file: DiffFile }[] = []
      const walk = (nodes: TreeNode[]) => nodes.forEach((n) => (n.kind === 'file' ? files.push({ key: n.key, file: n.file }) : walk(n.children)))
      walk(tree)
      return { repo, tree, files }
    })
}

/** Threads whose file the diff no longer shows (by file key). */
export function orphanThreadsByKey(keys: Set<string>, threads: ReviewThread[], defaultRoot: string): ReviewThread[] {
  return threads.filter((t) => !keys.has(threadKey(t, defaultRoot)))
}

/** A diff line element's data (FileDiff's `data-*`): `side` only in the split view. */
export interface LineRow {
  side?: Side
  o: number | null
  n: number | null
  t: string
}

/**
 * The lines a text selection covers (the rows from its first to its last, in order) as a
 * comment range. Split view: the side the selection ends on. Unified: the new side, unless the
 * selection holds only removed lines. null when no row of that side is in it.
 */
export function rowsToRange(rows: LineRow[], endSide?: Side): { side: Side; start: number; end: number } | null {
  if (!rows.length) return null
  const split = rows.some((r) => r.side)
  const side: Side = split ? (endSide ?? rows.at(-1)?.side ?? 'new') : rows.every((r) => r.t === '-') ? 'old' : 'new'
  const nums = rows
    .filter((r) => (!split || r.side === side) && (side === 'new' ? r.t !== '-' : r.t !== '+'))
    .map((r) => (side === 'new' ? r.n : r.o))
    .filter((n): n is number => n != null)
  if (!nums.length) return null
  return { side, start: Math.min(...nums), end: Math.max(...nums) }
}

/** Every clause of a diffed .plain module (sections, specs, acceptance tests), in page order. */
export function allClauses(diff: PlainDiff): Clause[] {
  const out: Clause[] = []
  for (const list of Object.values(diff.mod.sections)) out.push(...flatClauses(list).map((x) => x.c))
  for (const s of diff.mod.specs) out.push(...flatClauses(s.clauses).map((x) => x.c), ...flatClauses(s.tests).map((x) => x.c))
  return out
}

/** The clause a line belongs to on a side: the one holding it, else the last one starting before it. */
export function clauseAt(clauses: Clause[], side: Side, line: number): string | null {
  let best: Clause | null = null
  for (const c of clauses) {
    if (c.side !== side) continue
    if (c.start <= line && line <= c.end) return clauseKey(c)
    if (c.start <= line && (!best || c.start > best.start)) best = c
  }
  return best ? clauseKey(best) : null
}

/** A .plain file's threads by the clause they end on (the Spec view's placeThreads). */
export function placeOnClauses(file: DiffFile, clauses: Clause[], threads: ReviewThread[]): { at: Map<string, ReviewThread[]>; unplaced: ReviewThread[]; outdated: Set<string> } {
  const at = new Map<string, ReviewThread[]>()
  const unplaced: ReviewThread[] = []
  const outdated = new Set<string>()
  for (const t of threads) {
    if (t.path !== file.path) continue
    if (!sameCode(file, t)) outdated.add(t.id)
    const key = clauseAt(clauses, t.side, t.end)
    if (key == null) unplaced.push(t)
    else at.set(key, [...(at.get(key) ?? []), t])
  }
  return { at, unplaced, outdated }
}
