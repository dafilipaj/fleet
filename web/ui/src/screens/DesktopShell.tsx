import { useCallback, useEffect, useMemo, useRef, useState, type CSSProperties, type PointerEvent as ReactPointerEvent } from 'react'
import {
  BotIcon,
  CircleDotIcon,
  CodeXmlIcon,
  CopyIcon,
  FileTextIcon,
  FilterIcon,
  GaugeIcon,
  GitPullRequestIcon,
  InboxIcon,
  KanbanIcon,
  KeyboardIcon,
  Link2Icon,
  ListIcon,
  FileDiffIcon,
  MessageSquareTextIcon,
  MonitorIcon,
  MoonIcon,
  NotebookPenIcon,
  NotebookTextIcon,
  PaletteIcon,
  PanelRightIcon,
  PencilIcon,
  PlusIcon,
  RefreshCwIcon,
  Settings2Icon,
  SparklesIcon,
  SquareCheckBigIcon,
  SquareTerminalIcon,
  SunIcon,
} from 'lucide-react'
import { toast } from 'sonner'
import { Redirect, useLocation, useRoute } from 'wouter'

import { api, sessionErrorMessage } from '@/api/client'
import type { Session } from '@/api/types'
import { Board } from '@/components/board/Board'
import { CommandPalette, type PaletteAction } from '@/components/desktop/CommandPalette'
import { ShortcutsDialog } from '@/components/desktop/ShortcutsDialog'
import { Sidebar } from '@/components/desktop/Sidebar'
import { TopBar } from '@/components/desktop/TopBar'
import { NewSessionDialog } from '@/components/NewSessionDrawer'
import { StatusDot } from '@/components/StatusDot'
import { Kbd } from '@/components/ui/kbd'
import { useFleet } from '@/hooks/useFleet'
import { focusLeft, toggleFocus, useFocus } from '@/hooks/useFocus'
import { useBoardColumns, useGroups, useViewMode } from '@/hooks/useGroups'
import { useNotesHosts, useNotesTree } from '@/hooks/useNotes'
import { WIDE_QUERY } from '@/hooks/useMediaQuery'
import { useNow } from '@/hooks/useNow'
import { usePersistentState } from '@/hooks/usePersistentState'
import { useSessionList } from '@/hooks/useSessionList'
import { useSettings } from '@/hooks/useSettings'
import { openStackLink } from '@/hooks/useStackUi'
import { useTheme } from '@/hooks/useTheme'
import { openTitleEditor, startEditing, useSessionTitle } from '@/hooks/useTitles'
import { editorLabel } from '@/lib/brief'
import { canAttachInApp, copyWithToast, sessionAttachCommand, sessionAttachLink } from '@/lib/clipboard'
import { boardOrder, type ViewMode } from '@/lib/groups'
import { parseMemosLocation } from '@/lib/memos'
import { notesHref, parseNotesLocation } from '@/lib/notes'
import { scratchpad } from '@/lib/scratchpad'
import { PALETTES } from '@/lib/palettes'
import { stacksKnown } from '@/lib/stacks'
import { STATUS_FILTERS, allSessions, byLastActivity, findSession, sessionHref, statusLabel } from '@/lib/sessions'
import {
  clampFlyoutWidth,
  clampSidebarWidth,
  detailsFitBeside,
  FLYOUT_KEY_STEP,
  FLYOUT_MIN_W,
  flyoutMaxWidth,
  SIDEBAR_DEFAULT_W,
  SIDEBAR_MAX_W,
  SIDEBAR_MIN_W,
} from '@/lib/layout'
import {
  APP_COMMAND_EVENT,
  APP_COMMANDS,
  isMacPlatform,
  isTypingTarget,
  matchShortcut,
  sessionKey,
  SHOW_SHORTCUTS_EVENT,
  shortcutHint,
  stepCursor,
  type ShortcutAction,
} from '@/lib/shortcuts'
import { sessionTitle } from '@/lib/title'
import { cn } from '@/lib/utils'
import { NotesScreen } from '@/screens/NotesScreen'
import { PrsScreen } from '@/screens/PrsScreen'
import { TasksScreen } from '@/screens/TasksScreen'
import { ReviewScreen } from '@/screens/ReviewScreen'
import { isPrsLocation } from '@/lib/prs'
import { isTasksLocation } from '@/lib/tasks'
import { parseReviewLocation, reviewHref } from '@/lib/review'
import { SessionScreen, type PaneApi } from '@/screens/SessionScreen'
import { SettingsScreen } from '@/screens/SettingsScreen'
import { UsageScreen } from '@/screens/UsageScreen'
import { InboxScreen } from '@/screens/InboxScreen'
import { JobsScreen } from '@/screens/JobsScreen'
import { MemosScreen } from '@/screens/MemosScreen'
import { OperatorScreen } from '@/screens/OperatorScreen'

const parseBool01 = (raw: string) => (raw === '1' ? true : raw === '0' ? false : undefined)

