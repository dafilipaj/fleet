import { useCallback, useEffect, useLayoutEffect, useRef, useState } from 'react'
import { toast } from 'sonner'

import { ApiError, api } from '@/api/client'
import { emptyRecordingReason, micUnavailableReason, peakOf, pickMimeType } from '@/lib/voice'

export type VoiceState = 'idle' | 'recording' | 'transcribing'

// A forgotten recording stops itself (well under the server's 25 MB at opus bitrates).
const MAX_RECORDING_MS = 10 * 60 * 1000

function unavailableReason(): string | null {
  if (typeof window === 'undefined') return 'No browser'
  return micUnavailableReason({
    secure: window.isSecureContext,
    getUserMedia: typeof navigator.mediaDevices?.getUserMedia === 'function',
    recorder: typeof MediaRecorder !== 'undefined',
  })
}

/**
 * The mic's level while recording: the loudest sample so far, or null when it could not be
 * measured (no Web Audio, or the context never ran), so an unmeasured recording is still sent.
 */
type Meter = { peak: () => number | null; close: () => void }

/** Create inside the tap, before any await: a context made later may stay suspended. */
function openAudioContext(): AudioContext | null {
  try {
    const Ctx = window.AudioContext ?? (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext
    return Ctx ? new Ctx() : null
  } catch {
    return null
  }
}

function startMeter(ctx: AudioContext | null, media: MediaStream): Meter {
  if (!ctx) return { peak: () => null, close: () => {} }
  let max = 0
  let measured = false
  let timer: number | undefined
  try {
    void ctx.resume().catch(() => {})
    const analyser = ctx.createAnalyser()
    analyser.fftSize = 2048
    ctx.createMediaStreamSource(media).connect(analyser)
    const buf = new Float32Array(analyser.fftSize)
    timer = window.setInterval(() => {
      if (ctx.state !== 'running') return
      analyser.getFloatTimeDomainData(buf)
      measured = true
      max = Math.max(max, peakOf(buf))
    }, 50)
  } catch {
    measured = false
  }
  return {
    peak: () => (measured ? max : null),
    close: () => {
      window.clearInterval(timer)
      void ctx.close().catch(() => {})
    },
  }
}

function micError(err: unknown): string {
  const name = (err as DOMException)?.name
  if (name === 'NotAllowedError' || name === 'SecurityError') return 'Microphone access was denied'
  if (name === 'NotFoundError') return 'No microphone found'
  return (err as Error)?.message || 'Could not start recording'
}

/**
 * Record from the mic, send it to POST /api/transcribe on stop, hand the text to `onText`.
 * `hint`: words the transcriber should expect (session name, repo). One recording at a time;
 * the mic is released as soon as recording stops, is cancelled, or the component unmounts.
 */
export function useVoice({ onText, hint }: { onText: (text: string) => void; hint: string }) {
  const [state, setState] = useState<VoiceState>('idle')
  const [startedAt, setStartedAt] = useState<number | null>(null)
  const recorder = useRef<MediaRecorder | null>(null)
  const stream = useRef<MediaStream | null>(null)
  const chunks = useRef<Blob[]>([])
  const discard = useRef(false)
  const limitTimer = useRef<number | undefined>(undefined)
  const inflight = useRef<AbortController | null>(null)
  const meter = useRef<Meter | null>(null)
  // Latest callbacks for the recorder's onstop (it outlives renders).
  const latest = useRef({ onText, hint })
  useLayoutEffect(() => {
    latest.current = { onText, hint }
  })

  const release = useCallback(() => {
    window.clearTimeout(limitTimer.current)
    meter.current?.close()
    meter.current = null
    stream.current?.getTracks().forEach((t) => t.stop())
    stream.current = null
    recorder.current = null
    setStartedAt(null)
  }, [])

  const send = useCallback(async function transcribe(audio: Blob, ms: number): Promise<void> {
    const controller = new AbortController()
    inflight.current = controller
    setState('transcribing')
    try {
      const { text } = await api.transcribe(audio, latest.current.hint, { signal: controller.signal, ms })
      if (text) latest.current.onText(text)
      else toast.message('Nothing to transcribe', { description: 'No speech was recognized.' })
    } catch (err) {
      if (controller.signal.aborted) return
      // The recording stays retryable: a long dictation is not lost to one failed request.
      toast.error("Couldn't transcribe", {
        description: err instanceof ApiError ? err.message || `HTTP ${err.status}` : String(err),
        action: { label: 'Retry', onClick: () => void transcribe(audio, ms) },
        duration: 60_000,
      })
    } finally {
      if (inflight.current === controller) {
        inflight.current = null
        setState('idle')
      }
    }
  }, [])

  const start = useCallback(async () => {
    if (state !== 'idle') return
    const reason = unavailableReason()
    if (reason) {
      toast.error(reason)
      return
    }
    const ctx = openAudioContext()
    let media: MediaStream
    try {
      media = await navigator.mediaDevices.getUserMedia({ audio: true })
    } catch (err) {
      void ctx?.close().catch(() => {})
      toast.error(micError(err))
      return
    }
    const mimeType = pickMimeType((t) => MediaRecorder.isTypeSupported(t))
    const rec = new MediaRecorder(media, mimeType ? { mimeType } : undefined)
    chunks.current = []
    discard.current = false
    rec.ondataavailable = (e) => {
      if (e.data.size) chunks.current.push(e.data)
    }
    const level = startMeter(ctx, media)
    let began = Date.now()
    rec.onstart = () => {
      began = Date.now()
    }
    rec.onstop = () => {
      const audio = new Blob(chunks.current, { type: rec.mimeType || mimeType || 'audio/webm' })
      const ms = Date.now() - began
      const peak = level.peak()
      chunks.current = []
      release()
      if (discard.current || !audio.size) {
        setState('idle')
        return
      }
      const empty = emptyRecordingReason({ durationMs: ms, peak })
      if (empty) {
        setState('idle')
        toast.message('Nothing sent', { description: empty })
        return
      }
      void send(audio, ms)
    }
    meter.current = level
    stream.current = media
    recorder.current = rec
    rec.start()
    setStartedAt(Date.now())
    setState('recording')
    limitTimer.current = window.setTimeout(() => {
      toast.message('Recording stopped', { description: 'The 10-minute limit was reached.' })
      recorder.current?.stop()
    }, MAX_RECORDING_MS)
  }, [state, release, send])

  /** Stop and transcribe. */
  const stop = useCallback(() => {
    if (recorder.current?.state === 'recording') recorder.current.stop()
  }, [])

  /** Stop without transcribing, or drop a transcription in flight. */
  const cancel = useCallback(() => {
    if (recorder.current?.state === 'recording') {
      discard.current = true
      recorder.current.stop()
    } else if (inflight.current) {
      inflight.current.abort()
      inflight.current = null
      setState('idle')
    }
  }, [])

  useEffect(
    () => () => {
      discard.current = true
      if (recorder.current?.state === 'recording') recorder.current.stop()
      inflight.current?.abort()
      window.clearTimeout(limitTimer.current)
      meter.current?.close()
      stream.current?.getTracks().forEach((t) => t.stop())
    },
    [],
  )

  return { state, startedAt, start, stop, cancel }
}
