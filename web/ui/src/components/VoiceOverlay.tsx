// The full-screen recorder (providers/VoiceProvider.tsx drives it): full screen on a phone, a
// centred card on a desktop. Big targets on purpose — it is used while driving.
import { AlertTriangleIcon, CopyIcon, Loader2Icon, MicIcon, MicOffIcon, RotateCcwIcon, SendHorizontalIcon, SquareIcon, Trash2Icon, XIcon } from 'lucide-react'
import { Dialog as DialogPrimitive } from 'radix-ui'
import { useEffect, useState } from 'react'

import { Button } from '@/components/ui/button'
import { CONFIRM_MS, formatElapsed, hearsNothing } from '@/lib/voice'
import type { VoiceMeter } from '@/lib/voiceMeter'
import { cn } from '@/lib/utils'

export type VoicePhase =
  | { kind: 'idle' }
  | { kind: 'starting'; label: string }
  | { kind: 'recording'; label: string; startedAt: number }
  | { kind: 'transcribing'; label: string }
  /** The operator: the text goes out at `until` unless cancelled. */
  | { kind: 'confirm'; label: string; text: string; until: number }
  /** Text to check before it goes anywhere; `action` names the button that uses it (null: copy only). */
  | { kind: 'review'; label: string; text: string; action: 'Send' | 'Insert' | null }
  | { kind: 'failed'; label: string; error: string; durationMs: number | null }
  /** A take an earlier visit left behind. */
  | { kind: 'offer'; label: string; startedAt: number; durationMs: number | null; error: string | null }

export interface VoiceActions {
  stop: () => void
  cancel: () => void
  retry: () => void
  keep: () => void
  discard: () => void
  sendNow: (text: string) => void
  use: (text: string) => void
  copy: (text: string) => Promise<void>
  transcribeOffered: () => void
  meter: () => VoiceMeter
}

const BARS = 36

/** The mic's level as moving bars: proof that it is recording. */
function LevelBars({ meter }: { meter: () => VoiceMeter }) {
  const [levels, setLevels] = useState<number[]>(() => Array(BARS).fill(0))
  useEffect(() => {
    const t = window.setInterval(() => setLevels((l) => [...l.slice(1), meter().level()]), 80)
    return () => window.clearInterval(t)
  }, [meter])
  return (
    <div className="flex h-24 w-full max-w-sm items-center justify-center gap-[3px]" aria-hidden>
      {levels.map((v, i) => (
        <span key={i} className="w-1.5 rounded-full bg-destructive/80 transition-[height] duration-75" style={{ height: `${Math.max(6, Math.min(100, Math.sqrt(v) * 160))}%` }} />
      ))}
    </div>
  )
}

function useNow(ms: number) {
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    const t = window.setInterval(() => setNow(Date.now()), ms)
    return () => window.clearInterval(t)
  }, [ms])
  return now
}

function Recording({ phase, actions }: { phase: Extract<VoicePhase, { kind: 'recording' }>; actions: VoiceActions }) {
  const now = useNow(250)
  const elapsed = now - phase.startedAt
  const silent = hearsNothing({ elapsedMs: elapsed, peak: actions.meter().peak() })
  return (
    <>
      <Body>
        <div className="flex items-center gap-2 text-sm font-medium text-destructive">
          <span className="size-2.5 animate-pulse rounded-full bg-destructive" /> Recording
        </div>
        <div className="text-6xl font-light tabular-nums">{formatElapsed(elapsed)}</div>
        <LevelBars meter={actions.meter} />
        <p className={cn('flex min-h-6 items-center gap-1.5 text-sm', silent ? 'text-destructive' : 'text-muted-foreground')} role={silent ? 'alert' : undefined}>
          {silent ? (
            <>
              <MicOffIcon className="size-4" /> The microphone hears nothing
            </>
          ) : (
            'Speak — tap stop when you are done'
          )}
        </p>
      </Body>
      <Footer>
        <button
          type="button"
          autoFocus
          onClick={actions.stop}
          aria-label="Stop and transcribe"
          className="mx-auto grid size-36 place-items-center rounded-full bg-destructive text-white shadow-xl ring-8 ring-destructive/20 transition-transform active:scale-95 sm:size-24"
        >
          <SquareIcon className="size-12 fill-current sm:size-8" />
        </button>
        <Button variant="ghost" size="lg" className="mx-auto h-12 text-base text-muted-foreground" onClick={actions.cancel}>
          <Trash2Icon /> Discard
        </Button>
      </Footer>
    </>
  )
}

