import { expect, test } from 'vitest'

import { CHAT_FONT_REM, TEXT_SIZES, parseTextSize, rootFontSize } from './prefs'

test('rootFontSize scales the root so the chat lands on the chosen px', () => {
  expect(rootFontSize(15)).toBe('')
  expect(rootFontSize(13)).toBe('86.667%')
  expect(rootFontSize(17)).toBe('113.333%')
  expect(rootFontSize(16)).toBe('')
  for (const size of TEXT_SIZES) {
    const root = 16 * (rootFontSize(size) ? parseFloat(rootFontSize(size)) / 100 : 1)
    expect(root * parseFloat(CHAT_FONT_REM)).toBeCloseTo(size, 2)
  }
})

test('parseTextSize', () => {
  expect(parseTextSize('17')).toBe(17)
  expect(parseTextSize('16')).toBeUndefined()
})