/**
 * ≥ lg: master–detail. Resizable sidebar (the session list) + the selected
 * session as a pane (+ an optional details column). Same hash routes as mobile:
 * `#/` = nothing selected, `#/s/:host/:id` = that session open. One window keydown
 * listener drives the keyboard shortcuts (lib/shortcuts.ts).
 */
export function DesktopShell() {
  const now = useNow(1000)
  const focus = useFocus()
  const list = useSessionList(now)
  const { fleet, refresh, dropSession } = useFleet()
  const { setTheme, setPalette } = useTheme()
  const [location, navigate] = useLocation()
  const [match, params] = useRoute('/s/:host/:id')
  const [settingsOpen] = useRoute('/settings')
  const [usageOpen] = useRoute('/usage')
  const [jobsOpen] = useRoute(/^\/(?:bots|jobs)$/)
  const [inboxOpen] = useRoute('/inbox')
  const [operatorOpen] = useRoute('/operator')
  const memosOpen = parseMemosLocation(location) !== null
  const settings = useSettings()
  // `#/prs`: the pull requests board, in the pane.
  const prsOpen = isPrsLocation(location)
  // `#/tasks`: the tasks board too.
  const tasksOpen = isTasksLocation(location)
  // `#/notes…`: the notes explorer takes the whole window (no sidebar / board).
  const notesLoc = parseNotesLocation(location)
  const notesOpen = notesLoc !== null
  // `#/review/:host/:id`: the diff reviewer (with the session beside it) takes the whole window.
  const reviewLoc = parseReviewLocation(location)
  const reviewOpen = reviewLoc !== null
  const notesHosts = useNotesHosts()
  const selected = match ? { host: params.host, id: params.id } : null
  const selectedKey = selected ? `${selected.host}/${selected.id}` : null
  const selectedSession = selected ? findSession(fleet, selected.host, selected.id) : null

  const [sidebarW, setSidebarW] = usePersistentState<number>('fleet.sidebarWidth', SIDEBAR_DEFAULT_W, (r) => clampSidebarWidth(Number(r)))
  const [inspectorRaw, setInspectorRaw] = usePersistentState<string>(
    'fleet.inspector',
    typeof window !== 'undefined' && window.matchMedia?.(WIDE_QUERY).matches ? '1' : '0',
    (r) => (parseBool01(r) === undefined ? undefined : r),
  )
  // The Details column: the brief first, then the session's details (⌘I).
  const inspector = inspectorRaw === '1'
  const toggleInspector = () => setInspectorRaw(inspector ? '0' : '1')

  // List | Board (`fleet.view`). Board: the grouped Kanban replaces the sidebar; an open
  // session is a flyout over its right edge (the board never relayouts), so ↑/↓, Enter and
  // Esc work the same way.
  const [mode, setMode] = useViewMode()
  const board = mode === 'board'
  const groups = useGroups(board)
  const { columns, moveColumn } = useBoardColumns(board, list.view.sessions, groups.groups, fleet, list.host, list.query)
  // The cursor walks the sessions in on-screen order: the list, or the board column by column.
  const ordered = useMemo(() => (board ? boardOrder(columns) : list.view.sessions), [board, columns, list.view.sessions])

  const [newOpen, setNewOpen] = useState(false)
  const [paletteOpen, setPaletteOpen] = useState(false)
  const [helpOpen, setHelpOpen] = useState(false)
  useEffect(() => {
    const open = () => setHelpOpen(true)
    window.addEventListener(SHOW_SHORTCUTS_EVENT, open)
    return () => window.removeEventListener(SHOW_SHORTCUTS_EVENT, open)
  }, [])

  // --- cursor (↑/↓) — follows the selection when it changes ------------------
  const [cursor, setCursor] = useState<string | null>(selectedKey)
  const [prevSelected, setPrevSelected] = useState(selectedKey)
  if (prevSelected !== selectedKey) {
    setPrevSelected(selectedKey)
    if (selectedKey) setCursor(selectedKey)
  }
  const keys = useMemo(() => ordered.map(sessionKey), [ordered])
  const byKey = useMemo(() => new Map(ordered.map((s) => [sessionKey(s), s])), [ordered])
  const cursorKey = cursor && keys.includes(cursor) ? cursor : null

  // Opened with Enter → the new pane focuses its composer on mount.
  const [focusOnOpen, setFocusOnOpen] = useState<string | null>(null)
  const paneRef = useRef<PaneApi | null>(null)
  const searchRef = useRef<HTMLInputElement>(null)
  const notesSearchRef = useRef<HTMLInputElement>(null)

  const openSession = useCallback(
    (s: Session, focus: boolean) => {
      const key = sessionKey(s)
      setCursor(key)
      if (key === selectedKey) {
        if (focus) paneRef.current?.focusComposer()
        return
      }
      setFocusOnOpen(focus ? key : null)
      navigate(sessionHref(s))
    },
    [navigate, selectedKey],
  )

  // Board is for the overview: it closes the open session. List is for working through the
  // top: it opens the first session.
  const switchView = (v: ViewMode) => {
    if (v === mode) return
    setMode(v)
    if (v === 'board') {
      if (selectedKey) navigate('/')
    } else if (list.view.sessions[0]) {
      openSession(list.view.sessions[0], false)
    }
  }
  const toggleView = () => switchView(board ? 'list' : 'board')

  // Focus moves that must wait for the next commit (the cursor row may not be rendered
  // yet). An effect, not requestAnimationFrame: rAF never fires
  // in a background tab, and a late focus would steal the caret from the composer.
  const [focusReq, setFocusReq] = useState<{ to: 'search' | 'row'; key?: string; n: number } | null>(null)
  useEffect(() => {
    if (!focusReq) return
    if (focusReq.to === 'search') {
      searchRef.current?.focus()
      searchRef.current?.select()
    } else if (focusReq.key) {
      document.querySelector<HTMLElement>(`[data-session-list] [data-session-key="${CSS.escape(focusReq.key)}"]`)?.focus()
    }
  }, [focusReq])

  const focusSearch = () => {
    if (notesOpen) {
      notesSearchRef.current?.focus()
      notesSearchRef.current?.select()
      return
    }
    setFocusReq((r) => ({ to: 'search', n: (r?.n ?? 0) + 1 }))
  }

  const moveCursor = (delta: number) => {
    const next = stepCursor(keys, cursorKey ?? selectedKey, delta)
    setCursor(next)
    // Keyboard focus on a row follows the cursor (Tab-then-↑/↓ stays coherent).
    if (next && document.activeElement?.closest('[data-session-list]')) setFocusReq((r) => ({ to: 'row', key: next, n: (r?.n ?? 0) + 1 }))
  }

  // --- document title: "(2) name · Fleet" -------------------------------------
  const waiting = list.summary.waiting
  const { title: selectedTitle } = useSessionTitle(selectedSession, selectedKey ?? undefined)
  useEffect(() => {
    const name = selectedSession ? selectedTitle : ''
    document.title = `${waiting > 0 ? `(${waiting}) ` : ''}${name ? `${name} · ` : ''}Fleet`
  }, [waiting, selectedSession, selectedTitle])
  useEffect(() => () => void (document.title = 'Fleet'), [])

  // --- keyboard ----------------------------------------------------------------
  const dialogOpen = newOpen || paletteOpen || helpOpen

  // ⌘⌫ closes a session in two presses: the first arms it (3 s, with a toast), the second kills
  // Claude + its terminal — the same confirm-by-repeating as the Details panel's button.
  const closeArm = useRef<{ key: string; until: number; toast: string | number } | null>(null)
  const closeSession = (s: Session) => {
    const key = sessionKey(s)
    const name = sessionTitle(s)
    const armed = closeArm.current
    if (!armed || armed.key !== key || Date.now() > armed.until) {
      const t = toast(`Press ${shortcutHint('close')} again to close “${name}”`, { duration: 3000 })
      closeArm.current = { key, until: Date.now() + 3000, toast: t }
      return
    }
    toast.dismiss(armed.toast)
    closeArm.current = null
    const pending = toast.loading(`Closing “${name}”…`)
    api
      .kill(s.host, s.session_id)
      .then((res) => {
        toast.success(`Closed ${res.name || name}`, { id: pending, description: String(res.terminal || 'done').replace(/-/g, ' ') })
        dropSession(s.host, s.session_id)
        setTimeout(refresh, 2500)
        if (selectedKey === key) navigate('/', { replace: true })
      })
      .catch((err) => toast.error('Close failed', { id: pending, description: sessionErrorMessage(err) }))
  }

  const run = (action: ShortcutAction, target: HTMLElement | null): boolean => {
    const cur = cursorKey ? byKey.get(cursorKey) : null
    // The session list isn't on screen in the notes explorer: its keys do nothing there.
    if (notesOpen && !['search', 'new', 'back', 'blur', 'palette', 'help', 'notes', 'scratchpad'].includes(action)) return false
    if (reviewOpen && !['new', 'back', 'blur', 'palette', 'help', 'notes', 'scratchpad'].includes(action)) return false
    switch (action) {
      case 'next':
        moveCursor(1)
        return true
      case 'prev':
        moveCursor(-1)
        return true
      case 'openAndReply': {
        // A focused link / button handles Enter itself.
        if (target?.closest('a, button, [role="button"], [role="radio"], [role="option"], [role="switch"]')) return false
        const s = cur ?? (selectedKey ? null : ordered[0])
        if (s) openSession(s, true)
        else if (selectedKey) paneRef.current?.focusComposer()
        return true
      }
      case 'search':
        // Again from the search field: the browser's own find in page.
        if (target && (target === searchRef.current || target === notesSearchRef.current)) return false
        focusSearch()
        return true
      case 'new':
        setNewOpen(true)
        return true
      case 'back':
        // Esc in the diff reviewer: back to its session.
        if (reviewLoc) {
          navigate(sessionHref({ host: reviewLoc.host, session_id: reviewLoc.id }))
          return true
        }
        if (selectedKey || settingsOpen || usageOpen || jobsOpen || inboxOpen || operatorOpen || memosOpen || notesOpen || prsOpen || tasksOpen) {
          navigate('/')
          return true
        }
        return false
      case 'blur':
        target?.blur()
        return true
      case 'mode':
        paneRef.current?.toggleMode()
        return !!paneRef.current
      case 'view':
        toggleView()
        return true
      case 'inspector':
        if (!selectedKey) return false
        toggleInspector()
        return true
      case 'palette':
        setPaletteOpen((o) => !o)
        return true
      case 'help':
        setHelpOpen((o) => !o)
        return true
      case 'notes':
        navigate(notesOpen ? '/' : notesHref())
        return true
      case 'operator':
        navigate(operatorOpen ? '/' : '/operator')
        return true
      case 'memos':
        navigate(memosOpen ? '/' : '/memos')
        return true
      case 'prs':
        navigate(prsOpen ? '/' : '/prs')
        return true
      case 'tasks':
        navigate(tasksOpen ? '/' : '/tasks')
        return true
      case 'inbox':
        navigate(inboxOpen ? '/' : '/inbox')
        return true
      case 'jobs':
        navigate(jobsOpen ? '/' : '/bots')
        return true
      case 'usage':
        navigate(usageOpen ? '/' : '/usage')
        return true
      case 'settings':
        navigate(settingsOpen ? '/' : '/settings')
        return true
      case 'scratchpad':
        scratchpad.toggle(!!target?.closest('[data-scratchpad]'))
        return true
      case 'close': {
        const s = selectedSession ?? cur
        if (!s) return false
        closeSession(s)
        return true
      }
      case 'rename':
        // The open session's header, else the row / card under the cursor.
        if (selectedSession && selectedKey) openTitleEditor('header', selectedKey)
        else if (cur) openTitleEditor(board ? 'card' : 'row', cursorKey!)
        else return false
        return true
    }
  }
  const runRef = useRef(run)
  useEffect(() => {
    runRef.current = run
  })
  const dialogRef = useRef({ any: dialogOpen, help: helpOpen, others: newOpen || paletteOpen })
  useEffect(() => {
    dialogRef.current = { any: dialogOpen, help: helpOpen, others: newOpen || paletteOpen }
  }, [dialogOpen, helpOpen, newOpen, paletteOpen])

  useEffect(() => {
    const mac = isMacPlatform()
    const anyDialog = () => dialogRef.current.any || !!document.querySelector('[role="dialog"][data-state="open"], [data-vaul-drawer][data-state="open"]')
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.defaultPrevented) return
      const target = (e.target instanceof HTMLElement ? e.target : null) as HTMLElement | null
      const typing = isTypingTarget(target)
      const arrowsFree = !target || target === document.body || !!target.closest('[data-session-list]')
      const action = matchShortcut(e, { mac, typing, arrowsFree })
      if (!action) return
      // With a dialog open only ⌘K toggles the palette (and ⌘? closes the help); the dialog owns
      // every other key.
      if (anyDialog() && action !== 'palette' && !(action === 'help' && dialogRef.current.help && !dialogRef.current.others)) return
      if (runRef.current(action, target)) e.preventDefault()
    }
    // Fleet.app's native menu (New Session ⌘N — a combo browsers keep for themselves).
    const onCommand = (e: Event) => {
      const action = (e as CustomEvent<unknown>).detail as ShortcutAction
      if (!APP_COMMANDS.includes(action) || anyDialog()) return
      runRef.current(action, null)
    }
    window.addEventListener('keydown', onKeyDown)
    window.addEventListener(APP_COMMAND_EVENT, onCommand)
    return () => {
      window.removeEventListener('keydown', onKeyDown)
      window.removeEventListener(APP_COMMAND_EVENT, onCommand)
    }
  }, [])

  // --- sidebar resize ------------------------------------------------------------
  const drag = useRef<{ x: number; w: number } | null>(null)
  const onResizeDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    e.preventDefault()
    e.currentTarget.setPointerCapture(e.pointerId)
    drag.current = { x: e.clientX, w: sidebarW }
  }
  const onResizeMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!drag.current) return
    setSidebarW(clampSidebarWidth(drag.current.w + e.clientX - drag.current.x))
  }
  const onResizeUp = () => {
    drag.current = null
  }

  // --- board flyout: width pref (`fleet.flyoutWidth`, 0 = default share), bounded by the board --
  const boardAreaRef = useRef<HTMLDivElement>(null)
  const [boardW, setBoardW] = useState(() => (typeof window !== 'undefined' ? window.innerWidth : 1440))
  useEffect(() => {
    const el = boardAreaRef.current
    if (!el || typeof ResizeObserver === 'undefined') return
    // An unmounting board reports 0 wide: keep the last real width.
    const ro = new ResizeObserver(([entry]) => entry && entry.contentRect.width > 0 && setBoardW(entry.contentRect.width))
    ro.observe(el)
    return () => ro.disconnect()
  }, [board])
  const [flyoutPref, setFlyoutPref] = usePersistentState<number>('fleet.flyoutWidth', 0, (r) => {
    const n = Number(r)
    return Number.isFinite(n) && n >= 0 ? Math.round(n) : undefined
  })
  const flyoutW = clampFlyoutWidth(flyoutPref, boardW)
  const flyoutDrag = useRef<{ x: number; w: number } | null>(null)
  const onFlyoutDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    e.preventDefault()
    e.currentTarget.setPointerCapture(e.pointerId)
    flyoutDrag.current = { x: e.clientX, w: flyoutW }
  }
  // The handle is the flyout's left edge: dragging left widens it.
  const onFlyoutMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!flyoutDrag.current) return
    setFlyoutPref(clampFlyoutWidth(flyoutDrag.current.w - (e.clientX - flyoutDrag.current.x), boardW))
  }
  const onFlyoutUp = () => {
    flyoutDrag.current = null
  }

  // --- palette actions -----------------------------------------------------------
  const paletteSessions = useMemo(() => allSessions(fleet).sort(byLastActivity), [fleet])
  const attachCmd = sessionAttachCommand(selectedSession)
  const attachLink = canAttachInApp() ? sessionAttachLink(selectedSession) : null
  const editorUrl = selectedSession?.editorUrl ?? null
  const editorText = editorLabel(null, editorUrl)
  // ⌘K jumps to notes too: the first notes host's tree, fetched while the palette is open.
  const paletteNotesHost = notesLoc?.host ?? notesHosts[0]?.name ?? null
  const paletteNotes = useNotesTree(paletteOpen ? paletteNotesHost : null).tree
  const noteItems: PaletteAction[] = useMemo(
    () =>
      paletteNotes && paletteNotesHost
        ? paletteNotes.files
            .filter((f) => f.kind === 'markdown')
            .sort((a, b) => b.mtime - a.mtime)
            .slice(0, 300)
            .map((f) => ({
              id: `note-${paletteNotesHost}/${f.path}`,
              label: f.title ?? f.path,
              icon: <FileTextIcon />,
              keywords: ['note', f.path],
              run: () => navigate(notesHref(paletteNotesHost, f.path)),
            }))
        : [],
    [paletteNotes, paletteNotesHost, navigate],
  )
  const actions: { heading: string; items: PaletteAction[] }[] = [
    {
      heading: 'Actions',
      items: [
        { id: 'new', label: 'New session…', icon: <PlusIcon />, shortcut: shortcutHint('new'), keywords: ['spawn', 'start'], run: () => setNewOpen(true) },
        ...(selectedKey
          ? [
              { id: 'chat', label: 'Chat view', icon: <MessageSquareTextIcon />, shortcut: shortcutHint('mode'), run: () => paneRef.current?.setMode('chat') },
              { id: 'term', label: 'Terminal view', icon: <SquareTerminalIcon />, shortcut: shortcutHint('mode'), run: () => paneRef.current?.setMode('term') },
              {
                id: 'inspector',
                label: inspector ? 'Hide details (brief)' : 'Show details (brief: summary, todos, resources)',
                icon: <PanelRightIcon />,
                shortcut: shortcutHint('inspector'),
                keywords: ['details', 'brief', 'todos', 'summary', 'resources', 'continue', 'inspector'],
                run: toggleInspector,
              },
              ...(selected
                ? [
                    {
                      id: 'review',
                      label: 'Review changes (diff)',
                      icon: <FileDiffIcon />,
                      keywords: ['diff', 'review', 'changes', 'files', 'git', 'pr'],
                      run: () => navigate(reviewHref(selected.host, selected.id)),
                    },
                  ]
                : []),
            ]
          : []),
        board
          ? { id: 'view', label: 'List view', icon: <ListIcon />, shortcut: shortcutHint('view'), keywords: ['list', 'sidebar'], run: toggleView }
          : { id: 'view', label: 'Board view (grouped)', icon: <KanbanIcon />, shortcut: shortcutHint('view'), keywords: ['board', 'kanban', 'groups'], run: toggleView },
        ...(board && groups.groups?.enabled
          ? [{ id: 'regroup', label: 'Regroup now', icon: <SparklesIcon />, keywords: ['group', 'board'], run: () => void groups.run() }]
          : []),
        ...(selectedSession && selectedKey
          ? [{ id: 'rename', label: 'Rename session…', icon: <PencilIcon />, shortcut: shortcutHint('rename'), keywords: ['title', 'name'], run: () => startEditing('header', selectedKey) }]
          : []),
        ...(selected && selectedSession && stacksKnown(selectedSession)
          ? [
              {
                id: 'stack-link',
                label: selectedSession.stack?.id ? 'Add a session to this stack…' : 'Link with a session (stack)…',
                icon: <Link2Icon />,
                keywords: ['stack', 'link', 'group', 'join', 'sibling', 'together'],
                run: () => openStackLink(selected.host, selected.id),
              },
            ]
          : []),
        ...(attachLink
          ? [{ id: 'attach', label: 'Attach in iTerm', icon: <SquareTerminalIcon />, keywords: ['tmux', 'terminal', 'enter'], run: () => window.location.assign(attachLink) }]
          : []),
        ...(attachCmd
          ? [{ id: 'copy-attach', label: 'Copy attach command', icon: <CopyIcon />, keywords: ['tmux', 'terminal', 'enter', attachCmd], run: () => void copyWithToast(attachCmd) }]
          : []),
        ...(editorUrl && editorText
          ? [{ id: 'editor', label: editorText, icon: <CodeXmlIcon />, keywords: ['editor', 'vscode', 'cursor', 'code'], run: () => window.location.assign(editorUrl) }]
          : []),
        ...(focus.host
          ? (() => {
              const remaining = focusLeft(focus, now)
              return [
                {
                  id: 'focus',
                  label: remaining ? `Stop focus mode (${remaining} left)` : 'Start focus mode',
                  icon: <MoonIcon className={cn(remaining && 'fill-current')} />,
                  keywords: ['focus', 'do not disturb', 'dnd', 'distraction', 'quiet', 'deep work'],
                  run: () => void toggleFocus(focus),
                },
              ]
            })()
          : []),
        { id: 'scratchpad', label: 'Scratchpad', icon: <NotebookPenIcon />, shortcut: shortcutHint('scratchpad'), keywords: ['editor', 'text', 'paste', 'clipboard', 'draft'], run: () => scratchpad.show() },
        ...(notesHosts.length
          ? [{ id: 'notes', label: 'Notes…', icon: <NotebookTextIcon />, shortcut: shortcutHint('notes'), keywords: ['notes', 'markdown', 'knowledge', 'wiki', 'search'], run: () => navigate(notesHref()) }]
          : []),
        {
          id: 'inbox',
          label: 'Inbox…',
          icon: <InboxIcon />,
          shortcut: shortcutHint('inbox'),
          keywords: ['inbox', 'todo', 'to do', 'brief', 'quick wins', 'waiting', 'slack', 'email', 'today'],
          run: () => navigate('/inbox'),
        },
        {
          id: 'prs',
          label: 'Pull requests…',
          icon: <GitPullRequestIcon />,
          keywords: ['pull requests', 'prs', 'github', 'review', 'code review', 'drafts'],
          run: () => navigate('/prs'),
        },
        {
          id: 'tasks',
          label: 'Tasks…',
          icon: <SquareCheckBigIcon />,
          shortcut: shortcutHint('tasks'),
          keywords: ['tasks', 'productive', 'todo', 'issues', 'tickets', 'status'],
          run: () => navigate('/tasks'),
        },
        ...(settings.memos
          ? [{ id: 'memos', label: 'Voice memos…', icon: <CircleDotIcon />, shortcut: shortcutHint('memos'), keywords: ['memo', 'record', 'voice', 'transcribe', 'conversation', 'meeting'], run: () => navigate('/memos') }]
          : []),
        {
          id: 'jobs',
          label: 'Bots…',
          icon: <BotIcon />,
          keywords: ['bots', 'jobs', 'scheduled', 'cron', 'schedule', 'routines', 'run now'],
          run: () => navigate('/bots'),
        },
        {
          id: 'usage',
          label: 'Usage limits…',
          icon: <GaugeIcon />,
          keywords: ['usage', 'limits', 'quota', 'subscription', 'plan', 'weekly', 'session', 'rate limit'],
          run: () => navigate('/usage'),
        },
        {
          id: 'settings',
          label: 'Settings…',
          icon: <Settings2Icon />,
          keywords: ['preferences', 'text size', 'font', 'zoom', 'theme', 'progress notes'],
          run: () => navigate('/settings'),
        },
        { id: 'refresh', label: 'Refresh now', icon: <RefreshCwIcon />, keywords: ['reload', 'poll'], run: refresh },
        { id: 'help', label: 'Keyboard shortcuts', icon: <KeyboardIcon />, shortcut: shortcutHint('help'), run: () => setHelpOpen(true) },
      ],
    },
    { heading: paletteNotes ? `Notes · ${paletteNotes.name} (${paletteNotesHost})` : 'Notes', items: noteItems },
    {
      heading: 'Filter',
      items: STATUS_FILTERS.map((f) => ({
        id: `filter-${f.id}`,
        label: `Show: ${f.label}`,
        icon: <FilterIcon />,
        keywords: ['filter', 'status'],
        run: () => list.setStatus(f.id),
      })),
    },
    {
      heading: 'Theme',
      items: [
        ...PALETTES.map((p) => ({
          id: `palette-${p.id}`,
          label: `Theme: ${p.label}`,
          icon: <PaletteIcon />,
          keywords: ['palette', 'colours', 'colors'],
          run: () => setPalette(p.id),
        })),
        { id: 'theme-system', label: 'Mode: system', icon: <MonitorIcon />, keywords: ['theme', 'appearance'], run: () => setTheme('system') },
        { id: 'theme-dark', label: 'Mode: dark', icon: <MoonIcon />, keywords: ['theme', 'appearance'], run: () => setTheme('dark') },
        { id: 'theme-light', label: 'Mode: light', icon: <SunIcon />, keywords: ['theme', 'appearance'], run: () => setTheme('light') },
      ],
    },
  ]

  // Anything but `/`, `/settings`, `/usage`, `/bots`, `/inbox`, `/prs`, `/tasks`, notes and `/s/:host/:id` → the list (same as mobile).
  if (!match && !settingsOpen && !usageOpen && !jobsOpen && !inboxOpen && !operatorOpen && !memosOpen && !notesOpen && !prsOpen && !tasksOpen && !reviewOpen && location !== '/') return <Redirect to="/" replace />

  const onSearchNav = (a: 'next' | 'prev' | 'open') => {
    if (a === 'open') {
      const s = (cursorKey && byKey.get(cursorKey)) || ordered[0]
      if (s) openSession(s, true)
    } else moveCursor(a === 'next' ? 1 : -1)
  }

  const pane = selected ? (
    <SessionScreen
      key={selectedKey}
      host={selected.host}
      id={selected.id}
      layout="pane"
      inspector={inspector}
      onToggleInspector={toggleInspector}
      paneRef={paneRef}
      autoFocusComposer={focusOnOpen === selectedKey}
      onClose={board ? () => navigate('/') : undefined}
      detailsOverlay={board && !detailsFitBeside(flyoutW, parseFloat(getComputedStyle(document.documentElement).fontSize))}
    />
  ) : settingsOpen ? (
    <SettingsScreen layout="pane" />
  ) : usageOpen ? (
    <UsageScreen layout="pane" />
  ) : jobsOpen ? (
    <JobsScreen layout="pane" />
  ) : inboxOpen ? (
    <InboxScreen layout="pane" />
  ) : operatorOpen ? (
    <OperatorScreen layout="pane" />
  ) : memosOpen ? (
    <MemosScreen layout="pane" />
  ) : prsOpen ? (
    <PrsScreen layout="pane" />
  ) : tasksOpen ? (
    <TasksScreen layout="pane" />
  ) : null

  return (
    <div className="flex h-dvh flex-col overflow-hidden bg-background text-foreground">
      {notesOpen ? (
        <main className="flex min-h-0 min-w-0 flex-1">
          <NotesScreen layout="pane" searchRef={notesSearchRef} />
        </main>
      ) : reviewOpen ? (
        <main className="flex min-h-0 min-w-0 flex-1">
          <ReviewScreen layout="pane" />
        </main>
      ) : (
        <>
          <TopBar
            list={list}
            now={now}
            view={mode}
            onView={switchView}
            searchRef={searchRef}
            onSearchNav={onSearchNav}
            onNew={() => setNewOpen(true)}
            groups={board ? groups : undefined}
          />
          <div className="flex min-h-0 flex-1">
            {board ? (
              <div ref={boardAreaRef} className="relative flex min-w-0 flex-1">
                <section aria-label="Session board" className="flex min-w-0 flex-1">
                  <Board
                    list={list}
                    groups={groups}
                    columns={columns}
                    onMoveColumn={moveColumn}
                    now={now}
                    cursorKey={cursorKey}
                    selectedKey={selectedKey}
                    overlayInset={pane ? flyoutW : 0}
                    onOpen={(s) => openSession(s, false)}
                  />
                </section>
                {pane ? (
                  // Non-modal: no backdrop, the board stays scrollable and clickable (a card switches the session).
                  <main
                    className={cn(
                      'absolute inset-y-0 right-0 z-20 flex border-l bg-background shadow-2xl',
                      'animate-in duration-200 ease-out fade-in-0 slide-in-from-right-8 motion-reduce:animate-none',
                    )}
                    style={{ width: flyoutW, ...UNDER_TOP_BAR }}
                  >
                    <div
                      role="separator"
                      aria-orientation="vertical"
                      aria-label="Resize session panel"
                      aria-valuemin={Math.min(FLYOUT_MIN_W, flyoutMaxWidth(boardW))}
                      aria-valuemax={flyoutMaxWidth(boardW)}
                      aria-valuenow={flyoutW}
                      tabIndex={0}
                      title="Drag to resize · double-click to reset"
                      onPointerDown={onFlyoutDown}
                      onPointerMove={onFlyoutMove}
                      onPointerUp={onFlyoutUp}
                      onPointerCancel={onFlyoutUp}
                      onDoubleClick={() => setFlyoutPref(0)}
                      onKeyDown={(e) => {
                        if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') {
                          e.preventDefault()
                          const step = e.shiftKey ? FLYOUT_KEY_STEP * 4 : FLYOUT_KEY_STEP
                          setFlyoutPref(clampFlyoutWidth(flyoutW + (e.key === 'ArrowLeft' ? step : -step), boardW))
                        }
                      }}
                      className={cn(
                        'group/grip absolute inset-y-0 -left-1 z-30 w-2 cursor-col-resize touch-none outline-none',
                        'after:absolute after:inset-y-0 after:left-1/2 after:w-px after:-translate-x-1/2 after:transition-colors',
                        'hover:after:w-0.5 hover:after:bg-primary/50 focus-visible:after:w-0.5 focus-visible:after:bg-ring',
                      )}
                    >
                      <span
                        aria-hidden
                        className={cn(
                          'absolute top-1/2 left-1/2 h-10 w-1.5 -translate-x-1/2 -translate-y-1/2 rounded-full border bg-background opacity-0 shadow-sm transition-opacity',
                          'group-hover/grip:opacity-100 group-focus-visible/grip:opacity-100',
                        )}
                      />
                    </div>
                    {pane}
                  </main>
                ) : null}
              </div>
            ) : (
              <>
                <aside aria-label="Session list" className="relative shrink-0 border-r bg-background" style={{ width: sidebarW }}>
                  <Sidebar
                    list={list}
                    now={now}
                    cursorKey={cursorKey}
                    selectedKey={selectedKey}
                  />
                  <div
                    role="separator"
                    aria-orientation="vertical"
                    aria-label="Resize sidebar"
                    aria-valuemin={SIDEBAR_MIN_W}
                    aria-valuemax={SIDEBAR_MAX_W}
                    aria-valuenow={sidebarW}
                    tabIndex={0}
                    title="Drag to resize · double-click to reset"
                    onPointerDown={onResizeDown}
                    onPointerMove={onResizeMove}
                    onPointerUp={onResizeUp}
                    onPointerCancel={onResizeUp}
                    onDoubleClick={() => setSidebarW(SIDEBAR_DEFAULT_W)}
                    onKeyDown={(e) => {
                      if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') {
                        e.preventDefault()
                        setSidebarW(clampSidebarWidth(sidebarW + (e.key === 'ArrowRight' ? 24 : -24)))
                      }
                    }}
                    className={cn(
                      'absolute inset-y-0 -right-1 z-30 w-2 cursor-col-resize touch-none outline-none',
                      'after:absolute after:inset-y-0 after:left-1/2 after:w-px after:-translate-x-1/2 after:transition-colors',
                      'hover:after:w-0.5 hover:after:bg-primary/50 focus-visible:after:w-0.5 focus-visible:after:bg-ring',
                    )}
                  />
                </aside>
                <main className="flex min-w-0 flex-1" style={UNDER_TOP_BAR}>
                  {pane ?? (
                    <EmptyPane
                      waitingSessions={paletteSessions.filter((s) => s.status === 'waiting').slice(0, 6)}
                      onOpen={(s) => openSession(s, true)}
                    />
                  )}
                </main>
              </>
            )}
          </div>
        </>
      )}

      <NewSessionDialog open={newOpen} onOpenChange={setNewOpen} onOpenSession={(href) => navigate(href)} />
      <CommandPalette
        open={paletteOpen}
        onOpenChange={setPaletteOpen}
        sessions={paletteSessions}
        selectedKey={selectedKey}
        onOpenSession={(s) => openSession(s, false)}
        actions={actions}
      />
      <ShortcutsDialog open={helpOpen} onOpenChange={setHelpOpen} />
    </div>
  )
}

