import { useEffect, useState } from 'react'
import { Loader2Icon, MicIcon, SendHorizontalIcon, SquareIcon, Volume2Icon, VolumeXIcon, XIcon } from 'lucide-react'

import { Button } from '@/components/ui/button'
import type { VoiceState } from '@/hooks/useVoice'
import type { SpeechPlayer } from '@/hooks/useSpeechPlayer'
import { nextRate } from '@/lib/operator'
import { formatElapsed } from '@/lib/voice'
import { cn } from '@/lib/utils'

export interface VoiceDockProps {
  voice: { state: VoiceState; startedAt: number | null }
  speech: SpeechPlayer
  /** The session works on a turn (or a sent message has no reply yet). */
  thinking: boolean
  /** Voice input works on this server (a transcription key). */
  canRecord: boolean
  autoSpeak: boolean
  onAutoSpeak: (on: boolean) => void
  onMic: () => void
  /** A typed message (tagged like a spoken one). */
  onType: (text: string) => void
  onExit: () => void
  lockedReason?: string | null
}

/** Voice mode's footer in place of the composer: a big mic, what is happening, a small text field. */
export function VoiceDock({ voice, speech, thinking, canRecord, autoSpeak, onAutoSpeak, onMic, onType, onExit, lockedReason }: VoiceDockProps) {
  const [draft, setDraft] = useState('')
  const [now, setNow] = useState(() => Date.now())
  useEffect(() => {
    if (voice.state !== 'recording') return
    const t = window.setInterval(() => setNow(Date.now()), 500)
    return () => window.clearInterval(t)
  }, [voice.state])

  const phase = lockedReason
    ? lockedReason
    : voice.state === 'recording'
      ? `Listening… ${formatElapsed(now - (voice.startedAt ?? now))} — tap to send`
      : voice.state === 'transcribing'
        ? 'Transcribing…'
        : speech.state === 'loading'
          ? 'Preparing the reply…'
          : speech.state === 'playing'
            ? 'Speaking — tap the mic to interrupt'
            : thinking
              ? 'Working…'
              : 'Tap and speak'

  return (
    <div className="shrink-0 border-t bg-background/95 px-safe pb-safe">
      <div className="mx-auto flex w-full max-w-3xl flex-col items-center gap-2 px-4 pt-2 pb-3">
        <div className="flex w-full items-center gap-1">
          <span className="flex min-w-0 flex-1 items-center gap-1.5 text-xs font-medium text-primary">
            <MicIcon className="size-3.5" /> Voice mode
          </span>
          <Button
            variant="ghost"
            size="icon"
            aria-label={autoSpeak ? 'Read replies aloud: on' : 'Read replies aloud: off'}
            title={autoSpeak ? 'Replies are read aloud' : 'Replies are not read aloud'}
            onClick={() => {
              if (autoSpeak) speech.stop()
              onAutoSpeak(!autoSpeak)
            }}
            className="size-9 rounded-xl"
            disabled={!speech.available}
          >
            {autoSpeak && speech.available ? <Volume2Icon className="size-4" /> : <VolumeXIcon className="size-4" />}
          </Button>
          <Button
            variant="ghost"
            size="icon"
            aria-label={`Reading speed ${speech.rate}×`}
            title="Reading speed (tap to change)"
            onClick={() => speech.setRate(nextRate(speech.rate))}
            className="size-9 rounded-xl text-xs font-semibold tabular-nums"
            disabled={!speech.available}
          >
            {speech.rate}×
          </Button>
          <Button variant="ghost" size="sm" onClick={onExit} className="h-9 gap-1 rounded-xl px-2.5 text-xs" title="Back to the keyboard">
            <XIcon className="size-4" /> Exit
          </Button>
        </div>
        <p className="h-5 truncate text-sm text-muted-foreground" aria-live="polite">
          {phase}
        </p>
        <button
          type="button"
          onClick={onMic}
          disabled={!canRecord || Boolean(lockedReason)}
          aria-label={voice.state === 'recording' ? 'Stop and send' : voice.state === 'transcribing' ? 'Cancel' : 'Speak'}
          title={canRecord ? undefined : 'Voice input is off on this server (no transcription key)'}
          className={cn(
            'grid size-24 place-items-center rounded-full shadow-lg transition-transform active:scale-95 disabled:opacity-40',
            voice.state === 'recording' ? 'animate-pulse bg-destructive text-white' : 'bg-primary text-primary-foreground',
          )}
        >
          {voice.state === 'recording' ? (
            <SquareIcon className="size-9 fill-current" />
          ) : voice.state === 'transcribing' ? (
            <Loader2Icon className="size-10 animate-spin" />
          ) : (
            <MicIcon className="size-10" />
          )}
        </button>
        <form
          className="flex w-full items-center gap-1 rounded-full border bg-card pr-1 pl-3.5"
          onSubmit={(e) => {
            e.preventDefault()
            const t = draft.trim()
            if (!t) return
            setDraft('')
            speech.unlock()
            onType(t)
          }}
        >
          <input
            value={draft}
            onChange={(e) => setDraft(e.target.value)}
            placeholder="Or type…"
            aria-label="Message the session"
            disabled={Boolean(lockedReason)}
            className="h-9 min-w-0 flex-1 bg-transparent text-base outline-none"
          />
          <Button type="submit" variant="ghost" size="icon" aria-label="Send" disabled={!draft.trim()} className="size-8 rounded-full">
            <SendHorizontalIcon />
          </Button>
        </form>
      </div>
    </div>
  )
}
