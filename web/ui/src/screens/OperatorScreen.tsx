import { useCallback, useEffect, useMemo, useRef, useState } from 'react'
import {
  AudioLinesIcon,
  BellIcon,
  BellOffIcon,
  ChevronLeftIcon,
  Loader2Icon,
  MicIcon,
  SendHorizontalIcon,
  SquareIcon,
  Volume2Icon,
  VolumeXIcon,
  XIcon,
} from 'lucide-react'
import { toast } from 'sonner'
import { useLocation } from 'wouter'

import { ApiError, api, sendErrorMessage } from '@/api/client'
import type { Message, OperatorStatus } from '@/api/types'
import { Markdown } from '@/components/Markdown'
import { SpeakButton } from '@/components/SpeakButton'
import { ScreenHeader } from '@/components/ScreenHeader'
import { Button } from '@/components/ui/button'
import { useFleet } from '@/hooks/useFleet'
import { usePersistentState } from '@/hooks/usePersistentState'
import { usePoller } from '@/hooks/usePoller'
import { useSettings } from '@/hooks/useSettings'
import { useSpeechPlayer } from '@/hooks/useSpeechPlayer'
import { useSwipeBack } from '@/hooks/useSwipeBack'
import { useVoice } from '@/hooks/useVoice'
import { conversation, newlyWaiting, nextRate, replyToSpeak, sessionKey, waitingAnnouncement, waitingKeys } from '@/lib/operator'
import { allSessions } from '@/lib/sessions'
import { sessionTitle } from '@/lib/title'
import { formatElapsed } from '@/lib/voice'
import { cn } from '@/lib/utils'

const parseBool = (r: string) => (r === 'true' ? true : r === 'false' ? false : undefined)

/**
 * `#/operator`: talk to the fleet. Tap the mic, speak, tap again: the words go to the operator
 * (a Claude session this server runs, web/lib/operator.mjs), and its reply is read aloud.
 * Sessions that start waiting on you are announced while the screen is open.
 */
