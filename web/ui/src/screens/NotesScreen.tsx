import { useEffect, useMemo, useRef, useState, type KeyboardEvent, type Ref } from 'react'
import { ChevronLeftIcon, CodeXmlIcon, NotebookTextIcon, SearchIcon, XIcon } from 'lucide-react'
import { useLocation } from 'wouter'

import { HostDot } from '@/components/HostBadge'
import { NoteResults, RecentNotes } from '@/components/notes/NoteResults'
import { NoteTree } from '@/components/notes/NoteTree'
import { NoteView } from '@/components/notes/NoteView'
import { ScreenHeader } from '@/components/ScreenHeader'
import { Button } from '@/components/ui/button'
import { Input } from '@/components/ui/input'
import { Kbd } from '@/components/ui/kbd'
import { useNotesHosts, useNotesSearch, useNotesTree } from '@/hooks/useNotes'
import { usePersistentState } from '@/hooks/usePersistentState'
import { useSwipeBack } from '@/hooks/useSwipeBack'
import { ancestorDirs, buildTree, highlightTerms, notesHref, parseNotesLocation, recentNotes } from '@/lib/notes'
import { shortcutHint } from '@/lib/shortcuts'
import { storage } from '@/lib/storage'
import { cn } from '@/lib/utils'

// The query outlives the screen (note → back → the same results).
let lastQuery = ''

const openKey = (host: string) => `fleet.notesOpen.${host}`
function loadOpen(host: string | null): Set<string> {
  if (!host) return new Set()
  try {
    const v = JSON.parse(storage.get(openKey(host)) ?? '[]')
    return new Set(Array.isArray(v) ? v.filter((x) => typeof x === 'string') : [])
  } catch {
    return new Set()
  }
}

export interface NotesScreenProps {
  /** `screen` = mobile (stacked: browse/search → note, swipe back); `pane` = the desktop layout. */
  layout?: 'screen' | 'pane'
  /** Desktop: `/` focuses this field. */
  searchRef?: Ref<HTMLInputElement>
}

/**
 * `#/notes[/<host>[/<path>]]`: browse (folder tree), search (debounced, snippets with the
 * matches marked) and read the markdown notes of any host that has `web.notes.root`.
 */
