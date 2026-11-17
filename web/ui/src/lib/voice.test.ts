import { expect, test } from 'vitest'

import { appendTranscript, emptyRecordingReason, formatElapsed, micUnavailableReason, peakOf, pickMimeType } from './voice'

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

test('emptyRecordingReason holds back slipped taps and silence, trusts an unmeasured level', () => {
  expect(emptyRecordingReason({ durationMs: 200, peak: 0.5 })).toMatch(/too short/)
  expect(emptyRecordingReason({ durationMs: 3000, peak: 0 })).toMatch(/heard nothing/)
  expect(emptyRecordingReason({ durationMs: 3000, peak: 0.004 })).toMatch(/heard nothing/)
  expect(emptyRecordingReason({ durationMs: 3000, peak: 0.2 })).toBeNull()
  expect(emptyRecordingReason({ durationMs: 3000, peak: null })).toBeNull()
})

test('peakOf is the loudest sample, either sign', () => {
  expect(peakOf(new Float32Array([0, 0.1, -0.3, 0.2]))).toBeCloseTo(0.3)
  expect(peakOf([])).toBe(0)
})
