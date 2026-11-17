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

/**
 * Why a recording is not worth transcribing, or null. Sent anyway, silence comes back as text
 * the model makes up from the prompt (session names), which then goes out as a message.
 * `peak`: the loudest sample while recording; null when the level could not be measured.
 */
export function emptyRecordingReason({ durationMs, peak }: { durationMs: number; peak: number | null }): string | null {
  if (durationMs < MIN_RECORDING_MS) return 'The recording was too short.'
  if (peak !== null && peak < SILENCE_PEAK) return 'The microphone heard nothing. Check that it is not muted, then try again.'
  return null
}

/** The loudest sample in a block of time-domain samples (-1…1). */
export function peakOf(samples: ArrayLike<number>): number {
  let max = 0
  for (let i = 0; i < samples.length; i += 1) {
    const v = Math.abs(samples[i])
    if (v > max) max = v
  }
  return max
}

/** mm:ss */
export function formatElapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000))
  return `${Math.floor(s / 60)}:${String(s % 60).padStart(2, '0')}`
}
