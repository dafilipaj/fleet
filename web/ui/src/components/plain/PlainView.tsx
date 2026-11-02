import { ArrowUpRightIcon, CheckIcon, ChevronLeftIcon, CircleDashedIcon, CornerDownRightIcon, Loader2Icon, MessageSquarePlusIcon } from 'lucide-react'
import { Popover as PopoverPrimitive } from 'radix-ui'
import { createContext, Fragment, memo, useContext, useEffect, useId, useMemo, useState, type ReactNode } from 'react'

import { Popover, PopoverContent, PopoverTrigger } from '@/components/ui/popover'
import { usePlainConcepts } from '@/hooks/usePlainConcepts'
import { definitions, flatClauses, gist, inlineParts, SECTIONS, SPEC_SECTION, specAnchor, wordDiff, type Clause, type ClauseState, type PlainDiff, type Spec, type Touched } from '@/lib/plain'
import type { ConceptHit, ConceptResolver } from '@/lib/plainConcepts'
import { cn } from '@/lib/utils'

// A PlainLang module drawn for reading: the module's header, a Changes card (when diffed),
// definitions as a glossary, requirements, and one card per functional spec with its acceptance
// tests. A term (`:Widget:`) is a chip: a click shows its definition in a popover, wherever it is
// defined — this module, what it imports / requires, or elsewhere in its checkout (`source`
// gives the file API; without it only this module); terms inside a definition open in place,
// with Back. `§1.2` jumps to the spec.
// Diffed, every clause carries its state (added / changed with a word diff / removed in place).
// With `commenting`, a clause is a comment target: its own lines, on its side of the diff.

export interface ClauseCommenting {
  selected: (c: Clause) => boolean
  onClause: (c: Clause, extend: boolean) => void
  /** threads and the composer shown under a clause */
  after: (c: Clause) => ReactNode
}

export interface PlainViewProps {
  diff: PlainDiff
  /** the module's name (its file name without `.plain`) */
  name: string
  /** diffed against a base: show the Changes card and the states */
  diffed: boolean
  /** a sticky index of sections and specs beside the text (on wide containers) */
  index?: boolean
  commenting?: ClauseCommenting
  /** an import / requires chip opens that module */
  onOpenModule?: (name: string) => void
  /** where the module lives (absolute path on the session's host): terms resolve across modules */
  source?: PlainSource | null
  /** open another module (absolute path), e.g. the one a term is defined in */
  onOpenFile?: (path: string) => void
}

export interface PlainSource {
  host: string
  id: string
  path: string
}

interface Ctx {
  prefix: string
  defs: Map<string, string>
  specIds: Set<string>
  hl: boolean
  only: boolean
  commenting?: ClauseCommenting
  jump: (id: string) => void
  resolver: ConceptResolver
  /** this module's path ('' when unknown) */
  path: string
  onOpenFile?: (path: string) => void
  /** inside a definition popover: a term opens there */
  nav?: (name: string) => void
}
const PlainCtx = createContext<Ctx | null>(null)
const useCtx = () => useContext(PlainCtx)!

const BUILTIN = new Set(['UnitTests', 'ConformanceTests', 'AcceptanceTest', 'AcceptanceTests', 'Implementation'])

const changedTree = (nodes: Clause[]) => flatClauses(nodes).some((x) => x.c.state !== 'same')

const TERM = 'mx-px inline cursor-pointer rounded-[4px] px-1 py-px font-sans text-[0.86em] font-medium whitespace-nowrap'