export function NotesScreen({ layout = 'screen', searchRef }: NotesScreenProps) {
  const pane = layout === 'pane'
  const [location, navigate] = useLocation()
  const loc = parseNotesLocation(location) ?? { host: null, path: null }
  const hosts = useNotesHosts()
  const [lastHost, setLastHost] = usePersistentState<string>('fleet.notesHost', '')
  const host = loc.host ?? hosts.find((h) => h.name === lastHost)?.name ?? hosts[0]?.name ?? null
  useEffect(() => {
    if (loc.host && loc.host !== lastHost) setLastHost(loc.host)
  }, [loc.host, lastHost, setLastHost])

  const { tree, index, error } = useNotesTree(host)
  const root = useMemo(() => buildTree(tree?.files ?? []), [tree])
  const recent = useMemo(() => recentNotes(tree?.files ?? []), [tree])
  const entry = loc.path ? (index.byPath.get(loc.path) ?? null) : null

  const [q, setQState] = useState(lastQuery)
  const setQ = (v: string) => {
    lastQuery = v
    setQState(v)
  }
  const search = useNotesSearch(host, q)
  const highlight = useMemo(() => highlightTerms(q), [q])
  const [hitPath, setHitPath] = useState<string | null>(null)

  // Expanded folders, per host; the open note's folders are expanded when it opens.
  const [openState, setOpenState] = useState<{ host: string | null; open: Set<string> }>(() => ({ host, open: loadOpen(host) }))
  const open = openState.host === host ? openState.open : loadOpen(host)
  const saveOpen = (next: Set<string>) => {
    setOpenState({ host, open: next })
    if (host) storage.set(openKey(host), JSON.stringify([...next]))
  }
  const toggle = (dir: string) => {
    const next = new Set(open)
    if (next.has(dir)) next.delete(dir)
    else next.add(dir)
    saveOpen(next)
  }
  const [expandedFor, setExpandedFor] = useState<string | null>(null)
  if (loc.path && host && expandedFor !== `${host}/${loc.path}`) {
    setExpandedFor(`${host}/${loc.path}`)
    const missing = ancestorDirs(loc.path).filter((d) => !open.has(d))
    if (missing.length) {
      const next = new Set([...open, ...missing])
      setOpenState({ host, open: next })
      storage.set(openKey(host), JSON.stringify([...next]))
    }
  }

  // Desktop: keep the open note's tree row in view.
  const treeRef = useRef<HTMLDivElement>(null)
  useEffect(() => {
    if (!pane || !loc.path) return
    treeRef.current?.querySelector(`[data-note-path="${CSS.escape(loc.path)}"]`)?.scrollIntoView({ block: 'nearest' })
  }, [pane, loc.path, tree])

  const back = () => {
    if (window.history.length > 1) window.history.back()
    else navigate(loc.path && host ? notesHref(host) : '/', { replace: true })
  }
  const screenRef = useRef<HTMLDivElement>(null)
  useSwipeBack(screenRef, back, !pane)

  const pickHit = (path: string) => setHitPath(path)
  const onSearchKey = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Escape') {
      if (q) {
        e.preventDefault()
        e.stopPropagation()
        setQ('')
      } else e.currentTarget.blur()
    } else if (e.key === 'Enter' && search.data?.results[0] && host) {
      e.preventDefault()
      pickHit(search.data.results[0].path)
      navigate(notesHref(host, search.data.results[0].path))
    }
  }

  const hostPicker =
    hosts.length > 1 ? (
      <div role="radiogroup" aria-label="Host" className="no-scrollbar flex gap-1 overflow-x-auto">
        {hosts.map((h) => (
          <button
            key={h.name}
            type="button"
            role="radio"
            aria-checked={h.name === host}
            onClick={() => navigate(notesHref(h.name))}
            className={cn(
              'flex shrink-0 items-center gap-1.5 rounded-full border px-2.5 text-xs outline-none focus-visible:ring-2 focus-visible:ring-ring/50',
              pane ? 'h-7' : 'h-9',
              h.name === host ? 'border-primary/40 bg-primary/10 text-foreground' : 'text-muted-foreground hover:bg-muted/60',
            )}
          >
            <HostDot host={h.name} />
            {h.name}
          </button>
        ))}
      </div>
    ) : null

  const searchField = (
    <div className="relative min-w-0 flex-1">
      <SearchIcon className="pointer-events-none absolute top-1/2 left-2.5 size-4 -translate-y-1/2 text-dimmer" />
      <Input
        ref={searchRef}
        type="search"
        value={q}
        onChange={(e) => setQ(e.target.value)}
        onKeyDown={onSearchKey}
        placeholder={tree ? `Search ${tree.name}…` : 'Search notes…'}
        aria-label="Search notes"
        enterKeyHint="search"
        autoCapitalize="off"
        autoCorrect="off"
        spellCheck={false}
        className={cn('pr-8 pl-8', pane ? 'h-8 text-sm' : 'h-10 text-base')}
      />
      {q ? (
        <button type="button" aria-label="Clear search" onClick={() => setQ('')} className="absolute top-1/2 right-1 flex size-8 -translate-y-1/2 items-center justify-center rounded-md text-dimmer hover:text-foreground">
          <XIcon className="size-4" />
        </button>
      ) : pane ? (
        <Kbd className="pointer-events-none absolute top-1/2 right-2 -translate-y-1/2">{shortcutHint('search')}</Kbd>
      ) : null}
    </div>
  )

  const notice = !hosts.length ? (
    <Empty title="No notes configured">
      Point a host at a folder of markdown notes: <code className="font-mono">fleet config set web.notes.root ~/notes</code>, then restart its web server.
    </Empty>
  ) : error && !tree ? (
    <Empty title="Notes unavailable">{error}</Empty>
  ) : null

  const results = q.trim() && host ? (
    <NoteResults host={host} data={search.data} loading={search.loading} error={search.error} selected={loc.path} onPick={(r) => pickHit(r.path)} compact={pane} />
  ) : null

  const noteView =
    loc.path && host ? (
      entry || !tree ? (
        <NoteView
          key={`${host}/${loc.path}`}
          host={host}
          path={loc.path}
          entry={entry}
          index={index}
          highlight={q.trim() ? highlight : []}
          scrollToHit={hitPath === loc.path}
          desktop={pane}
          header={
            pane ? undefined : (
              <ScreenHeader>
                <div className="-ml-2 flex min-h-8 items-center gap-1">
                  <Button variant="ghost" size="icon" aria-label="Back" onClick={back} className="-my-1.5 size-11 shrink-0 rounded-xl">
                    <ChevronLeftIcon className="size-6" />
                  </Button>
                  <span className="min-w-0 flex-1 truncate text-sm text-muted-foreground">{tree?.name ?? 'Notes'}</span>
                </div>
              </ScreenHeader>
            )
          }
        />
      ) : (
        <Empty title="Not found">
          <span className="font-mono">{loc.path}</span> is not in {tree.name} (moved, excluded or hidden).
        </Empty>
      )
    ) : null

  if (pane) {
    return (
      <section aria-label="Notes" className="flex h-full min-w-0 flex-1 flex-col bg-background">
        <header
          data-tauri-drag-region="deep"
          className="titlebar titlebar-lead flex shrink-0 items-center gap-2 border-b py-1.5 pr-2 pl-4 [--titlebar-pad:1rem]"
        >
          <h1 className="flex shrink-0 items-center gap-1.5 text-[0.9375rem] font-bold">
            <NotebookTextIcon className="size-4 text-muted-foreground" /> {tree?.name ?? 'Notes'}
          </h1>
          {hostPicker}
          <div className="mx-2 flex max-w-xl min-w-0 flex-1">{searchField}</div>
          {tree ? <span className="hidden truncate font-mono text-[0.6875rem] text-dimmer xl:inline">{tree.root}</span> : null}
          {tree?.editorUrl ? (
            <Button asChild variant="ghost" size="icon" className="shrink-0 text-muted-foreground">
              <a href={tree.editorUrl} aria-label="Open the notes folder in the editor" title={`Open ${tree.root} in ${/^cursor:/.test(tree.editorUrl) ? 'Cursor' : 'VS Code'}`}>
                <CodeXmlIcon />
              </a>
            </Button>
          ) : null}
        </header>
        {notice ?? (
          <div className="flex min-h-0 flex-1">
            <nav ref={treeRef} aria-label="Folders" className="w-64 shrink-0 overflow-y-auto border-r px-1.5 xl:w-72">
              {host && tree ? <NoteTree host={host} root={root} open={open} onToggle={toggle} selected={loc.path} compact /> : null}
              {tree?.truncated ? <p className="px-2 py-2 text-[0.6875rem] text-dimmer">Only the first {tree.files.length} files are listed.</p> : null}
            </nav>
            <div className="w-80 shrink-0 overflow-y-auto border-r xl:w-96">{results ?? (host ? <RecentNotes host={host} notes={recent} selected={loc.path} compact /> : null)}</div>
            <main className="flex min-w-0 flex-1">
              {noteView ?? (
                <Empty title={tree ? `${tree.files.length} files in ${tree.name}` : 'Loading…'}>Pick a note from the tree, or search with /.</Empty>
              )}
            </main>
          </div>
        )}
      </section>
    )
  }

  if (noteView) {
    return (
      <div ref={screenRef} className="fixed-app flex flex-col bg-background">
        {noteView}
      </div>
    )
  }

  return (
    <div ref={screenRef} className="flex min-h-app flex-col bg-background">
      <ScreenHeader>
        <div className="-ml-2 flex min-h-8 items-center gap-1">
          <Button variant="ghost" size="icon" aria-label="Back" onClick={() => navigate('/')} className="-my-1.5 size-11 shrink-0 rounded-xl">
            <ChevronLeftIcon className="size-6" />
          </Button>
          <h1 className="min-w-0 flex-1 truncate text-xl font-bold tracking-tight">{tree?.name ?? 'Notes'}</h1>
        </div>
        <div className="mt-2 flex flex-col gap-2">
          {searchField}
          {hostPicker}
        </div>
      </ScreenHeader>
      <div className="mx-auto w-full max-w-3xl pb-[max(1.5rem,env(safe-area-inset-bottom))]">
        {notice ??
          results ?? (
            <>
              {host ? <RecentNotes host={host} notes={recent.slice(0, 5)} selected={null} /> : null}
              {host && tree ? (
                <section aria-label="Folders" className="px-1.5">
                  <h2 className="px-2.5 pt-3 pb-1 text-[0.6875rem] font-semibold tracking-wider text-dimmer uppercase">All notes</h2>
                  <NoteTree host={host} root={root} open={open} onToggle={toggle} selected={null} />
                </section>
              ) : null}
            </>
          )}
      </div>
    </div>
  )
}

function Empty({ title, children }: { title: string; children?: React.ReactNode }) {
  return (
    <div className="flex flex-1 flex-col items-center justify-center gap-1.5 px-6 py-16 text-center">
      <p className="text-sm font-semibold">{title}</p>
      {children ? <p className="max-w-md text-sm text-dimmer">{children}</p> : null}
    </div>
  )
}