/** Panes under the top bar are clear of the desktop app's traffic lights (index.css → Desktop
 *  app): their own bars need no room for them. */
const UNDER_TOP_BAR = { '--titlebar-offset': '100vw' } as CSSProperties

function EmptyPane({ waitingSessions, onOpen }: { waitingSessions: Session[]; onOpen: (s: Session) => void }) {
  const hints: [string[], string][] = [
    [['↑', '↓'], 'move'],
    [['Enter'], 'open'],
    [[shortcutHint('search')], 'search'],
    [[shortcutHint('new')], 'new session'],
    [[shortcutHint('palette')], 'jump'],
    [[shortcutHint('help')], 'all shortcuts'],
  ]
  return (
    <div data-tauri-drag-region className="flex flex-1 flex-col items-center justify-center gap-6 p-8 text-center">
      <div>
        <p className="text-base font-semibold">No session open</p>
        <p className="mt-1 text-sm text-dimmer">Pick one from the list, or use the keyboard.</p>
      </div>
      <ul className="flex flex-wrap justify-center gap-x-4 gap-y-2 text-xs text-muted-foreground" aria-label="Keyboard hints">
        {hints.map(([ks, label]) => (
          <li key={label} className="flex items-center gap-1">
            {ks.map((k) => (
              <Kbd key={k}>{k}</Kbd>
            ))}
            <span className="ml-0.5">{label}</span>
          </li>
        ))}
      </ul>
      {waitingSessions.length ? (
        <section aria-label="Needs you" className="w-full max-w-md text-left">
          <h2 className="pb-1.5 text-[0.6875rem] font-semibold tracking-wider text-status-waiting uppercase">Needs you</h2>
          <div className="flex flex-col gap-1.5">
            {waitingSessions.map((s) => (
              <button
                key={sessionKey(s)}
                type="button"
                onClick={() => onOpen(s)}
                className="flex items-center gap-2 rounded-lg border border-status-waiting/35 bg-card px-3 py-2 text-left text-sm outline-none hover:bg-muted/60 focus-visible:ring-3 focus-visible:ring-ring/50"
              >
                <StatusDot status={s.status} className="size-2" />
                <span className="min-w-0 flex-1 truncate font-medium">{sessionTitle(s)}</span>
                <span className="shrink-0 truncate text-xs text-status-waiting">{statusLabel(s)}</span>
              </button>
            ))}
          </div>
        </section>
      ) : null}
    </div>
  )
}
