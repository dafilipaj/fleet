// The mic's level while recording, for the recorder screen (a live meter, "hears nothing") and
// for holding back a silent take (lib/voice.ts emptyRecordingReason).
import { peakOf } from '@/lib/voice'

export interface VoiceMeter {
  /** The loudest sample so far, or null when the level could not be measured (no Web Audio, or the context never ran). */
  peak: () => number | null
  /** The loudest sample in the last block (0–1), for a live meter; 0 when unmeasured. */
  level: () => number
  close: () => void
}

export const NO_METER: VoiceMeter = { peak: () => null, level: () => 0, close: () => {} }

/** Create inside the tap, before any await: a context made later may stay suspended (iOS). */
export function openAudioContext(): AudioContext | null {
  try {
    const Ctx = window.AudioContext ?? (window as unknown as { webkitAudioContext?: typeof AudioContext }).webkitAudioContext
    return Ctx ? new Ctx() : null
  } catch {
    return null
  }
}

export function startMeter(ctx: AudioContext | null, media: MediaStream): VoiceMeter {
  if (!ctx) return NO_METER
  let max = 0
  let last = 0
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
      last = peakOf(buf)
      max = Math.max(max, last)
    }, 50)
  } catch {
    measured = false
  }
  return {
    peak: () => (measured ? max : null),
    level: () => last,
    close: () => {
      window.clearInterval(timer)
      void ctx.close().catch(() => {})
    },
  }
}
