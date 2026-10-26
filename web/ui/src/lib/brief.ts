// Session briefs — pure helpers for the brief panel (unit-tested in brief.test.ts).
// The file format is the server's contract (docs/architecture.md → "Session briefs"); these
// only touch the body the way a human editor would: toggle a todo's checkbox, keep every other line.
import type { BriefResource, BriefResourceKind, BriefTodo } from '@/api/types'

const FRONTMATTER_RE = /^---\n[\s\S]*?\n---[ \t]*(?:\n|$)/
const HEADING_RE = /^##\s+(.+?)\s*#*\s*$/
const CHECK_RE = /^(\s*[-*+]\s+\[)([ xX])(\]\s+.*)$/

/** The markdown without its `---` frontmatter (what the editor shows; the server keeps its own keys). */
export function briefBody(markdown: string | null | undefined): string {
  const src = String(markdown ?? '').replace(/\r\n/g, '\n')
  return src.replace(FRONTMATTER_RE, '').replace(/^\n+/, '')
}

/** Section headings whose checkbox lines are the todos (`## Plan` is the legacy name; the server reads it as Todos). */
const TODO_HEADINGS = new Set(['todos', 'plan'])

/** The brief's todos: `parsed.todos`, else the deprecated `parsed.plan` (older servers). */
export function briefTodos(parsed: { todos?: readonly BriefTodo[] | null; plan?: readonly BriefTodo[] | null } | null | undefined): BriefTodo[] {
  return [...(parsed?.todos ?? parsed?.plan ?? [])]
}

/**
 * Set todo `index` (0-based, the order of `parsed.todos`: checkbox lines of `## Todos`, or of a
 * legacy `## Plan`, in file order) to `done`. → the new body (frontmatter dropped), or null when
 * there is no such item.
 */
export function setTodoItem(markdown: string, index: number, done: boolean): string | null {
  const lines = briefBody(markdown).split('\n')
  let inTodos = false
  let n = 0
  for (let i = 0; i < lines.length; i++) {
    const h = HEADING_RE.exec(lines[i])
    if (h) {
      inTodos = TODO_HEADINGS.has(h[1].trim().toLowerCase())
      continue
    }
    if (!inTodos) continue
    const m = CHECK_RE.exec(lines[i])
    if (!m) continue
    if (n === index) {
      lines[i] = `${m[1]}${done ? 'x' : ' '}${m[3]}`
      return lines.join('\n')
    }
    n += 1
  }
  return null
}

/**
 * Display order of resource groups: where the work came from (Origin), where it lives (Git, and the legacy Branch / Worktree
 * lines), what it produced, links, then files (long, collapsed by default); hand-written lines
 * (`kind: null`) come last as Notes.
 */
export const RESOURCE_ORDER: BriefResourceKind[] = ['Origin', 'Git', 'Branch', 'Worktree', 'PR', 'Issue', 'Artifact', 'Spec', 'Link', 'File']

const GROUP_LABEL: Record<string, string> = {
  Origin: 'Origin',
  Git: 'Git',
  PR: 'Pull requests',
  Issue: 'Issues',
  Artifact: 'Artifacts',
  Spec: 'Specs',
  File: 'Files',
  Branch: 'Branch',
  Worktree: 'Worktree',
  Link: 'Links',
}

export interface ResourceGroup {
  /** A kind, or null for hand-written lines. Unknown kinds from a newer server group on their own. */
  kind: string | null
  label: string
  items: BriefResource[]
}

/** Group resources by kind in RESOURCE_ORDER (unknown kinds before notes); order inside a group is kept. */
export function groupResources(resources: readonly BriefResource[] | null | undefined): ResourceGroup[] {
  const by = new Map<string | null, BriefResource[]>()
  for (const r of resources ?? []) {
    const k = r.kind ?? null
    const list = by.get(k)
    if (list) list.push(r)
    else by.set(k, [r])
  }
  const rank = (k: string | null) => {
    if (k == null) return 1000
    const i = RESOURCE_ORDER.indexOf(k as BriefResourceKind)
    return i >= 0 ? i : 500
  }
  return [...by.entries()]
    .sort((a, b) => rank(a[0]) - rank(b[0]))
    .map(([kind, items]) => ({ kind, label: kind == null ? 'Notes' : (GROUP_LABEL[kind] ?? kind), items }))
}

/**
 * A `Git` resource as one line: the branch (null = detached / unknown) and where it is checked
 * out — `worktree ~/path` for a linked worktree, `repo ~/path` for the main checkout, just the
 * path when the server could not tell.
 */
export function gitLine(r: Pick<BriefResource, 'branch' | 'linked' | 'path'>): { branch: string | null; where: string | null } {
  const branch = r.branch ?? null
  const where = r.path ? (r.linked === true ? `worktree ${r.path}` : r.linked === false ? `repo ${r.path}` : r.path) : null
  return { branch, where }
}

/** "Open in VS Code" / "Open in Cursor" for an editorUrl; null = no button. Guesses from the URL when `editor` is missing. */
export function editorLabel(editor: string | null | undefined, url: string | null | undefined): string | null {
  if (!url) return null
  const which = editor ?? (url.startsWith('cursor:') ? 'cursor' : url.startsWith('vscode:') ? 'vscode' : null)
  return which === 'cursor' ? 'Open in Cursor' : which === 'vscode' ? 'Open in VS Code' : 'Open in editor'
}

/** "3/7" progress of the todos; null when there are none. */
export function todoProgress(todos: readonly { done: boolean }[] | null | undefined): { done: number; total: number } | null {
  if (!todos?.length) return null
  return { done: todos.filter((i) => i.done).length, total: todos.length }
}

/** The New session prompt for "Continue in new session": the brief's prompt (it ends with the separator), a blank line, the caret. */
export function continueDraft(prompt: string | null | undefined): string {
  const p = String(prompt ?? '').trimEnd()
  return p ? `${p}\n\n` : ''
}

/** The server's CONTINUE_DEFAULT_TASK (lib/brief-format.mjs): the task when nothing is written below the separator. */
export const CONTINUE_DEFAULT_TASK =
  'Pick up where it left off. Check the current state (git status, the PRs above) before changing anything, and tell me briefly what you found first.'

/** A continue prompt sent with nothing below its separator gets the default task; any other prompt is sent as is. */
export function withContinueTask(prompt: string): string {
  if (!/\bContinue the work of [^\n]*\. Its brief:/.test(prompt)) return prompt
  const p = prompt.trimEnd()
  return /(?:^|\n)---$/.test(p) ? `${p}\n\n${CONTINUE_DEFAULT_TASK}` : prompt
}

/** Whole minutes (≥ 1) until the hourly cap frees up. */
export function retryMinutes(ms: number | null | undefined): number {
  const n = Number(ms)
  return Number.isFinite(n) && n > 0 ? Math.max(1, Math.ceil(n / 60_000)) : 1
}

/** ISO (frontmatter) or epoch ms → epoch ms; null when missing / unparsable. */
export function briefTime(v: string | number | null | undefined): number | null {
  if (v == null || v === '') return null
  const t = typeof v === 'number' ? v : Date.parse(v)
  return Number.isFinite(t) && t > 0 ? t : null
}