function Confirm({ phase, actions }: { phase: Extract<VoicePhase, { kind: 'confirm' }>; actions: VoiceActions }) {
  const now = useNow(100)
  const left = Math.max(0, phase.until - now)
  return (
    <>
      <Body>
        <Transcript text={phase.text} />
        <div className="w-full max-w-sm">
          <div className="h-1.5 overflow-hidden rounded-full bg-muted">
            <div className="h-full bg-primary transition-[width] duration-100 ease-linear" style={{ width: `${(left / CONFIRM_MS) * 100}%` }} />
          </div>
          <p className="mt-2 text-center text-sm text-muted-foreground">Sending in {Math.ceil(left / 1000)} s</p>
        </div>
      </Body>
      <Footer>
        <Button autoFocus variant="destructive" className="h-20 w-full rounded-2xl text-xl" onClick={actions.cancel}>
          <XIcon className="size-6" /> Cancel
        </Button>
        <Button variant="outline" size="lg" className="h-12 w-full text-base" onClick={() => actions.sendNow(phase.text)}>
          <SendHorizontalIcon /> Send now
        </Button>
      </Footer>
    </>
  )
}

// Review opens where Cancel was tapped: a second, anxious tap must not hit Send.
const ARM_MS = 700

function Review({ phase, actions }: { phase: Extract<VoicePhase, { kind: 'review' }>; actions: VoiceActions }) {
  const [text, setText] = useState(phase.text)
  const [armed, setArmed] = useState(false)
  useEffect(() => {
    const t = window.setTimeout(() => setArmed(true), ARM_MS)
    return () => window.clearTimeout(t)
  }, [])
  return (
    <>
      <Body>
        <textarea
          value={text}
          onChange={(e) => setText(e.target.value)}
          aria-label="Transcript"
          className="min-h-40 w-full max-w-md flex-1 resize-none rounded-xl border bg-muted/30 p-4 text-base leading-relaxed outline-none focus-visible:ring-2 focus-visible:ring-ring sm:max-h-80"
        />
      </Body>
      <Footer>
        {phase.action ? (
          <Button className="h-16 w-full rounded-2xl text-lg" disabled={!armed || !text.trim()} onClick={() => actions.use(text.trim())}>
            <SendHorizontalIcon className="size-5" /> {phase.action}
          </Button>
        ) : null}
        <div className="flex gap-2">
          <Button variant="outline" size="lg" className="h-12 flex-1 text-base" disabled={!armed || !text.trim()} onClick={() => void actions.copy(text.trim())}>
            <CopyIcon /> Copy
          </Button>
          <Button variant="ghost" size="lg" className="h-12 flex-1 text-base text-muted-foreground" disabled={!armed} onClick={actions.discard}>
            <Trash2Icon /> Discard
          </Button>
        </div>
      </Footer>
    </>
  )
}

function Failed({ phase, actions }: { phase: Extract<VoicePhase, { kind: 'failed' }>; actions: VoiceActions }) {
  return (
    <>
      <Body>
        <AlertTriangleIcon className="size-12 text-destructive" />
        <div className="text-xl font-medium">Couldn't transcribe</div>
        <p className="max-w-sm text-center text-sm break-words text-muted-foreground">{phase.error}</p>
        <p className="max-w-sm text-center text-sm">
          The recording{phase.durationMs ? ` (${formatElapsed(phase.durationMs)})` : ''} is saved on this device: nothing is lost.
        </p>
      </Body>
      <Footer>
        <Button autoFocus className="h-16 w-full rounded-2xl text-lg" onClick={actions.retry}>
          <RotateCcwIcon className="size-5" /> Retry
        </Button>
        <div className="flex gap-2">
          <Button variant="outline" size="lg" className="h-12 flex-1 text-base" onClick={actions.keep}>
            Keep for later
          </Button>
          <Button variant="ghost" size="lg" className="h-12 flex-1 text-base text-muted-foreground" onClick={actions.discard}>
            <Trash2Icon /> Discard
          </Button>
        </div>
      </Footer>
    </>
  )
}