export function OperatorScreen({ layout = 'screen' }: { layout?: 'screen' | 'pane' }) {
  const pane = layout === 'pane'
  const [, navigate] = useLocation()
  const settings = useSettings()
  const { fleet } = useFleet()
  const [status, setStatus] = useState<OperatorStatus | null>(null)
  const [starting, setStarting] = useState(false)
  // The conversation, tagged with its session: a new one (after the idle /clear) starts empty.
  const [chat, setChat] = useState<{ key: string; list: Message[] } | null>(null)
  const [opStatus, setOpStatus] = useState<string | null>(null)
  // Sent, no reply yet: "thinking" until the operator answers.
  const [awaiting, setAwaiting] = useState(false)
  const [draft, setDraft] = useState('')
  const [autoSpeak, setAutoSpeak] = usePersistentState<boolean>('fleet.operator.speak', true, parseBool)
  const [announce, setAnnounce] = usePersistentState<boolean>('fleet.operator.announce', true, parseBool)
  const speech = useSpeechPlayer()
  const { rate, setRate } = speech
  // Replies can be read aloud: by the server (OpenAI voice) or, failing that, by this device.
  const canSpeak = speech.available
  const { speak, state: speechState } = speech
  const session = status?.session ?? null
  const opKey = session ? sessionKey(session) : null
  const messages = chat && chat.key === opKey ? chat.list : null

  const back = () => {
    if (!pane && window.history.length > 1) window.history.back()
    else navigate('/', { replace: !pane })
  }
  const screenRef = useRef<HTMLDivElement>(null)
  useSwipeBack(screenRef, back, !pane)

  // Find the operator; once started, until it shows in the fleet.
  usePoller(
    async (signal) => {
      try {
        setStatus(await api.operator({ signal }))
      } catch (err) {
        if (err instanceof ApiError && err.status === 501) setStatus(null)
      }
    },
    2000,
    { enabled: settings.operator && !session },
  )

  // Its conversation.
  const spoken = useRef<string | null>(null)
  const primed = useRef(false)
  usePoller(
    async (signal) => {
      if (!session) return
      try {
        const r = await api.messages(session.host, session.session_id, 40, { signal })
        setChat({ key: sessionKey(session), list: r.messages })
        setOpStatus(r.status)
      } catch (err) {
        // Just started, nothing said yet: no transcript until the first prompt.
        if (err instanceof ApiError && err.status === 404 && err.message.startsWith('transcript not found')) {
          setChat({ key: sessionKey(session), list: [] })
          setOpStatus(session.status ?? null)
        }
        // Gone (closed, host restarted): look for the operator again.
        else if (err instanceof ApiError && err.status === 404) setStatus(null)
        else throw err
      }
    },
    1500,
    { enabled: Boolean(session) },
  )
  const turns = useMemo(() => conversation(messages ?? []), [messages])

  // Read each new reply aloud (never the ones already there when the screen opened).
  useEffect(() => {
    if (!messages) return
    const last = turns[turns.length - 1]
    if (!primed.current) {
      primed.current = true
      spoken.current = last?.role === 'assistant' ? last.key : null
      return
    }
    // Claude's transcript lines are written whole: a final reply is the end of the turn, so it
    // is read at once rather than after the status turns idle (a second or two later).
    const reply = replyToSpeak(turns, opStatus, spoken.current, { final: (session?.harness ?? 'claude') === 'claude' })
    if (!reply) return
    spoken.current = reply.key
    setAwaiting(false)
    if (autoSpeak) void speak(reply.text, `operator:${reply.key}`)
  }, [messages, turns, opStatus, autoSpeak, speak, session?.harness])

  // Announce sessions that start waiting on you.
  const sessions = useMemo(() => allSessions(fleet), [fleet])
  const waitingBefore = useRef<Set<string> | null>(null)
  const waiting = useMemo(() => sessions.filter((s) => s.status === 'waiting' && sessionKey(s) !== opKey), [sessions, opKey])
  const voiceRef = useRef<{ busy: boolean }>({ busy: false })
  useEffect(() => {
    if (!fleet) return
    const before = waitingBefore.current
    waitingBefore.current = waitingKeys(sessions)
    if (!before || !announce) return
    const fresh = newlyWaiting(sessions, before, opKey)
    if (fresh.length && speechState === 'idle' && !voiceRef.current.busy && !awaiting) void speak(waitingAnnouncement(fresh))
  }, [fleet, sessions, announce, opKey, speak, speechState, awaiting])

  const send = useCallback(
    async (text: string) => {
      const t = text.trim()
      if (!t || !session) return
      try {
        await api.send(session.host, session.session_id, t)
        setAwaiting(true)
        setChat((c) => ({ key: sessionKey(session), list: [...(c?.key === sessionKey(session) ? c.list : []), { role: 'user', kind: 'user', text: t, ts: Date.now(), queued: true }] }))
      } catch (err) {
        toast.error("Couldn't reach the operator", { description: sendErrorMessage(err) })
      }
    },
    [session],
  )

  const hint = useMemo(
    () => ['operator', ...sessions.map((s) => sessionTitle(s))].join(', ').slice(0, 380),
    [sessions],
  )
  const voice = useVoice({ onText: (t) => void send(t), hint })
  useEffect(() => {
    voiceRef.current.busy = voice.state !== 'idle'
  }, [voice.state])

  const start = async () => {
    setStarting(true)
    try {
      const r = await api.startOperator()
      setStatus(r)
      if (r.started) toast.message('Operator starting…', { description: 'It shows up here in a few seconds.' })
    } catch (err) {
      toast.error("Couldn't start the operator", { description: err instanceof ApiError ? err.message : String(err) })
    } finally {
      setStarting(false)
    }
  }

  const onMic = () => {
    if (speech.state !== 'idle') speech.stop()
    if (voice.state === 'recording') return voice.stop()
    if (voice.state === 'transcribing') return voice.cancel()
    speech.unlock()
    void voice.start()
  }

  // Keep the newest turn in view.
  const scroller = useRef<HTMLDivElement>(null)
  useEffect(() => {
    const el = scroller.current
    if (el) el.scrollTop = el.scrollHeight
  }, [turns.length, awaiting])

  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (voice.state !== 'recording') return
    const t = window.setInterval(() => setNow(Date.now()), 500)
    return () => window.clearInterval(t)
  }, [voice.state])

  const thinking = awaiting || opStatus === 'busy'
  const phase =
    voice.state === 'recording'
      ? `Listening… ${formatElapsed(now - (voice.startedAt ?? now))} — tap to send`
      : voice.state === 'transcribing'
        ? 'Transcribing…'
        : speech.state === 'loading'
          ? 'Preparing the reply…'
          : speech.state === 'playing'
            ? 'Speaking — tap to interrupt'
            : thinking
              ? 'Thinking…'
              : session
                ? 'Tap and speak'
                : ''

  const toggles = (
    <>
      <Button
        variant="ghost"
        size="icon"
        aria-label={autoSpeak ? 'Read replies aloud: on' : 'Read replies aloud: off'}
        title={autoSpeak ? 'Replies are read aloud' : 'Replies are not read aloud'}
        onClick={() => {
          if (autoSpeak) speech.stop()
          setAutoSpeak(!autoSpeak)
        }}
        className="size-11 shrink-0 rounded-xl"
        disabled={!canSpeak}
      >
        {autoSpeak && canSpeak ? <Volume2Icon className="size-5" /> : <VolumeXIcon className="size-5" />}
      </Button>
      <Button
        variant="ghost"
        size="icon"
        aria-label={`Reading speed ${rate}×`}
        title="Reading speed (tap to change)"
        onClick={() => setRate(nextRate(rate))}
        className="size-11 shrink-0 rounded-xl text-sm font-semibold tabular-nums"
        disabled={!canSpeak}
      >
        {rate}×
      </Button>
      <Button
        variant="ghost"
        size="icon"
        aria-label={announce ? 'Announce waiting sessions: on' : 'Announce waiting sessions: off'}
        title={announce ? 'Sessions that start waiting on you are announced' : 'Waiting sessions are not announced'}
        onClick={() => setAnnounce(!announce)}
        className="size-11 shrink-0 rounded-xl"
        disabled={!canSpeak}
      >
        {announce && canSpeak ? <BellIcon className="size-5" /> : <BellOffIcon className="size-5" />}
      </Button>
    </>
  )

  const body = !settings.loaded ? null : !settings.operator ? (
    <Empty>This server has no operator (update fleet-web).</Empty>
  ) : !session ? (
    <Empty>
      <AudioLinesIcon className="size-10 text-muted-foreground" />
      <p className="max-w-xs text-sm text-muted-foreground">
        The operator is a Claude session on {status?.host ?? settings.self ?? 'this host'} that watches every session and acts for you. Talk to it by voice.
      </p>
      {status && !starting && !status.started ? (
        <Button onClick={start} size="lg" className="rounded-full px-5">
          Start the operator
        </Button>
      ) : (
        <Loader2Icon className="size-5 animate-spin text-muted-foreground" />
      )}
    </Empty>
  ) : (
    <div ref={scroller} className="min-h-0 flex-1 overflow-x-hidden overflow-y-auto overscroll-contain">
      <div className="mx-auto flex w-full max-w-2xl flex-col gap-3 px-4 py-4">
        {messages && !turns.length ? <p className="py-10 text-center text-sm text-muted-foreground">Ask what the fleet is doing, who is waiting on you, what is next.</p> : null}
        {turns.map((t) =>
          t.role === 'user' ? (
            <div key={t.key} className="ml-auto max-w-[85%] rounded-2xl rounded-br-md bg-primary px-3.5 py-2 text-[0.9375rem] [overflow-wrap:anywhere] text-primary-foreground">
              {t.text}
            </div>
          ) : (
            <div key={t.key} className="group mr-auto flex max-w-[92%] items-start gap-1.5">
              <div className="min-w-0 overflow-hidden rounded-2xl rounded-bl-md bg-muted px-3.5 py-2 text-[0.9375rem] [overflow-wrap:anywhere]">
                <Markdown text={t.text} />
              </div>
              <SpeakButton text={t.text} id={`operator:${t.key}`} className="mt-1 shrink-0" />
            </div>
          ),
        )}
        {thinking ? (
          <div className="mr-auto flex items-center gap-2 rounded-2xl bg-muted px-3.5 py-2 text-sm text-muted-foreground">
            <Loader2Icon className="size-4 animate-spin" /> Thinking…
          </div>
        ) : null}
      </div>
    </div>
  )

  const footer = session ? (
    <div className="shrink-0 border-t bg-background/95 px-safe pb-safe">
      <div className="mx-auto flex w-full max-w-2xl flex-col items-center gap-2 px-4 pt-3 pb-3">
        {waiting.length ? (
          <div className="flex w-full gap-1.5 overflow-x-auto pb-0.5">
            {waiting.map((s) => (
              <button
                key={sessionKey(s)}
                type="button"
                onClick={() => void send(`Što traži sesija "${sessionTitle(s)}" na ${s.host}?`)}
                className="shrink-0 rounded-full border border-amber-500/40 bg-amber-500/10 px-3 py-1 text-xs text-amber-700 dark:text-amber-300"
                title="Ask the operator what it needs"
              >
                ⏳ {sessionTitle(s)}
              </button>
            ))}
          </div>
        ) : null}
        <p className="h-5 text-sm text-muted-foreground" aria-live="polite">
          {phase}
        </p>
        <div className="flex w-full items-center gap-3">
          <form
            className="flex min-w-0 flex-1 items-center gap-1 rounded-full border bg-card pr-1 pl-3.5"
            onSubmit={(e) => {
              e.preventDefault()
              const t = draft
              setDraft('')
              speech.unlock()
              void send(t)
            }}
          >
            <input
              value={draft}
              onChange={(e) => setDraft(e.target.value)}
              placeholder="Or type…"
              aria-label="Message the operator"
              className="h-10 min-w-0 flex-1 bg-transparent text-base outline-none"
            />
            <Button type="submit" variant="ghost" size="icon" aria-label="Send" disabled={!draft.trim()} className="rounded-full">
              <SendHorizontalIcon />
            </Button>
          </form>
          <button
            type="button"
            onClick={onMic}
            disabled={!settings.voice}
            aria-label={voice.state === 'recording' ? 'Stop and send' : voice.state === 'transcribing' ? 'Cancel' : 'Speak'}
            title={settings.voice ? undefined : 'Voice input is off on this server (no transcription key)'}
            className={cn(
              'grid size-16 shrink-0 place-items-center rounded-full shadow-lg transition-transform active:scale-95 disabled:opacity-40',
              voice.state === 'recording' ? 'animate-pulse bg-destructive text-white' : 'bg-primary text-primary-foreground',
            )}
          >
            {voice.state === 'recording' ? (
              <SquareIcon className="size-6 fill-current" />
            ) : voice.state === 'transcribing' ? (
              <Loader2Icon className="size-7 animate-spin" />
            ) : (
              <MicIcon className="size-7" />
            )}
          </button>
        </div>
      </div>
    </div>
  ) : null

  if (pane) {
    return (
      <section aria-label="Operator" className="flex h-full min-w-0 flex-1 flex-col bg-background">
        <header
          data-tauri-drag-region="deep"
          className="titlebar titlebar-lead flex shrink-0 items-center gap-1 border-b py-1.5 pr-2 pl-4 [--titlebar-pad:1rem]"
        >
          <h1 className="flex min-w-0 flex-1 items-center gap-1.5 text-[0.9375rem] leading-tight font-bold">
            <AudioLinesIcon className="size-4 text-muted-foreground" /> Operator
            {status?.host ? <span className="font-normal text-muted-foreground">· {status.host}</span> : null}
          </h1>
          {toggles}
          <Button variant="ghost" size="icon" aria-label="Close operator" title="Close (Esc)" onClick={back} className="size-11 shrink-0 rounded-xl">
            <XIcon className="size-5" />
          </Button>
        </header>
        {body}
        {footer}
      </section>
    )
  }

  return (
    <div ref={screenRef} className="flex h-app flex-col overflow-x-clip bg-background">
      <ScreenHeader>
        <div className="-ml-2 flex min-h-8 items-center gap-1">
          <Button variant="ghost" size="icon" aria-label="Back" onClick={back} className="-my-1.5 size-11 shrink-0 rounded-xl">
            <ChevronLeftIcon className="size-6" />
          </Button>
          <h1 className="min-w-0 flex-1 truncate text-xl font-bold tracking-tight">
            Operator{status?.host ? <span className="ml-1.5 text-sm font-normal text-muted-foreground">{status.host}</span> : null}
          </h1>
          {toggles}
        </div>
      </ScreenHeader>
      {body}
      {footer}
    </div>
  )
}

function Empty({ children }: { children: React.ReactNode }) {
  return <div className="flex flex-1 flex-col items-center justify-center gap-4 px-6 text-center">{children}</div>
}
