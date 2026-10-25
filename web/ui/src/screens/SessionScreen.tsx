import { useCallback, useEffect, useLayoutEffect, useRef, useState, type PointerEvent as ReactPointerEvent } from 'react'
import {
  ChevronLeftIcon,
  CircleAlertIcon,
  EllipsisIcon,
  FileDiffIcon,
  WifiOffIcon,
  XIcon,
} from 'lucide-react'
import { useLocation } from 'wouter'
import { toast } from 'sonner'

import { ApiError, api, isAbortError, isSessionGone, sendErrorMessage, sessionErrorMessage } from '@/api/client'
import type { FileStat, Message, SessionKey } from '@/api/types'
import { ContextMeter } from '@/components/ContextMeter'
import { DropOverlay } from '@/components/DropOverlay'
import { EditableTitle } from '@/components/EditableTitle'
import { HostBadge } from '@/components/HostBadge'
import { PrChip } from '@/components/PrChip'
import { TaskChip } from '@/components/TaskChip'
import { NewSessionDialog, NewSessionDrawer, type SpawnPrefill } from '@/components/NewSessionDrawer'
import { StatusDot } from '@/components/StatusDot'
import { StackBar } from '@/components/stack/StackBar'
import { ChatView } from '@/components/session/ChatView'
import { CompactHint } from '@/components/session/CompactHint'
import { Composer, type ComposerApi } from '@/components/session/Composer'
import { FilePreview } from '@/components/session/FilePreview'
import { OutboxBubbles } from '@/components/session/OutboxBubbles'
import type { QuestionActions } from '@/components/session/QuestionCard'
import { DetailsDrawer, DetailsPanel, type DetailsPanelProps } from '@/components/session/DetailsPanel'
import { ScratchpadButton } from '@/components/Scratchpad'
import { Button } from '@/components/ui/button'
import { useFileDrop } from '@/hooks/useAttach'
import { useBrief } from '@/hooks/useBrief'
import { useFileStats } from '@/hooks/useFileLinks'
import { useFleet } from '@/hooks/useFleet'
import { useNow } from '@/hooks/useNow'
import { useOutbox } from '@/hooks/useOutbox'
import { usePersistentState } from '@/hooks/usePersistentState'
import { usePoller } from '@/hooks/usePoller'
import { usePrefs } from '@/hooks/usePrefs'
import { useSettings } from '@/hooks/useSettings'
import { openStackLink, openStackSheet, type SiblingTarget } from '@/hooks/useStackUi'
import { useSwipeBack } from '@/hooks/useSwipeBack'
import { useSessionTitle } from '@/hooks/useTitles'
import { CHAT_LIMITS, CHAT_POLL_MS, nextChatLimit } from '@/lib/chat'
import { continueDraft } from '@/lib/brief'
import { modelLabel, relTime, shortCwd } from '@/lib/format'
import { harnessLabel } from '@/lib/harness'
import { tailAnchor } from '@/lib/outbox'
import { findSession, statusMeta } from '@/lib/sessions'
import { reviewHref } from '@/lib/review'
import { stacksKnown } from '@/lib/stacks'
import { isUndoBackspace } from '@/lib/shortcuts'
import { STATUS_TEXT } from '@/lib/styles'
import { clampDetailsWidth, DETAILS_DEFAULT_W, DETAILS_MAX_W, DETAILS_MIN_W, detailsWidth } from '@/lib/layout'
import { cn } from '@/lib/utils'

interface ChatState {
  messages: Message[]
  truncated: boolean
  status: string
  name: string | null
  at: number
  /** JSON of the payload, to skip re-renders when a poll brings nothing new. */
  key: string
}

/** What the desktop shell's shortcuts can ask of the open pane. */
export interface PaneApi {
  focusComposer: () => void
}