function Term({ name }: { name: string }) {
  const { defs, nav } = useCtx()
  const def = defs.get(name)
  const cls = cn(TERM, def != null ? 'bg-primary/10 text-primary hover:bg-primary/20' : 'bg-muted text-muted-foreground hover:bg-foreground/10 hover:text-foreground')
  if (nav)
    return (
      <button type="button" className={cls} onClick={() => nav(name)}>
        {name}
      </button>
    )
  return (
    <Popover>
      <PopoverTrigger asChild>
        <button type="button" className={cls} title={def != null ? def.replace(/`/g, '') : 'Show the definition'}>
          {name}
        </button>
      </PopoverTrigger>
      <PopoverContent align="start" collisionPadding={12} className="w-[min(32rem,92vw)] gap-0 p-0">
        <ConceptCard start={name} />
      </PopoverContent>
    </Popover>
  )
}

/** `to` relative to the directory of `from` (both absolute). */
function relPath(from: string, to: string) {
  if (!from || !to.startsWith('/')) return to
  const a = from.split('/').slice(0, -1)
  const b = to.split('/')
  let i = 0
  while (i < a.length && i < b.length - 1 && a[i] === b[i]) i++
  const up = a.length - i
  if (up > 3) return to
  return [...Array(up).fill('..'), ...b.slice(i)].join('/')
}

/** A concept's definition, looked up across modules; terms in it open here, with Back. */
function ConceptCard({ start }: { start: string }) {
  const ctx = useCtx()
  const [stack, setStack] = useState([start])
  const name = stack[stack.length - 1]
  const [res, setRes] = useState<{ name: string; hit: ConceptHit | null } | null>(null)
  useEffect(() => {
    let live = true
    ctx.resolver.lookup(name).then(
      (hit) => live && setRes({ name, hit }),
      () => live && setRes({ name, hit: null }),
    )
    return () => {
      live = false
    }
  }, [name, ctx.resolver])
  const hit = res?.name === name ? res.hit : undefined
  const inner = useMemo<Ctx | null>(
    () => (hit ? { ...ctx, defs: definitions(hit.mod), specIds: new Set(), hl: false, only: false, commenting: undefined, nav: (n) => setStack((s) => [...s, n]) } : null),
    [hit, ctx],
  )
  const head = hit ? { ...hit.clause, text: hit.clause.text.replace(/^:[A-Za-z0-9+.\-_]+:\s*/, ''), oldText: undefined } : null
  const where = hit ? relPath(ctx.path, hit.path) : ''
  return (
    <div className="flex max-h-[min(60vh,32rem)] flex-col font-sans">
      <div className="flex items-center gap-1.5 border-b px-3 py-2">
        {stack.length > 1 ? (
          <button type="button" aria-label="Back" onClick={() => setStack((s) => s.slice(0, -1))} className="-ml-1 cursor-pointer rounded p-0.5 text-muted-foreground hover:bg-muted hover:text-foreground">
            <ChevronLeftIcon className="size-4" />
          </button>
        ) : null}
        <span className="min-w-0 flex-1 truncate text-sm font-semibold text-primary">{name}</span>
        {stack.length > 1 ? <span className="truncate text-[0.6875rem] text-dimmer">{stack.slice(0, -1).join(' › ')}</span> : null}
      </div>
      <div className="min-h-0 flex-1 overflow-y-auto px-3 py-2.5 font-serif text-[0.92rem] leading-relaxed [font-family:ui-serif,Georgia,serif]">
        {hit === undefined ? (
          <p className="flex items-center gap-2 font-sans text-sm text-muted-foreground">
            <Loader2Icon className="size-3.5 animate-spin" /> Looking it up…
          </p>
        ) : hit === null || !inner || !head ? (
          <p className="font-sans text-sm text-muted-foreground">
            {BUILTIN.has(name) ? 'Predefined by PlainLang.' : 'No definition in this module, the modules it imports or requires, or any other .plain file of its checkout.'}
          </p>
        ) : (
          <PlainCtx.Provider value={inner}>
            <ClauseList nodes={[head]} />
          </PlainCtx.Provider>
        )}
      </div>
      {hit ? (
        <div className="flex flex-wrap items-center gap-x-3 gap-y-1 border-t bg-muted/40 px-3 py-1.5 text-xs text-muted-foreground">
          {hit.scope === 'here' ? (
            <span>Defined in this module</span>
          ) : (
            <span className={cn('min-w-0', hit.scope === 'elsewhere' && 'text-status-waiting')} title={hit.path}>
              <span className="font-mono">{where}</span>
              {hit.scope === 'imported' && hit.via.length > 1 ? (
                <span className="text-dimmer">
                  {' '}
                  <CornerDownRightIcon className="inline size-3 align-[-2px]" /> via {hit.via.join(' → ')}
                </span>
              ) : null}
              {hit.scope === 'elsewhere' ? ' — not imported by this module' : null}
            </span>
          )}
          <span className="flex-1" />
          {hit.scope === 'here' && !ctx.nav && stack.length === 1 ? (
            <PopoverPrimitive.Close asChild>
              <button type="button" className="cursor-pointer text-primary hover:underline" onClick={() => ctx.jump(`def-${name}`)}>
                Go to definition
              </button>
            </PopoverPrimitive.Close>
          ) : null}
          {hit.scope !== 'here' && ctx.onOpenFile ? (
            <PopoverPrimitive.Close asChild>
              <button type="button" className="inline-flex cursor-pointer items-center gap-0.5 text-primary hover:underline" onClick={() => ctx.onOpenFile?.(hit.path)}>
                Open <ArrowUpRightIcon className="size-3" />
              </button>
            </PopoverPrimitive.Close>
          ) : null}
        </div>
      ) : null}
    </div>
  )
}

function Inline({ text }: { text: string }) {
  const { specIds, jump } = useCtx()
  return inlineParts(text).map((p, i) =>
    p.k === 'text' ? (
      <Fragment key={i}>{p.t}</Fragment>
    ) : p.k === 'code' ? (
      <code key={i} className="rounded-[4px] bg-foreground/[0.07] px-1 py-px font-mono text-[0.84em] [overflow-wrap:anywhere]">
        {p.t}
      </code>
    ) : p.k === 'term' ? (
      <Term key={i} name={p.name} />
    ) : specIds.has(p.id) ? (
      <button key={i} type="button" className="cursor-pointer font-mono text-[0.9em] text-primary hover:underline" onClick={() => jump(specAnchor(p.id))}>
        {p.id}
      </button>
    ) : (
      <span key={i} className="font-mono text-[0.9em] text-muted-foreground">
        {p.id}
      </span>
    ),
  )
}

/** A changed text: the word diff when highlighting, else just the new text. */
function Body({ text, oldText }: { text: string; oldText?: string }) {
  const { hl } = useCtx()
  if (oldText == null || !hl) return <Inline text={text} />
  return wordDiff(oldText, text).map((p, i) => (
    <Fragment key={i}>
      {i ? ' ' : ''}
      {p.k === 'eq' ? (
        <Inline text={p.t} />
      ) : p.k === 'ins' ? (
        <ins className="rounded-[3px] bg-diff-add/15 px-px text-diff-add no-underline shadow-[inset_0_-1px_0] shadow-diff-add/60">
          <Inline text={p.t} />
        </ins>
      ) : (
        <del className="rounded-[3px] bg-diff-del/12 px-px text-diff-del decoration-diff-del/70">
          <Inline text={p.t} />
        </del>
      )}
    </Fragment>
  ))
}

const STATE_TAG: Record<Exclude<ClauseState, 'same'>, string> = {
  added: 'text-diff-add ring-diff-add/50',
  changed: 'text-status-waiting ring-status-waiting/50',
  removed: 'text-diff-del ring-diff-del/50',
}
const STATE_ROW: Record<Exclude<ClauseState, 'same'>, string> = {
  added: 'border-diff-add bg-diff-add/[0.07]',
  changed: 'border-status-waiting bg-status-waiting/[0.08]',
  removed: 'border-diff-del bg-diff-del/[0.06] text-muted-foreground',
}

function StateTag({ state }: { state: ClauseState }) {
  const { hl } = useCtx()
  if (state === 'same' || !hl) return null
  return <span className={cn('mr-1.5 inline-block rounded-[3px] px-1 py-px align-[0.15em] font-sans text-[0.625rem] leading-none font-semibold tracking-wider uppercase ring-1 ring-inset', STATE_TAG[state])}>{state}</span>
}

function ClauseItem({ c, depth }: { c: Clause; depth: number }) {
  const ctx = useCtx()
  const { hl, only, commenting } = ctx
  if (c.state === 'removed' && !hl) return null
  const changedBelow = changedTree(c.children)
  if (only && c.state === 'same' && !changedBelow) return null
  const marked = hl && c.state !== 'same'
  const selected = commenting?.selected(c) ?? false
  return (
    <li className="relative">
      <div
        className={cn(
          'group/clause relative -ml-3 rounded-r-md border-l-[3px] border-transparent py-0.5 pr-2 pl-3',
          marked && STATE_ROW[c.state as Exclude<ClauseState, 'same'>],
          only && c.state === 'same' && 'opacity-60',
          selected && 'bg-primary/10 ring-1 ring-primary/40',
        )}
      >
        {commenting ? (
          <button
            type="button"
            aria-label={`Comment on ${c.start === c.end ? `line ${c.start}` : `lines ${c.start}–${c.end}`}`}
            title={`Comment on this clause (${c.side === 'old' ? 'removed ' : ''}${c.start === c.end ? `line ${c.start}` : `lines ${c.start}–${c.end}`}; shift-click: extend)`}
            onClick={(e) => commenting.onClause(c, e.shiftKey)}
            className={cn(
              'absolute top-1 -left-7 flex size-5 cursor-pointer items-center justify-center rounded bg-primary text-primary-foreground opacity-0 group-hover/clause:opacity-100 focus-visible:opacity-100',
              selected && 'opacity-100',
            )}
          >
            <MessageSquarePlusIcon className="size-3" />
          </button>
        ) : null}
        {marked ? null : depth === 0 ? (
          <span aria-hidden className="absolute top-[0.72em] left-0 size-1.5 rounded-full bg-primary/55" />
        ) : (
          <span aria-hidden className="absolute top-[0.85em] left-0 h-[1.5px] w-1.5 bg-muted-foreground/60" />
        )}
        <StateTag state={c.state} />
        <span className={cn(c.state === 'removed' && hl && 'line-through decoration-diff-del/60')}>
          <Body text={c.text} oldText={c.oldText} />
        </span>
        {c.notes.length ? (
          <span className="mt-0.5 flex flex-wrap gap-1">
            {c.notes.map((n, i) => (
              <span key={i} className="rounded-[4px] bg-muted px-1.5 py-px font-mono text-[0.6875rem] text-muted-foreground [overflow-wrap:anywhere]">
                {n}
              </span>
            ))}
          </span>
        ) : null}
      </div>
      {commenting ? <div className="font-sans">{commenting.after(c)}</div> : null}
      {c.children.length ? <ClauseList nodes={c.children} depth={depth + 1} /> : null}
    </li>
  )
}

function ClauseList({ nodes, depth = 0 }: { nodes: Clause[]; depth?: number }) {
  return (
    <ul className={cn('space-y-1', depth ? 'mt-1 ml-4' : 'ml-3')}>
      {nodes.map((c, i) => (
        <ClauseItem key={`${c.side}:${c.start}:${i}`} c={c} depth={depth} />
      ))}
    </ul>
  )
}

function SectionHead({ title, count, id }: { title: string; count?: number; id?: string }) {
  const { prefix } = useCtx()
  return (
    <h2 id={id ? prefix + id : undefined} className="mb-3 flex scroll-mt-4 items-baseline justify-between border-b pb-1.5 font-sans text-xs font-semibold tracking-wider text-muted-foreground uppercase">
      {title}
      {count != null ? <span className="font-normal tabular-nums">{count}</span> : null}
    </h2>
  )
}

function Definitions({ nodes }: { nodes: Clause[] }) {
  const { prefix, hl, only } = useCtx()
  return (
    <dl className="space-y-3">
      {nodes.map((c, i) => {
        if (c.state === 'removed' && !hl) return null
        const touched = c.state !== 'same' || changedTree(c.children)
        if (only && !touched) return null
        const m = c.text.match(/^:([A-Za-z0-9+.\-_]+):\s*(.*)$/)
        const name = m?.[1] ?? ''
        const head: Clause = m ? { ...c, text: m[2], oldText: c.oldText?.replace(/^:[A-Za-z0-9+.\-_]+:\s*/, '') } : c
        return (
          <div key={`${c.side}:${c.start}:${i}`} id={name ? `${prefix}def-${name}` : undefined} className="scroll-mt-4">
            {name ? (
              <dt className="mb-0.5 font-sans text-sm font-semibold text-primary">
                {name}
              </dt>
            ) : null}
            <dd className="m-0">
              <ClauseList nodes={[{ ...head, children: c.children }]} />
            </dd>
          </div>
        )
      })}
    </dl>
  )
}

function SpecCard({ s, diffed }: { s: Spec; diffed: boolean }) {
  const { prefix, hl, only } = useCtx()
  if (s.state === 'removed' && !hl) return null
  if (only && s.state === 'same' && !s.affectedBy.length) return null
  const tests = s.tests.filter((t) => t.state !== 'removed' || hl)
  return (
    <article
      id={prefix + specAnchor(s.id)}
      className={cn(
        'scroll-mt-4 rounded-lg border bg-card/60 px-4 py-3',
        hl && diffed && s.state === 'added' && 'border-diff-add/50',
        hl && diffed && s.state === 'changed' && 'border-status-waiting/50',
        hl && diffed && s.state === 'removed' && 'border-diff-del/50 opacity-80',
      )}
    >
      <header className="mb-1.5 flex flex-wrap items-center gap-2 font-sans text-xs">
        <span className="font-mono font-semibold text-primary">{s.id}</span>
        {diffed ? <StateTag state={s.state} /> : null}
        {s.tests.length ? (
          <span className="inline-flex items-center gap-1 text-diff-add" title="Has acceptance tests">
            <CheckIcon className="size-3.5" /> {s.tests.length} acceptance test{s.tests.length === 1 ? '' : 's'}
          </span>
        ) : (
          <span className="inline-flex items-center gap-1 text-dimmer" title="No acceptance tests">
            <CircleDashedIcon className="size-3.5" /> no acceptance tests
          </span>
        )}
        {diffed && s.state === 'same' && s.affectedBy.length ? (
          <span className="rounded-[4px] bg-status-waiting/12 px-1.5 py-px text-status-waiting" title="Its text did not change, but a definition it uses did">
            uses changed {s.affectedBy.join(', ')}
          </span>
        ) : null}
      </header>
      <ClauseList nodes={s.clauses} />
      {tests.length && (!only || changedTree(s.tests) || s.state !== 'same') ? (
        <div className="mt-3 rounded-md border border-diff-add/25 bg-diff-add/[0.05] px-3 py-2">
          <div className="mb-1 font-sans text-[0.6875rem] font-semibold tracking-wider text-diff-add uppercase">Acceptance tests</div>
          <ClauseList nodes={s.tests} />
        </div>
      ) : null}
    </article>
  )
}

function touchedLabel(t: Touched) {
  if (t.newSpec) return 'new spec'
  if (t.removedSpec) return 'spec removed'
  const what = [t.added && `${t.added} added`, t.changed && `${t.changed} changed`, t.removed && `${t.removed} removed`].filter(Boolean).join(', ')
  return t.tests ? `acceptance tests: ${what}` : what
}

function Changes({ diff, hl, only, setHl, setOnly }: { diff: PlainDiff; hl: boolean; only: boolean; setHl: (v: boolean) => void; setOnly: (v: boolean) => void }) {
  const { jump } = useCtx()
  const { stats } = diff
  const total = stats.added + stats.changed + stats.removed
  const affected = diff.mod.specs.filter((s) => s.affectedBy.length && s.state === 'same')
  const target = (where: string) => (where === 'description' ? 'description' : SECTIONS.some((x) => x.key === where) ? `sec-${where}` : specAnchor(where))
  return (
    <section className="mt-5 rounded-lg border border-l-4 border-l-status-waiting bg-card px-4 py-3 font-sans text-sm">
      <div className="mb-2 flex flex-wrap items-center gap-x-4 gap-y-1">
        <h2 className="text-xs font-semibold tracking-wider text-muted-foreground uppercase">Changes</h2>
        <span className="ml-auto flex gap-3 text-xs tabular-nums">
          <span className="text-diff-add">{stats.added} added</span>
          <span className="text-status-waiting">{stats.changed} changed</span>
          <span className="text-diff-del">{stats.removed} removed</span>
        </span>
      </div>
      {!total ? <p className="text-muted-foreground">No clause of this module differs.</p> : null}
      <ul className="grid gap-0.5">
        {diff.touched.map((t, i) => (
          <li key={i} className="flex items-baseline gap-3">
            <button type="button" onClick={() => jump(target(t.where))} className="min-w-28 cursor-pointer text-left font-mono text-[0.8125rem] text-primary hover:underline">
              {SECTIONS.find((x) => x.key === t.where)?.title ?? t.where}
            </button>
            <span className="text-muted-foreground">{touchedLabel(t)}</span>
          </li>
        ))}
        {affected.map((s) => (
          <li key={`a-${s.id}`} className="flex items-baseline gap-3">
            <button type="button" onClick={() => jump(specAnchor(s.id))} className="min-w-28 cursor-pointer text-left font-mono text-[0.8125rem] text-primary hover:underline">
              {s.id}
            </button>
            <span className="text-muted-foreground">unchanged, uses changed {s.affectedBy.join(', ')}</span>
          </li>
        ))}
      </ul>
      {total ? (
        <div className="mt-2.5 flex flex-wrap gap-x-5 gap-y-1 border-t pt-2 text-[0.8125rem]">
          <label className="inline-flex cursor-pointer items-center gap-1.5">
            <input type="checkbox" checked={hl} onChange={(e) => setHl(e.target.checked)} className="accent-primary" /> Highlight changes
          </label>
          <label className="inline-flex cursor-pointer items-center gap-1.5">
            <input type="checkbox" checked={only} onChange={(e) => setOnly(e.target.checked)} className="accent-primary" /> Only what changed
          </label>
        </div>
      ) : null}
    </section>
  )
}

function Index({ diff, diffed }: { diff: PlainDiff; diffed: boolean }) {
  const { jump } = useCtx()
  const { mod } = diff
  const item = 'flex w-full cursor-pointer items-baseline gap-2 rounded px-2 py-0.5 text-left hover:bg-muted'
  const dot = (state: ClauseState | 'affected') =>
    diffed && state !== 'same' ? (
      <span aria-hidden className={cn('size-1.5 shrink-0 self-center rounded-full', state === 'added' ? 'bg-diff-add' : state === 'removed' ? 'bg-diff-del' : 'bg-status-waiting', state === 'affected' && 'bg-transparent ring-1 ring-status-waiting')} />
    ) : (
      <span aria-hidden className="size-1.5 shrink-0" />
    )
  return (
    <nav aria-label="Spec index" className="max-h-[85vh] overflow-y-auto py-1 font-sans text-[0.8125rem]">
      {SECTIONS.filter((x) => mod.sections[x.key]?.length).length ? (
        <>
          <div className="mb-1 px-2 text-[0.6875rem] font-semibold tracking-wider text-muted-foreground uppercase">Sections</div>
          <ol className="mb-3">
            {SECTIONS.filter((x) => mod.sections[x.key]?.length).map((x) => (
              <li key={x.key}>
                <button type="button" className={item} onClick={() => jump(`sec-${x.key}`)}>
                  {dot(diff.touchedSections.has(x.key) ? 'changed' : 'same')}
                  <span className="min-w-0 flex-1 truncate">{x.title}</span>
                  <span className="text-dimmer tabular-nums">{mod.sections[x.key].filter((c) => c.state !== 'removed').length}</span>
                </button>
              </li>
            ))}
          </ol>
        </>
      ) : null}
      {mod.specs.length ? (
        <>
          <div className="mb-1 px-2 text-[0.6875rem] font-semibold tracking-wider text-muted-foreground uppercase">Functional specs</div>
          <ol>
            {mod.specs.map((s) => (
              <li key={s.id}>
                <button type="button" className={cn(item, s.state === 'removed' && 'text-dimmer line-through')} onClick={() => jump(specAnchor(s.id))} title={gist(s, 200)}>
                  {dot(s.state === 'same' && s.affectedBy.length ? 'affected' : s.state)}
                  <span className="w-10 shrink-0 font-mono text-primary tabular-nums">{s.id.replace(/^old /, '')}</span>
                  <span className="min-w-0 flex-1 truncate text-muted-foreground">{gist(s)}</span>
                  {s.tests.length ? <CheckIcon className="size-3 shrink-0 self-center text-diff-add" aria-label="has acceptance tests" /> : null}
                </button>
              </li>
            ))}
          </ol>
        </>
      ) : null}
    </nav>
  )
}

/** One PlainLang module, readable. */
export const PlainView = memo(function PlainView({ diff, name, diffed, index = false, commenting, onOpenModule, source, onOpenFile }: PlainViewProps) {
  const prefix = `${useId().replace(/[^A-Za-z0-9_-]/g, '')}-`
  const [hl, setHl] = useState(true)
  const [onlyRaw, setOnly] = useState(false)
  const only = diffed && onlyRaw
  const showHl = diffed && hl
  const { mod } = diff
  const src = source ?? null
  const resolver = usePlainConcepts(src?.host ?? null, src?.id ?? null, src?.path ?? null, mod)
  const ctx = useMemo<Ctx>(
    () => ({
      prefix,
      defs: definitions(mod),
      specIds: new Set(mod.specs.map((s) => s.id)),
      hl: showHl,
      only,
      commenting,
      jump: (id) => document.getElementById(prefix + id)?.scrollIntoView({ block: 'start', behavior: 'smooth' }),
      resolver,
      path: src?.path ?? '',
      onOpenFile,
    }),
    [prefix, mod, showHl, only, commenting, resolver, src?.path, onOpenFile],
  )
  const live = mod.specs.filter((s) => s.state !== 'removed')
  const moduleChip = (m: string) =>
    onOpenModule ? (
      <button key={m} type="button" onClick={() => onOpenModule(m)} className="cursor-pointer rounded-[4px] bg-muted px-1.5 py-px font-mono text-xs text-primary hover:bg-primary/15" title={`Open ${m}.plain`}>
        {m}
      </button>
    ) : (
      <span key={m} className="rounded-[4px] bg-muted px-1.5 py-px font-mono text-xs">
        {m}
      </span>
    )
  const sectionShown = (key: string) => mod.sections[key]?.length && (!only || diff.touchedSections.has(key))

  const text = (
    <div className="min-w-0 max-w-[76ch] font-serif text-[0.97rem] leading-relaxed [font-family:ui-serif,Georgia,'Times_New_Roman',serif]">
      <header className="font-sans">
        <h1 className="text-2xl leading-tight font-semibold">
          {name}
          <span className="font-mono text-[0.75em] font-normal text-dimmer">.plain</span>
        </h1>
        {mod.description ? (
          <p id={`${prefix}description`} className="mt-1.5 font-serif text-[1.02rem] leading-relaxed [font-family:ui-serif,Georgia,serif]">
            <Body text={mod.description} oldText={diff.oldDescription ?? undefined} />
          </p>
        ) : null}
        <div className="mt-2 flex flex-wrap items-center gap-x-4 gap-y-1.5 text-xs text-muted-foreground">
          {mod.import.length ? (
            <span className="flex flex-wrap items-center gap-1">imports {mod.import.map(moduleChip)}</span>
          ) : null}
          {mod.requires.length ? (
            <span className="flex flex-wrap items-center gap-1">requires {mod.requires.map(moduleChip)}</span>
          ) : null}
          {mod.exported.length ? (
            <span className="flex flex-wrap items-center gap-1">
              exports{' '}
              {mod.exported.map((t) => (
                <Term key={t} name={t} />
              ))}
            </span>
          ) : null}
          <span>
            <b className="font-medium text-foreground">{live.length}</b> functional specs, <b className="font-medium text-foreground">{live.filter((s) => s.tests.length).length}</b> with acceptance tests
          </span>
        </div>
      </header>
      {diffed ? <Changes diff={diff} hl={hl} only={onlyRaw} setHl={setHl} setOnly={setOnly} /> : null}
      {mod.notes.length && !only ? (
        <aside className="mt-5 rounded-md border border-status-waiting/30 bg-status-waiting/[0.07] px-4 py-2.5 font-sans text-sm">
          {mod.notes.map((n, i) => (
            <p key={i} className="my-1">
              <Inline text={n.text} />
            </p>
          ))}
        </aside>
      ) : null}
      {sectionShown('definitions') ? (
        <section className="mt-8">
          <SectionHead id="sec-definitions" title="Definitions" count={mod.sections['definitions'].filter((c) => c.state !== 'removed').length} />
          <Definitions nodes={mod.sections['definitions']} />
        </section>
      ) : null}
      {SECTIONS.slice(1).map((x) =>
        sectionShown(x.key) ? (
          <section key={x.key} className="mt-8">
            <SectionHead id={`sec-${x.key}`} title={x.title} count={mod.sections[x.key].filter((c) => c.state !== 'removed').length} />
            <ClauseList nodes={mod.sections[x.key]} />
          </section>
        ) : null,
      )}
      {Object.keys(mod.sections)
        .filter((k) => k !== SPEC_SECTION && !SECTIONS.some((x) => x.key === k) && sectionShown(k))
        .map((k) => (
          <section key={k} className="mt-8">
            <SectionHead title={k} />
            <ClauseList nodes={mod.sections[k]} />
          </section>
        ))}
      {mod.specs.length ? (
        <section className="mt-8">
          <SectionHead id="sec-specs" title="Functional specs" count={live.length} />
          <div className="space-y-3">
            {mod.specs.map((s) => (
              <SpecCard key={s.id} s={s} diffed={diffed} />
            ))}
          </div>
        </section>
      ) : null}
    </div>
  )

  return (
    <PlainCtx.Provider value={ctx}>
      {index ? (
        <div className="@container">
          <div className="grid gap-8 @3xl:grid-cols-[220px_minmax(0,1fr)]">
            <div className="sticky top-0 hidden max-h-[85vh] self-start @3xl:block">
              <Index diff={diff} diffed={diffed} />
            </div>
            {text}
          </div>
        </div>
      ) : (
        text
      )}
    </PlainCtx.Provider>
  )
})
