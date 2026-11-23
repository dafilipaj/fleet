import { describe, expect, it } from 'vitest'

import { VOICE_BRIEF, VOICE_TAG, tagVoice, voiceModeKey } from './voiceMode'

describe('tagVoice', () => {
  it('tags a message once', () => {
    expect(tagVoice('  kako ide? ')).toBe(`${VOICE_TAG} kako ide?`)
    expect(tagVoice(`${VOICE_TAG} već tagirano`)).toBe(`${VOICE_TAG} već tagirano`)
  })
  it('leaves empty text empty', () => {
    expect(tagVoice('   ')).toBe('')
  })
  it('the brief is tagged itself', () => {
    expect(VOICE_BRIEF.startsWith(VOICE_TAG)).toBe(true)
  })
})

describe('voiceModeKey', () => {
  it('is per session', () => {
    expect(voiceModeKey('workstation', 'abc')).toBe('fleet.voiceMode.workstation/abc')
  })
})