export interface SessionScreenProps {
  host: string
  id: string
  /** `screen` (mobile, default): fixed full-screen page. `pane`: the desktop detail pane. */
  layout?: 'screen' | 'pane'
  /** Pane only: filled with the pane's API while mounted. */
  paneRef?: React.RefObject<PaneApi | null>
  /** Pane only: focus the composer on mount (opened with Enter). */
  autoFocusComposer?: boolean
  /**
   * Shown beside the diff reviewer (pane) or inside another app (the embed view, screen): no
   * Review / Details / Scratchpad buttons, no stack bar.
   */
  embedded?: boolean
  /** Screen only: Back (and after closing the session) goes here instead of the history / the list. */
  onBack?: () => void
  /** Pane only: a close button in the header (the Board view's flyout). */
  onClose?: () => void
}

/**
 * Session detail — `#/s/:host/:id`: the chat (transcript, polls `messages` every 3s).
 * Shared composer with quick replies + key chips; ⋯ opens the session menu (drawer).
 */
export function SessionScreen({
  host,
  id,
  layout = 'screen',
  paneRef,
  autoFocusComposer = false,
  onClose,
  embedded = false,
  onBack,
}: SessionScreenProps) {
  const pane = layout === 'pane'
  const { fleet, refresh: refreshFleet, dropSession } = useFleet()
  const settings = useSettings()
  const [, navigate] = useLocation()
  const now = useNow(1000)
  const session = findSession(fleet, host, id)
  const hostEntry = fleet?.hosts.find((h) => h.name === host) ?? null

  // View prefs (localStorage): text size and progress notes are global (the Settings screen).
  const { hideNotes, sendDelay } = usePrefs()

  const [menuOpen, setMenuOpen] = useState(false)
  // Pane: the Details column (brief + session details) is always beside the chat, resizable
  // from its left edge (`fleet.detailsWidth`); a pane too narrow for both uses the ⋯ drawer.
  const paneEl = useRef<HTMLDivElement>(null)
  const [paneW, setPaneW] = useState(0)
  useLayoutEffect(() => {
    const el = paneEl.current
    if (!el) return
    setPaneW(el.getBoundingClientRect().width)
    if (typeof ResizeObserver === 'undefined') return
    const ro = new ResizeObserver(([entry]) => entry && entry.contentRect.width > 0 && setPaneW(entry.contentRect.width))
    ro.observe(el)
    return () => ro.disconnect()
  }, [])
  const [detailsPref, setDetailsPref] = usePersistentState<number>('fleet.detailsWidth', DETAILS_DEFAULT_W, (r) => clampDetailsWidth(Number(r)))
  const detailsW = pane && !embedded ? detailsWidth(detailsPref, paneW) : 0
  const detailsDrag = useRef<{ x: number; w: number } | null>(null)
  const onDetailsDown = (e: ReactPointerEvent<HTMLDivElement>) => {
    e.preventDefault()
    e.currentTarget.setPointerCapture(e.pointerId)
    detailsDrag.current = { x: e.clientX, w: detailsW }
  }
  const onDetailsMove = (e: ReactPointerEvent<HTMLDivElement>) => {
    if (!detailsDrag.current) return
    setDetailsPref(clampDetailsWidth(detailsDrag.current.w - (e.clientX - detailsDrag.current.x)))
  }
  const onDetailsUp = () => {
    detailsDrag.current = null
  }
  const [jumpSignal, setJumpSignal] = useState(0)
  // Paths in Claude's messages that exist on the host → links → this preview.
  const sessionKey = `${host}/${id}`
  const [previewOf, setPreviewOf] = useState<{ key: string; file: FileStat } | null>(null)
  const preview = previewOf?.key === sessionKey ? previewOf.file : null
  const openPreview = useCallback((file: FileStat) => setPreviewOf({ key: sessionKey, file }), [sessionKey])
  const fileLinks = useFileStats(host, id, openPreview)

  // Brief: the top of the Details panel — the desktop column (open state owned by the shell)
  // or the mobile drawer (⋯). Polled only while it shows.
  const brief = useBrief(host, id, detailsW > 0 || menuOpen)
  // "Continue in new session" → the New session form, prefilled (host, this cwd, the brief's prompt).
  const [continueWith, setContinueWith] = useState<SpawnPrefill | null>(null)

  const composerRef = useRef<HTMLTextAreaElement>(null)
  useEffect(() => {
    if (!paneRef) return
    paneRef.current = {
      focusComposer: () => composerRef.current?.focus(),
    }
    return () => {
      paneRef.current = null
    }
  }, [paneRef])
  useEffect(() => {
    if (autoFocusComposer) composerRef.current?.focus()
  }, [autoFocusComposer])

  // --- chat loop -----------------------------------------------------------
  const [chat, setChat] = useState<ChatState | null>(null)
  const [chatErr, setChatErr] = useState<ApiError | Error | null>(null)
  const [chatLimit, setChatLimit] = useState<number>(CHAT_LIMITS[0])
  const [loadingOlder, setLoadingOlder] = useState(false)
  const [msgBackend, setMsgBackend] = useState<string | null>(null)

  const [gone, setGone] = useState(false)

  const pollChat = useCallback(
    async (signal: AbortSignal) => {
      try {
        const data = await api.messages(host, id, chatLimit, { signal })
        const messages = Array.isArray(data.messages) ? data.messages : []
        const truncated = data.truncated === true
        const status = String(data.status || 'unknown')
        const name = typeof data.name === 'string' && data.name ? data.name : null
        const key = JSON.stringify([messages, truncated, status, name])
        const at = data.capturedAt || Date.now()
        setChat((prev) => (prev?.key === key ? { ...prev, at } : { messages, truncated, status, name, at, key }))
        if (data.backend) setMsgBackend(String(data.backend))
        setChatErr(null)
        setGone(false)
      } catch (err) {
        if (isAbortError(err)) throw err
        // 404 is usually "no transcript on disk yet"; "unknown session" means it is gone.
        setChatErr(err as Error)
        if (isSessionGone(err)) setGone(true)
      } finally {
        if (!signal.aborted) setLoadingOlder(false)
      }
    },
    [host, id, chatLimit],
  )

  const refreshChat = usePoller(pollChat, CHAT_POLL_MS)

  // Limit changes re-poll now (the poller always calls the latest fn).
  const firstLimit = useRef(true)
  useEffect(() => {
    if (firstLimit.current) {
      firstLimit.current = false
      return
    }
    refreshChat()
  }, [chatLimit, refreshChat])

  // Follow-up polls after steering, so the answer shows up quickly.
  const followUps = useRef(new Set<ReturnType<typeof setTimeout>>())
  useEffect(() => {
    const timers = followUps.current
    return () => {
      for (const t of timers) clearTimeout(t)
      timers.clear()
    }
  }, [])
  const refreshChatRef = useRef(refreshChat)
  useEffect(() => {
    refreshChatRef.current = refreshChat
  }, [refreshChat])
  const scheduleFollowUps = () => {
    for (const delay of [800, 2000]) {
      const t = setTimeout(() => {
        followUps.current.delete(t)
        refreshChatRef.current()
      }, delay)
      followUps.current.add(t)
    }
  }

  // --- derived -------------------------------------------------------------
  const backend = msgBackend || (session?.backend ? String(session.backend) : null)
  const status = gone ? 'unknown' : chat ? chat.status : String(session?.status || 'unknown')
  const meta = statusMeta(status)
  const updatedAt = chat?.at
  const lockedReason = gone
    ? 'Session is gone — nothing to steer.'
    : backend === 'unknown'
      ? "Backend is unknown — this session can't be steered."
      : null

  // Files dropped anywhere on the session go through the composer (upload → path at the caret).
  const attachRef = useRef<(files: File[]) => void>(null)
  const drop = useFileDrop((files) => attachRef.current?.(files), lockedReason == null)

  const noTranscript = chatErr instanceof ApiError && chatErr.status === 404 && !isSessionGone(chatErr)
  const errText = !chatErr || gone ? null : noTranscript ? (chat ? 'No transcript for this session yet' : null) : sessionErrorMessage(chatErr)
  const hostDown = hostEntry?.ok === false
  const missing = !!fleet && !hostDown && !session

  // --- actions -------------------------------------------------------------
  const afterSteer = () => {
    setJumpSignal((n) => n + 1)
    scheduleFollowUps()
  }
  const steerError = (err: unknown) => {
    if (isAbortError(err)) return
    if (err instanceof ApiError && err.status === 404) setGone(true)
    toast.error(sessionErrorMessage(err))
  }

  // Messages go through the outbox (lib/outbox.ts): an undo window (Settings → Send delay), then
  // one POST at a time; the composer never waits. Keys stay immediate.
  const composerApi = useRef<ComposerApi>(null)
  const chatRef = useRef(chat)
  useEffect(() => {
    chatRef.current = chat
  }, [chat])
  const { box, items: outbox } = useOutbox({
    host,
    id,
    onSent: afterSteer,
    onError: (err, _item, mounted) => {
      if (isAbortError(err)) return
      if (mounted && err instanceof ApiError && err.status === 404) setGone(true)
      // On screen the bubble says it (Retry / Edit); after leaving only a toast can.
      if (!mounted) toast.error(`Not sent: ${sendErrorMessage(err)}`)
    },
  })
  // The real message arrived → drop its optimistic bubble.
  useEffect(() => {
    if (chat) box.reconcile(chat.messages)
  }, [chat, box])
  const hasPending = outbox.some((it) => it.state === 'pending')

  const send = (text: string): boolean => {
    if (lockedReason || !text.trim()) return false
    box.add(text, sendDelay, tailAnchor(chatRef.current?.messages))
    setJumpSignal((n) => n + 1)
    return true
  }
  const undo = () => {
    const it = box.cancel()
    if (it) composerApi.current?.restore(it.text)
  }
  const editFailed = (itemId: number) => {
    const text = box.discard(itemId)
    if (text != null) composerApi.current?.restore(text)
  }
  const retryFailed = (itemId: number) => box.retry(itemId, tailAnchor(chatRef.current?.messages))

  // Backspace cancels the pending message (not while it edits text: in a non-empty field it
  // deletes as usual) — capture phase, only while one counts down, not over a dialog.
  const undoRef = useRef(undo)
  useEffect(() => {
    undoRef.current = undo
  })
  useEffect(() => {
    if (!hasPending) return
    const onKeyDown = (e: KeyboardEvent) => {
      if (!isUndoBackspace(e, e.target) || e.defaultPrevented) return
      if (document.querySelector('[role="dialog"][data-state="open"], [data-vaul-drawer][data-state="open"]')) return
      e.preventDefault()
      e.stopPropagation()
      undoRef.current()
    }
    window.addEventListener('keydown', onKeyDown, true)
    return () => window.removeEventListener('keydown', onKeyDown, true)
  }, [hasPending])

  const sendKey = async (key: SessionKey) => {
    if (lockedReason) return
    try {
      await api.keys(host, id, key)
      toast.success(`${key} sent`, { duration: 1200 })
      afterSteer()
    } catch (err) {
      steerError(err)
    }
  }

  const questionActions: QuestionActions | null = lockedReason
    ? null
    : {
        answer: async (toolUseId, answers) => {
          try {
            await api.answer(host, id, toolUseId, answers)
            afterSteer()
          } catch (err) {
            steerError(err)
            throw err
          }
        },
        dismiss: async () => {
          try {
            await api.keys(host, id, 'Escape')
            afterSteer()
            composerRef.current?.focus()
          } catch (err) {
            steerError(err)
            throw err
          }
        },
      }


  const back = () => {
    if (onBack) onBack()
    else if (window.history.length > 1) window.history.back()
    else navigate('/', { replace: true })
  }
  // Mobile: swipe right anywhere on the screen = Back (the installed PWA has no native back swipe).
  const screenRef = useRef<HTMLDivElement>(null)
  useSwipeBack(screenRef, back, !pane && !menuOpen && !continueWith)

  const loadOlder = () => {
    const next = nextChatLimit(chatLimit)
    if (!next) return
    setLoadingOlder(true)
    setChatLimit(next)
  }

  const titleKey = `${host}/${id}`
  const { title: shownTitle } = useSessionTitle(session, titleKey)
  const name = session ? shownTitle : chat?.name || id.slice(0, 8)

  // Mobile: one drawer at a time — Details closes, then the next one opens.
  const fromMenu = (fn: () => void) => {
    if (pane) return fn()
    setMenuOpen(false)
    setTimeout(fn, 300)
  }
  const stackRef = session?.stack ?? null
  const stackHome = stackRef ? stackRef.host || host : host
  // The form's Sibling mode (hosts that predate stacks: Hand off only). A member off the
  // stack's home host spawns from the stack; otherwise from this session (creating the stack).
  const siblingTarget: SiblingTarget | null = !session || !stacksKnown(session)
    ? null
    : {
        host: stackHome,
        ...(stackRef && stackHome !== host ? { stackId: stackRef.id } : { sessionId: id }),
        cwd: session.cwd ?? null,
        cwdHost: host,
        label: stackRef ? stackRef.label : shownTitle,
        creates: !stackRef,
      }
  const newFromHere = () => {
    const prompt = brief.brief?.continuePrompt
    const prefill: SpawnPrefill = { host, dir: session?.cwd ?? null, prompt: prompt ? continueDraft(prompt) : '', sibling: siblingTarget }
    fromMenu(() => setContinueWith(prefill))
  }
  const stackMenu: DetailsPanelProps['stack'] =
    session && stacksKnown(session)
      ? {
          label: stackRef ? stackRef.label : null,
          onOpen: stackRef ? () => fromMenu(() => openStackSheet(stackHome, stackRef.id)) : undefined,
          onLink: () => fromMenu(() => openStackLink(host, id)),
        }
      : null

  const menuProps: DetailsPanelProps = {
    open: menuOpen,
    onOpenChange: setMenuOpen,
    host,
    id,
    session,
    brief: { host, brief, fileLinks },
    onNewFromHere: session ? newFromHere : undefined,
    onReview: session?.cwd && detailsW > 0 ? () => navigate(reviewHref(host, id)) : undefined,
    stack: embedded ? null : stackMenu,
    onClosed: () => {
      // Drop it from the list now and keep stale polls from bringing it back.
      dropSession(host, id)
      setTimeout(refreshFleet, 2500)
      if (onBack) onBack()
      else navigate('/', { replace: true })
    },
  }

  const NewSession = pane ? NewSessionDialog : NewSessionDrawer
  const continueForm = (
    <NewSession
      open={continueWith != null}
      onOpenChange={(o) => !o && setContinueWith(null)}
      onOpenSession={(href) => navigate(href)}
      prefill={continueWith}
    />
  )

  const column = (
    <>
      <header
        data-tauri-drag-region={pane ? 'deep' : undefined}
        className="z-20 shrink-0 border-b bg-background/90 pt-safe px-safe backdrop-blur-md backdrop-saturate-150"
      >
        <div
          className={
            pane
              ? 'titlebar titlebar-lead flex w-full items-center gap-1 py-1.5 pr-2 pl-4 [--titlebar-pad:1rem]'
              : 'mx-auto flex w-full max-w-3xl items-center gap-1 py-1.5 pr-2 pl-1'
          }
        >
          {pane ? null : (
            <Button variant="ghost" size="icon" aria-label="Back" onClick={back} className="size-11 shrink-0 rounded-xl">
              <ChevronLeftIcon className="size-6" />
            </Button>
          )}
          <div className="min-w-0 flex-1">
            <div className="flex min-w-0 text-[0.9375rem] leading-tight font-bold">
              {session ? <EditableTitle session={session} scope="header" tapToEdit className="text-[0.9375rem] font-bold" /> : <span className="truncate">{name}</span>}
            </div>
            <div className="mt-0.5 flex min-w-0 items-center gap-1.5 overflow-hidden text-xs whitespace-nowrap">
              <StatusDot status={status} className="size-2" />
              <span className={cn('shrink-0', STATUS_TEXT[meta.key])}>{gone ? 'gone' : meta.label}</span>
              <HostBadge host={host} className="h-4 px-1 text-[0.625rem]" />
              {session?.session_id ? <PrChip session={{ host, session_id: session.session_id }} className="h-4" /> : null}
              {session?.session_id ? <TaskChip session={{ host, session_id: session.session_id }} className="h-4" /> : null}
              {session?.context?.model ? (
                <span className="shrink-0 text-dim" title={session.context.model}>
                  {modelLabel(session.context.model)}
                </span>
              ) : null}
              <ContextMeter context={session?.context} />
              {pane && session?.cwd ? (
                <span className="min-w-0 shrink truncate font-mono text-[0.6875rem] text-dimmer" title={session.cwd}>
                  · {shortCwd(session.cwd, 60)}
                </span>
              ) : null}
              <span className="truncate text-dimmer tabular-nums">
                {updatedAt ? `· ${relTime(updatedAt, now)} ago` : '· connecting…'}
              </span>
            </div>
          </div>
          {session?.cwd && !embedded && detailsW === 0 ? (
            <Button
              variant="ghost"
              size="icon"
              aria-label="Review changes"
              title="Review changes (diff)"
              onClick={() => navigate(reviewHref(host, id))}
              className="size-11 shrink-0 rounded-xl text-muted-foreground"
            >
              <FileDiffIcon className="size-5" />
            </Button>
          ) : null}
          {detailsW > 0 ? null : (
            <>
            {embedded ? null : <ScratchpadButton className="size-11 shrink-0 rounded-xl text-muted-foreground [&_svg:not([class*='size-'])]:size-5" />}
            <Button
              variant="ghost"
              size="icon"
              aria-label="More"
              aria-expanded={menuOpen}
              onClick={() => setMenuOpen(true)}
              className="size-11 shrink-0 rounded-xl"
            >
              <EllipsisIcon className="size-5" />
            </Button>
            </>
          )}
          {pane && onClose ? (
            <Button variant="ghost" size="icon" aria-label="Close session" title="Close (Esc)" onClick={onClose} className="size-11 shrink-0 rounded-xl">
              <XIcon className="size-5" />
            </Button>
          ) : null}
        </div>
      </header>

      {session?.stack && !embedded ? <StackBar session={session} pane={pane} /> : null}

      {hostDown || missing ? (
        <div className="shrink-0 px-3 px-safe">
          <div
            className={cn(
              pane
                ? 'mx-auto mt-2 flex max-w-4xl items-start gap-2 rounded-lg border px-3 py-2 text-xs'
                : 'mx-auto mt-2 flex max-w-3xl items-start gap-2 rounded-lg border px-3 py-2 text-xs',
              hostDown ? 'border-destructive/40 bg-destructive/10 text-destructive' : 'border-border bg-card text-muted-foreground',
            )}
          >
            {hostDown ? <WifiOffIcon className="mt-px size-3.5 shrink-0" /> : <CircleAlertIcon className="mt-px size-3.5 shrink-0" />}
            <span className="break-words">
              {hostDown
                ? `${host} unreachable: ${hostEntry?.error || 'no response'}`
                : gone
                  ? 'This session has ended.'
                  : 'Not in the fleet list right now — it may have ended.'}
            </span>
          </div>
        </div>
      ) : null}

      <main className="relative flex min-h-0 flex-1 flex-col">
        <ChatView
            messages={chat?.messages ?? null}
            noTranscript={noTranscript}
            gone={gone}
            hideNotes={hideNotes}
            status={status}
            waitingFor={session?.waiting_for}
            canLoadOlder={!!chat?.truncated && chatLimit < CHAT_LIMITS[CHAT_LIMITS.length - 1]}
            loadingOlder={loadingOlder}
            onLoadOlder={loadOlder}
            jumpSignal={jumpSignal}
            wide={pane}
            fileLinks={fileLinks}
            outbox={<OutboxBubbles items={outbox} desktop={pane} onUndo={undo} onRetry={retryFailed} onEdit={editFailed} />}
            outboxKey={outbox.map((it) => `${it.id}:${it.state}`).join(',')}
            questionActions={questionActions}
          />
        {(session?.harness || 'claude') === 'claude' && !lockedReason && !gone && !outbox.some((it) => it.state !== 'sent' && it.text.trim() === '/compact') ? (
          <CompactHint context={session?.context} onCompact={() => send('/compact')} />
        ) : null}
        {errText ? (
          <div className="pointer-events-none absolute inset-x-0 top-2 z-10 flex justify-center px-3">
            <div className="flex max-w-full items-center gap-2 rounded-full border border-destructive/40 bg-popover/95 px-3 py-1.5 text-xs text-destructive shadow-md backdrop-blur-sm">
              <CircleAlertIcon className="size-3.5 shrink-0" />
              <span className="truncate">{errText}</span>
            </div>
          </div>
        ) : null}
      </main>

      <Composer
        quickReplies={settings.quickReplies}
        lockedReason={lockedReason}
        onSend={send}
        onKey={sendKey}
        desktop={pane}
        inputRef={composerRef}
        host={host}
        attachRef={attachRef}
        draftKey={sessionKey}
        apiRef={composerApi}
        agent={harnessLabel(session?.harness)}
        voiceHint={[name, session?.cwd?.split('/').pop()].filter(Boolean).join(', ')}
        sessionId={id}
        slashCommands={(session?.harness || 'claude') === 'claude'}
      />
      <DropOverlay show={drop.dragging} hint={`Uploaded to ${host}; the path goes into the message`} />
      <FilePreview host={host} id={id} file={preview} onClose={() => setPreviewOf(null)} desktop={pane} />
    </>
  )

  if (pane) {
    return (
      <div ref={paneEl} className="flex h-full min-w-0 flex-1 overflow-hidden bg-background">
        <section aria-label={`Session ${name}`} className="relative flex min-w-0 flex-1 flex-col" {...drop.bind}>
          {column}
        </section>
        {detailsW > 0 ? (
          <aside aria-label="Session details" className="relative shrink-0 border-l bg-card/30" style={{ width: detailsW }}>
            <div
              role="separator"
              aria-orientation="vertical"
              aria-label="Resize details"
              aria-valuemin={DETAILS_MIN_W}
              aria-valuemax={DETAILS_MAX_W}
              aria-valuenow={detailsW}
              tabIndex={0}
              title="Drag to resize · double-click to reset"
              onPointerDown={onDetailsDown}
              onPointerMove={onDetailsMove}
              onPointerUp={onDetailsUp}
              onPointerCancel={onDetailsUp}
              onDoubleClick={() => setDetailsPref(DETAILS_DEFAULT_W)}
              onKeyDown={(e) => {
                if (e.key === 'ArrowLeft' || e.key === 'ArrowRight') {
                  e.preventDefault()
                  setDetailsPref(clampDetailsWidth(detailsW + (e.key === 'ArrowLeft' ? 24 : -24)))
                }
              }}
              className={cn(
                'absolute inset-y-0 -left-1 z-30 w-2 cursor-col-resize touch-none outline-none',
                'after:absolute after:inset-y-0 after:left-1/2 after:w-px after:-translate-x-1/2 after:transition-colors',
                'hover:after:w-0.5 hover:after:bg-primary/50 focus-visible:after:w-0.5 focus-visible:after:bg-ring',
              )}
            />
            <DetailsPanel {...menuProps} open variant="panel" />
          </aside>
        ) : (
          <DetailsDrawer {...menuProps} />
        )}
        {continueForm}
      </div>
    )
  }

  return (
    <div ref={screenRef} className="fixed-app flex flex-col overflow-hidden bg-background" {...drop.bind}>
      {column}
      <DetailsDrawer {...menuProps} />
      {continueForm}
    </div>
  )
}

