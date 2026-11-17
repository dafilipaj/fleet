// The app's one voice recorder: every mic button (operator, composer, new-session prompt) opens
// the same full-screen recorder (components/VoiceOverlay.tsx). It shows the mic opening, then a
// live level and a timer, so a slow start is visible; audio goes to IndexedDB as it is recorded
// (lib/voiceStore.ts), so a failed transcription keeps the take for Retry or a later visit.
// The text goes into the field that opened it at once, or — the operator, which sends it — after
// a CONFIRM_MS pause with Cancel.
import { useCallback, useEffect, useMemo, useRef, useState, type ReactNode } from 'react'
import { toast } from 'sonner'

import { ApiError, api } from '@/api/client'
import { VoiceOverlay, type VoicePhase } from '@/components/VoiceOverlay'
import { VoiceContext, type VoiceOwner, type VoiceRecorder, type VoiceState } from '@/hooks/useVoice'
import { CONFIRM_MS, HEARTBEAT_MS, KEEP_UNRECOGNIZED_MS, emptyRecordingReason, formatElapsed, meterHeardNothing, micUnavailableReason, pickMimeType, staleTakes } from '@/lib/voice'
import { NO_METER, openAudioContext, startMeter, type VoiceMeter } from '@/lib/voiceMeter'
import { voiceStore, type VoiceTake } from '@/lib/voiceStore'

// A forgotten recording stops itself (well under the server's 25 MB at this bitrate).
const MAX_RECORDING_MS = 10 * 60 * 1000
const CHUNK_MS = 1000
const BITRATE = 48000

function unavailableReason(): string | null {
  return micUnavailableReason({
    secure: window.isSecureContext,
    getUserMedia: typeof navigator.mediaDevices?.getUserMedia === 'function',
    recorder: typeof MediaRecorder !== 'undefined',
  })
}

function micError(err: unknown): string {
  const name = (err as DOMException)?.name
  if (name === 'NotAllowedError' || name === 'SecurityError') return 'Microphone access was denied'
  if (name === 'NotFoundError') return 'No microphone found'
  return (err as Error)?.message || 'Could not start recording'
}

const errorText = (err: unknown) => (err instanceof ApiError ? err.message || `HTTP ${err.status}` : String((err as Error)?.message ?? err))
const newId = () => (typeof crypto.randomUUID === 'function' ? crypto.randomUUID() : `${Date.now()}-${Math.random().toString(36).slice(2)}`)
// The store is a safety net: when it fails, the take still lives in memory for this visit.
const quiet = (p: Promise<unknown>) => void p.catch((err) => console.warn('voice store:', err))
// Every step of a take goes to the server's log: a take that never reaches /api/transcribe
// (closed by the app, dropped on the device) leaves no other trace.
const vlog = (event: string, data: Record<string, string | number | boolean | null | undefined> = {}) => void api.voiceLog({ event, ...data })
const round3 = (n: number | null) => (n === null ? null : Math.round(n * 1000) / 1000)

const stateOf = (phase: VoicePhase): VoiceState =>
  phase.kind === 'idle' ? 'idle' : phase.kind === 'starting' || phase.kind === 'recording' ? 'recording' : 'transcribing'

/** The take being worked on: its record, its audio once stopped, the field it goes to. */
interface Current {
  take: VoiceTake
  audio: Blob | null
  owner: VoiceOwner | null
  /** Offered again from an earlier visit: its text is always reviewed, never sent on its own. */
  recovered: boolean
  /** Why its text must be reviewed before it is sent (the meter heard nothing), if it must. */
  doubt?: string
}

