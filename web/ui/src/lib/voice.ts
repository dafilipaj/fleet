// Voice prompts: pure helpers for the composer's mic (hooks/useVoice.ts records, the server
// transcribes — web/lib/transcribe.mjs).

/** Recording formats in order of preference: webm/opus (Chrome, Firefox), mp4/aac (Safari, WKWebView). */
const MIME_TYPES = ['audio/webm;codecs=opus', 'audio/webm', 'audio/mp4', 'audio/ogg;codecs=opus']

/** The first format this browser records, or '' (let MediaRecorder pick its default). */
export function pickMimeType(isSupported: (type: string) => boolean): string {
  return MIME_TYPES.find((t) => isSupported(t)) ?? ''
}

/** The transcript appended to what is already typed: one space between them, nothing when empty. */
export function appendTranscript(current: string, transcript: string): string {
  const t = transcript.trim()
  if (!t) return current
  if (!current || /\s$/.test(current)) return current + t
  return `${current} ${t}`
}

/** Why the mic cannot work in this page, or null. Insecure origins (plain http to a non-localhost host) have no getUserMedia. */
export function micUnavailableReason(env: { secure: boolean; getUserMedia: boolean; recorder: boolean }): string | null {
  if (!env.secure) return 'The microphone needs HTTPS (or localhost)'
  if (!env.getUserMedia || !env.recorder) return 'This browser cannot record audio'
  return null
}

// A recording this short is a slipped tap, not a prompt.
export const MIN_RECORDING_MS = 400
// Peak sample amplitude (0–1) under which a recording is silence: about -40 dBFS, well under
// quiet speech and above a muted or dead mic, which reads 0.
export const SILENCE_PEAK = 0.01

/** Why a recording is not worth transcribing (a slipped tap), or null. */
export function emptyRecordingReason({ durationMs }: { durationMs: number }): string | null {
  return durationMs < MIN_RECORDING_MS ? 'The recording was too short.' : null
}

/**
 * The level meter heard nothing (`peak`: the loudest sample; null when unmeasured). Never a
 * reason to drop the take: on iOS the meter can read silence while the recorder gets speech
 * (a Bluetooth / car route switching as the mic opens), so the take is still transcribed, and
 * its text reviewed before it is sent: silence comes back as text made up from the prompt.
 */
export function meterHeardNothing(peak: number | null): boolean {
  return peak !== null && peak < SILENCE_PEAK
}

/** A take whose transcription came back empty is kept (Retry) from this long on; shorter ones are let go. */
export const KEEP_UNRECOGNIZED_MS = 3000

/** The loudest sample in a block of time-domain samples (-1…1). */
export function peakOf(samples: ArrayLike<number>): number {
  let max = 0
  for (let i = 0; i < samples.length; i += 1) {
    const v = Math.abs(samples[i])
    if (v > max) max = v
  }
  return max
}

/** Operator prompts: the transcript waits this long, cancellable, before it is sent. */
export const CONFIRM_MS = 5000
/** A recording tab touches its take this often… */
export const HEARTBEAT_MS = 5000
/** …and a take untouched this long is no longer being recorded: it is offered again. */
export const STALE_MS = 30 * 1000
/** After this long without a sound the recorder warns that the mic hears nothing. */
export const SILENCE_WARN_MS = 2500

/** Takes left on this device by an earlier visit (not the one being recorded), oldest first. */
export function staleTakes<T extends { id: string; heartbeat: number; startedAt: number }>(takes: T[], now: number, activeId: string | null): T[] {
  return takes.filter((t) => t.id !== activeId && now - t.heartbeat > STALE_MS).sort((a, b) => a.startedAt - b.startedAt)
}

/** Whether the recorder should say the mic hears nothing (an unmeasured level never warns). */
export function hearsNothing({ elapsedMs, peak }: { elapsedMs: number; peak: number | null }): boolean {
  return peak !== null && elapsedMs >= SILENCE_WARN_MS && peak < SILENCE_PEAK
}

/** mm:ss */
export function formatElapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000))
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`
}
