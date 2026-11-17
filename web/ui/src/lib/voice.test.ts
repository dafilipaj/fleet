import { expect, test } from 'vitest'

import { STALE_MS, appendTranscript, emptyRecordingReason, formatElapsed, hearsNothing, meterHeardNothing, micUnavailableReason, peakOf, pickMimeType, staleTakes } from './voice'

test('appendTranscript joins with exactly one space and ignores empty transcripts', () => {
  expect(appendTranscript('', 'Pozdrav.')).toBe('Pozdrav.')
  expect(appendTranscript('fix the bug', 'in fleet')).toBe('fix the bug in fleet')
  expect(appendTranscript('line one\n', 'two')).toBe('line one\ntwo')
  expect(appendTranscript('keep ', '  padded  ')).toBe('keep padded')
  expect(appendTranscript('unchanged', '   ')).toBe('unchanged')
})

test('pickMimeType prefers webm/opus, falls back to mp4, else the default', () => {
  expect(pickMimeType(() => true)).toBe('audio/webm;codecs=opus')
  expect(pickMimeType((t) => t === 'audio/mp4')).toBe('audio/mp4')
  expect(pickMimeType(() => false)).toBe('')
})

test('micUnavailableReason explains insecure origins first', () => {
  expect(micUnavailableReason({ secure: true, getUserMedia: true, recorder: true })).toBeNull()
  expect(micUnavailableReason({ secure: false, getUserMedia: false, recorder: true })).toMatch(/HTTPS/)
  expect(micUnavailableReason({ secure: true, getUserMedia: true, recorder: false })).toMatch(/cannot record/)
})

test('formatElapsed is m:ss', () => {
  expect(formatElapsed(0)).toBe('0:00')
  expect(formatElapsed(9_999)).toBe('0:09')
  expect(formatElapsed(75_000)).toBe('1:15')
})

test('emptyRecordingReason holds back only slipped taps, never a silent-looking take', () => {
  expect(emptyRecordingReason({ durationMs: 200 })).toMatch(/too short/)
  expect(emptyRecordingReason({ durationMs: 3000 })).toBeNull()
})

test('meterHeardNothing: a measured level under the silence floor; an unmeasured one is trusted', () => {
  expect(meterHeardNothing(0)).toBe(true)
  expect(meterHeardNothing(0.004)).toBe(true)
  expect(meterHeardNothing(0.2)).toBe(false)
  expect(meterHeardNothing(null)).toBe(false)
})

test('peakOf is the loudest sample, either sign', () => {
  expect(peakOf(new Float32Array([0, 0.1, -0.3, 0.2]))).toBeCloseTo(0.3)
  expect(peakOf([])).toBe(0)
})

test('staleTakes offers takes no tab is recording, oldest first, never the active one', () => {
  const now = 1_000_000
  const old = { id: 'a', startedAt: 2, heartbeat: now - STALE_MS - 1 }
  const older = { id: 'b', startedAt: 1, heartbeat: now - STALE_MS - 5000 }
  const live = { id: 'c', startedAt: 3, heartbeat: now - 1000 }
  expect(staleTakes([old, live, older], now, null).map((t) => t.id)).toEqual(['b', 'a'])
  expect(staleTakes([old, older], now, 'a').map((t) => t.id)).toEqual(['b'])
})

test('hearsNothing warns only after a while of measured silence', () => {
  expect(hearsNothing({ elapsedMs: 1000, peak: 0 })).toBe(false)
  expect(hearsNothing({ elapsedMs: 3000, peak: 0 })).toBe(true)
  expect(hearsNothing({ elapsedMs: 3000, peak: 0.2 })).toBe(false)
  expect(hearsNothing({ elapsedMs: 3000, peak: null })).toBe(false)
})