function Offer({ phase, actions }: { phase: Extract<VoicePhase, { kind: 'offer' }>; actions: VoiceActions }) {
  const when = new Date(phase.startedAt).toLocaleString([], { weekday: 'long', hour: '2-digit', minute: '2-digit' })
  return (
    <>
      <Body>
        <MicIcon className="size-12 text-muted-foreground" />
        <div className="text-xl font-medium">Unsent recording</div>
        <p className="text-center text-sm text-muted-foreground">
          {when}
          {phase.durationMs ? ` · ${formatElapsed(phase.durationMs)}` : ''}
        </p>
        {phase.error ? <p className="max-w-sm text-center text-sm break-words text-muted-foreground">Last try: {phase.error}</p> : null}
      </Body>
      <Footer>
        <Button autoFocus className="h-16 w-full rounded-2xl text-lg" onClick={actions.transcribeOffered}>
          Transcribe
        </Button>
        <div className="flex gap-2">
          <Button variant="outline" size="lg" className="h-12 flex-1 text-base" onClick={actions.cancel}>
            Later
          </Button>
          <Button variant="ghost" size="lg" className="h-12 flex-1 text-base text-muted-foreground" onClick={actions.discard}>
            <Trash2Icon /> Discard
          </Button>
        </div>
      </Footer>
    </>
  )
}

function Waiting({ title, note, actions }: { title: string; note?: string; actions: VoiceActions }) {
  return (
    <>
      <Body>
        <Loader2Icon className="size-12 animate-spin text-muted-foreground" />
        <div className="text-xl font-medium">{title}</div>
        {note ? <p className="text-center text-sm text-muted-foreground">{note}</p> : null}
      </Body>
      <Footer>
        <Button autoFocus variant="outline" className="h-16 w-full rounded-2xl text-lg" onClick={actions.cancel}>
          Cancel
        </Button>
      </Footer>
    </>
  )
}

function Transcript({ text }: { text: string }) {
  return <p className="max-h-[50vh] w-full max-w-md overflow-y-auto rounded-xl border bg-muted/30 p-4 text-lg leading-relaxed whitespace-pre-wrap">{text}</p>
}

const Body = ({ children }: { children: React.ReactNode }) => <div className="flex min-h-0 flex-1 flex-col items-center justify-center gap-5 px-4 py-6">{children}</div>
const Footer = ({ children }: { children: React.ReactNode }) => <div className="flex flex-col gap-3 px-4 pt-2 pb-[max(1.5rem,env(safe-area-inset-bottom))]">{children}</div>

// A Radix dialog, so it stacks over the new-session drawer/dialog instead of closing it.
export function VoiceOverlay({ phase, actions }: { phase: VoicePhase; actions: VoiceActions }) {
  if (phase.kind === 'idle') return null
  return (
    <DialogPrimitive.Root open>
      <DialogPrimitive.Portal>
        <DialogPrimitive.Overlay className="fixed inset-0 z-[60] bg-background sm:bg-black/50 sm:backdrop-blur-sm" />
        <DialogPrimitive.Content
          aria-describedby={undefined}
          // Esc backs out (discards a recording, cancels a send); never while editing a transcript.
          onEscapeKeyDown={(e) => {
            e.preventDefault()
            if (phase.kind !== 'review') actions.cancel()
          }}
          onPointerDownOutside={(e) => e.preventDefault()}
          onInteractOutside={(e) => e.preventDefault()}
          onKeyDown={(e) => {
            if (e.key === 'Enter' && phase.kind === 'recording') {
              e.preventDefault()
              actions.stop()
            }
          }}
          className="fixed inset-0 z-[60] flex flex-col bg-background pt-[env(safe-area-inset-top)] outline-none sm:inset-auto sm:top-1/2 sm:left-1/2 sm:min-h-[28rem] sm:w-full sm:max-w-md sm:-translate-x-1/2 sm:-translate-y-1/2 sm:rounded-2xl sm:border sm:pt-0 sm:shadow-2xl"
        >
          <div className="flex items-center justify-between px-4 pt-3">
            <DialogPrimitive.Title className="truncate text-sm font-medium text-muted-foreground">{phase.label}</DialogPrimitive.Title>
            <Button variant="ghost" size="icon-lg" aria-label="Close" onClick={actions.cancel}>
              <XIcon className="size-5" />
            </Button>
          </div>
          {phase.kind === 'starting' ? (
            <Waiting title="Opening the microphone…" note="Start speaking when it shows Recording." actions={actions} />
          ) : phase.kind === 'recording' ? (
            <Recording phase={phase} actions={actions} />
          ) : phase.kind === 'transcribing' ? (
            <Waiting title="Transcribing…" actions={actions} />
          ) : phase.kind === 'confirm' ? (
            <Confirm phase={phase} actions={actions} />
          ) : phase.kind === 'review' ? (
            <Review phase={phase} actions={actions} />
          ) : phase.kind === 'failed' ? (
            <Failed phase={phase} actions={actions} />
          ) : (
            <Offer phase={phase} actions={actions} />
          )}
        </DialogPrimitive.Content>
      </DialogPrimitive.Portal>
    </DialogPrimitive.Root>
  )
}