export function VoiceProvider({ children }: { children: ReactNode }) {
  const [phase, setPhase] = useState<VoicePhase>({ kind: 'idle' })
  const [ownerId, setOwnerId] = useState<string | null>(null)
  const owners = useRef<VoiceOwner[]>([])
  const current = useRef<Current | null>(null)
  const recorder = useRef<MediaRecorder | null>(null)
  const stream = useRef<MediaStream | null>(null)
  const meter = useRef<VoiceMeter>(NO_METER)
  const timers = useRef<number[]>([])
  const wakeLock = useRef<{ release: () => Promise<void> } | null>(null)
  const discard = useRef(false)
  const startToken = useRef(0)
  const inflight = useRef<AbortController | null>(null)
  const confirmTimer = useRef<number | undefined>(undefined)

  const register = useCallback((owner: VoiceOwner) => {
    owners.current = [...owners.current.filter((o) => o !== owner), owner]
    return () => {
      owners.current = owners.current.filter((o) => o !== owner)
    }
  }, [])
  // The owner, if its field is still mounted.
  const live = useCallback((owner: VoiceOwner | null) => (owner && owners.current.includes(owner) ? owner : null), [])

  const close = useCallback(() => {
    window.clearTimeout(confirmTimer.current)
    current.current = null
    setOwnerId(null)
    setPhase({ kind: 'idle' })
  }, [])

  /** Close and forget the take (its text was delivered, or it was thrown away). */
  const finish = useCallback(() => {
    const id = current.current?.take.id
    if (id) quiet(voiceStore.drop(id))
    close()
  }, [close])

  const release = useCallback(() => {
    timers.current.forEach((t) => window.clearTimeout(t))
    timers.current = []
    meter.current.close()
    meter.current = NO_METER
    stream.current?.getTracks().forEach((t) => t.stop())
    stream.current = null
    recorder.current = null
    void wakeLock.current?.release().catch(() => {})
    wakeLock.current = null
  }, [])

  const delivering = useRef(false)
  /** The text to its field. It is forgotten only once taken: a field that is gone or a send that fails keeps it for review. */
  const deliver = useCallback(
    async (text: string, owner: VoiceOwner | null) => {
      if (delivering.current) return
      const cur = current.current
      const label = cur?.take.label ?? 'Voice'
      const target = live(owner)
      if (!target) {
        vlog('deliver-failed', { label, reason: 'field gone' })
        setPhase({ kind: 'review', text, label, action: null, note: 'The field this was for is closed. Copy the text, or discard it.' })
        return
      }
      delivering.current = true
      try {
        await target.target.current.onText(text)
      } catch (err) {
        if (current.current !== cur) return
        const error = errorText(err)
        vlog('deliver-failed', { label, reason: error.slice(0, 80) })
        setPhase({ kind: 'review', text, label, action: target.target.current.confirm ? 'Send' : 'Insert', note: `Not sent: ${error}` })
        return
      } finally {
        delivering.current = false
      }
      vlog('delivered', { label, chars: text.length })
      if (current.current === cur) finish()
    },
    [finish, live],
  )

  const transcribed = useCallback(
    (text: string) => {
      const cur = current.current
      if (!cur) return
      const owner = live(cur.owner)
      if (cur.recovered || !owner) {
        setPhase({ kind: 'review', text, label: cur.take.label, action: owners.current.at(-1)?.target.current.confirm ? 'Send' : owners.current.length ? 'Insert' : null })
        return
      }
      if (owner.target.current.confirm) {
        if (cur.doubt) {
          setPhase({ kind: 'review', text, label: cur.take.label, action: 'Send', note: cur.doubt })
          return
        }
        setPhase({ kind: 'confirm', text, until: Date.now() + CONFIRM_MS, label: cur.take.label })
        confirmTimer.current = window.setTimeout(() => void deliver(text, owner), CONFIRM_MS)
        return
      }
      void deliver(text, owner)
    },
    [deliver, live],
  )

  const transcribe = useCallback(async () => {
    const cur = current.current
    if (!cur) return
    const controller = new AbortController()
    inflight.current = controller
    setPhase({ kind: 'transcribing', label: cur.take.label })
    try {
      const audio = cur.audio ?? (cur.audio = await voiceStore.blob(cur.take.id, cur.take.mimeType))
      if (!audio.size) {
        vlog('empty', { label: cur.take.label })
        toast.error('That recording is empty', { description: 'Nothing of it was saved on this device.' })
        finish()
        return
      }
      const { text } = await api.transcribe(audio, cur.take.hint, { signal: controller.signal, ms: cur.take.durationMs })
      if (controller.signal.aborted || current.current !== cur) return
      vlog('transcribed', { label: cur.take.label, chars: text.length })
      if (!text) {
        // A real take the model heard nothing in stays for Retry; a short one is let go.
        if ((cur.take.durationMs ?? 0) >= KEEP_UNRECOGNIZED_MS) {
          const error = 'No speech was recognized in this recording.'
          quiet(voiceStore.update(cur.take.id, { error }))
          setPhase({ kind: 'failed', error, label: cur.take.label, durationMs: cur.take.durationMs ?? null })
          return
        }
        toast.error('Nothing sent', { description: 'No speech was recognized.' })
        finish()
        return
      }
      transcribed(text)
    } catch (err) {
      if (controller.signal.aborted || current.current !== cur) return
      const error = errorText(err)
      vlog('transcribe-failed', { label: cur.take.label, error: error.slice(0, 80) })
      quiet(voiceStore.update(cur.take.id, { error }))
      setPhase({ kind: 'failed', error, label: cur.take.label, durationMs: cur.take.durationMs ?? null })
    } finally {
      if (inflight.current === controller) inflight.current = null
    }
  }, [finish, transcribed])

  const start = useCallback(
    async (owner: VoiceOwner) => {
      if (current.current) return
      const reason = unavailableReason()
      if (reason) {
        toast.error(reason)
        return
      }
      const { hint, label } = owner.target.current
      const token = (startToken.current += 1)
      const ctx = openAudioContext()
      const take: VoiceTake = { id: newId(), startedAt: Date.now(), mimeType: '', hint, label, heartbeat: Date.now() }
      current.current = { take, audio: null, owner, recovered: false }
      discard.current = false
      setOwnerId(owner.id)
      setPhase({ kind: 'starting', label })
      vlog('start', { label, audioContext: ctx?.state ?? 'none' })

      let media: MediaStream
      try {
        media = await navigator.mediaDevices.getUserMedia({ audio: true })
      } catch (err) {
        void ctx?.close().catch(() => {})
        vlog('mic-failed', { label, error: micError(err) })
        if (startToken.current === token) {
          toast.error(micError(err))
          close()
        }
        return
      }
      // Cancelled while the mic was opening.
      if (startToken.current !== token || current.current?.take !== take) {
        media.getTracks().forEach((t) => t.stop())
        void ctx?.close().catch(() => {})
        return
      }
      const mimeType = pickMimeType((t) => MediaRecorder.isTypeSupported(t))
      let rec: MediaRecorder
      try {
        rec = new MediaRecorder(media, { ...(mimeType ? { mimeType } : {}), audioBitsPerSecond: BITRATE })
      } catch (err) {
        media.getTracks().forEach((t) => t.stop())
        void ctx?.close().catch(() => {})
        toast.error(micError(err))
        close()
        return
      }
      take.mimeType = rec.mimeType || mimeType || 'audio/webm'
      quiet(voiceStore.put(take))
      const chunks: Blob[] = []
      let n = 0
      let began = Date.now()
      rec.ondataavailable = (e) => {
        if (!e.data.size) return
        chunks.push(e.data)
        quiet(voiceStore.putChunk(take.id, (n += 1), e.data))
      }
      rec.onstart = () => {
        began = Date.now()
        vlog('recording', { label, mime: take.mimeType, openMs: began - take.startedAt, audioContext: ctx?.state ?? 'none' })
        if (current.current?.take === take) setPhase({ kind: 'recording', startedAt: began, label })
      }
      // iOS can take the mic away (a call, a route change): the recorder then stops by itself.
      const track = media.getAudioTracks()[0]
      if (track) {
        track.onended = () => vlog('track-ended', { label, ms: Date.now() - began })
        track.onmute = () => vlog('track-muted', { label, ms: Date.now() - began })
      }
      rec.onerror = (e) => vlog('recorder-error', { label, error: String((e as Event & { error?: Error }).error?.message ?? e.type) })
      rec.onstop = () => {
        const ms = Date.now() - began
        const peak = meter.current.peak()
        release()
        const cur = current.current
        const audio = new Blob(chunks, { type: take.mimeType })
        vlog('stop', { label, ms, bytes: audio.size, chunks: chunks.length, peak: round3(peak), discarded: discard.current, current: cur?.take === take })
        if (!cur || cur.take !== take) return
        if (discard.current) {
          finish()
          return
        }
        const empty = emptyRecordingReason({ durationMs: ms })
        if (empty) {
          toast.message('Nothing sent', { description: empty })
          finish()
          return
        }
        take.durationMs = ms
        // No chunks in memory: transcribe() reads the take back from the device.
        if (audio.size) cur.audio = audio
        if (meterHeardNothing(peak)) cur.doubt = 'The level meter heard nothing while recording. Check the text before you send it.'
        quiet(voiceStore.update(take.id, { durationMs: ms, mimeType: take.mimeType }))
        void transcribe()
      }
      stream.current = media
      recorder.current = rec
      meter.current = startMeter(ctx, media)
      rec.start(CHUNK_MS)
      const beat = window.setInterval(() => quiet(voiceStore.update(take.id, { heartbeat: Date.now() })), HEARTBEAT_MS)
      const limit = window.setTimeout(() => {
        toast.message('Recording stopped', { description: 'The 10-minute limit was reached.' })
        if (recorder.current?.state === 'recording') recorder.current.stop()
      }, MAX_RECORDING_MS)
      timers.current = [beat, limit]
      // A phone in a car holder must not lock mid-sentence.
      const wl = (navigator as Navigator & { wakeLock?: { request: (t: 'screen') => Promise<{ release: () => Promise<void> }> } }).wakeLock
      wl?.request('screen').then(
        (lock) => {
          if (recorder.current === rec) wakeLock.current = lock
          else void lock.release().catch(() => {})
        },
        () => {},
      )
    },
    [close, finish, release, transcribe],
  )

  const stop = useCallback(() => {
    if (recorder.current?.state === 'recording') recorder.current.stop()
    else if (phase.kind === 'recording') vlog('stop-ignored', { recorder: recorder.current?.state ?? 'none' })
    else if (phase.kind === 'starting') {
      startToken.current += 1
      finish()
    }
  }, [phase.kind, finish])

  /** Recording: throw it away. Transcribing: give up on it. Confirm: hold the text for review. */
  const cancel = useCallback(() => {
    vlog('cancel', { phase: phase.kind })
    switch (phase.kind) {
      case 'starting':
        startToken.current += 1
        finish()
        return
      case 'recording':
        discard.current = true
        if (recorder.current?.state === 'recording') recorder.current.stop()
        else finish()
        return
      case 'transcribing': {
        // Never loses the take: Retry, keep it for later or discard it from there.
        inflight.current?.abort()
        inflight.current = null
        const take = current.current?.take
        if (take) setPhase({ kind: 'failed', error: 'Transcription cancelled.', label: take.label, durationMs: take.durationMs ?? null })
        else close()
        return
      }
      case 'confirm':
        window.clearTimeout(confirmTimer.current)
        setPhase({ kind: 'review', text: phase.text, label: phase.label, action: live(current.current?.owner ?? null) ? 'Send' : null })
        return
      case 'failed':
      case 'offer':
        close() // the take stays on this device
        return
      case 'review':
        finish()
        return
    }
  }, [phase, close, finish, live])

  /** Offer takes an earlier visit left behind (a failed transcription, a closed app). */
  const offerStale = useCallback(async () => {
    let takes: VoiceTake[]
    try {
      takes = staleTakes(await voiceStore.all(), Date.now(), current.current?.take.id ?? null)
    } catch {
      return
    }
    if (!takes.length) return
    const take = takes[0]
    const when = new Date(take.startedAt).toLocaleString([], { weekday: 'short', hour: '2-digit', minute: '2-digit' })
    toast('Unsent voice recording', {
      id: 'voice-offer',
      description: `${take.label} · ${when}${take.durationMs ? ` · ${formatElapsed(take.durationMs)}` : ''}${takes.length > 1 ? ` (+${takes.length - 1} more)` : ''}`,
      duration: Infinity,
      action: {
        label: 'Open',
        onClick: () => {
          if (current.current) return
          current.current = { take, audio: null, owner: null, recovered: true }
          setPhase({ kind: 'offer', label: take.label, startedAt: take.startedAt, durationMs: take.durationMs ?? null, error: take.error ?? null })
        },
      },
    })
  }, [])

  useEffect(() => {
    const t = window.setTimeout(() => void offerStale(), 1500)
    return () => window.clearTimeout(t)
  }, [offerStale])
  // Offer the rest once the recorder closes after one was handled.
  const wasOffer = useRef(false)
  useEffect(() => {
    if (phase.kind === 'offer') wasOffer.current = true
    else if (phase.kind === 'idle' && wasOffer.current) {
      wasOffer.current = false
      void offerStale()
    }
  }, [phase.kind, offerStale])

  useEffect(
    () => () => {
      if (current.current) vlog('unmount', { label: current.current.take.label })
      discard.current = true
      if (recorder.current?.state === 'recording') recorder.current.stop()
      inflight.current?.abort()
      window.clearTimeout(confirmTimer.current)
      release()
    },
    [release],
  )

  const actions = useMemo(
    () => ({
      stop,
      cancel,
      retry: () => void transcribe(),
      /** Failed: keep the take on this device for later. */
      keep: () => {
        close()
        toast.message('Kept on this device', { description: 'It is offered again the next time Fleet opens.' })
      },
      discard: finish,
      /** Confirm: send now instead of waiting. */
      sendNow: (text: string) => {
        window.clearTimeout(confirmTimer.current)
        void deliver(text, current.current?.owner ?? null)
      },
      /** Review: the (edited) text to the field that opened it, else the newest one open. */
      use: (text: string) => {
        const cur = current.current
        void deliver(text, live(cur?.owner ?? null) ?? (cur?.recovered ? (owners.current.at(-1) ?? null) : null))
      },
      copy: async (text: string) => {
        try {
          await navigator.clipboard.writeText(text)
          toast.success('Copied')
          finish()
        } catch {
          toast.error("Couldn't copy")
        }
      },
      transcribeOffered: () => void transcribe(),
      meter: () => meter.current,
    }),
    [stop, cancel, transcribe, close, finish, deliver, live],
  )

  const value = useMemo<VoiceRecorder>(
    () => ({
      ownerId,
      state: stateOf(phase),
      startedAt: phase.kind === 'recording' ? phase.startedAt : null,
      start: (owner) => void start(owner),
      stop,
      cancel,
      register,
    }),
    [ownerId, phase, start, stop, cancel, register],
  )

  return (
    <VoiceContext.Provider value={value}>
      {children}
      {phase.kind !== 'idle' ? <VoiceOverlay phase={phase} actions={actions} /> : null}
    </VoiceContext.Provider>
  )
}
