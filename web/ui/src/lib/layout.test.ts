import { expect, test } from 'vitest'

import {
  FLYOUT_BOARD_GUTTER,
  FLYOUT_DEFAULT_FRACTION,
  FLYOUT_MIN_W,
  SIDEBAR_DEFAULT_W,
  SIDEBAR_MAX_W,
  SIDEBAR_MIN_W,
  clampFlyoutWidth,
  clampSidebarWidth,
  defaultFlyoutWidth,
  clampDetailsWidth,
  DETAILS_CHAT_MIN_W,
  DETAILS_DEFAULT_W,
  DETAILS_MIN_W,
  detailsWidth,
  flyoutMaxWidth,
} from '@/lib/layout'

test('clampSidebarWidth keeps the sidebar within bounds', () => {
  expect(clampSidebarWidth(400)).toBe(400)
  expect(clampSidebarWidth(401.6)).toBe(402)
  expect(clampSidebarWidth(10)).toBe(SIDEBAR_MIN_W)
  expect(clampSidebarWidth(5000)).toBe(SIDEBAR_MAX_W)
  expect(clampSidebarWidth(Number('abc'))).toBe(SIDEBAR_DEFAULT_W)
  expect(clampSidebarWidth(Number(''))).toBe(SIDEBAR_MIN_W) // Number('') is 0
})

test('flyout bounds follow the board width', () => {
  expect(flyoutMaxWidth(1440)).toBe(1440 - FLYOUT_BOARD_GUTTER)
  expect(flyoutMaxWidth(600)).toBe(FLYOUT_MIN_W) // a narrow board: the minimum wins over the gutter
  expect(flyoutMaxWidth(300)).toBe(300) // never wider than the board itself
})

test('clampFlyoutWidth survives a 0-wide board (no recursion)', () => {
  expect(clampFlyoutWidth(0, 0)).toBe(0)
  expect(defaultFlyoutWidth(0)).toBe(0)
})

test('clampFlyoutWidth keeps the flyout within bounds', () => {
  expect(clampFlyoutWidth(700, 1440)).toBe(700)
  expect(clampFlyoutWidth(700.4, 1440)).toBe(700)
  expect(clampFlyoutWidth(100, 1440)).toBe(FLYOUT_MIN_W)
  expect(clampFlyoutWidth(5000, 1440)).toBe(1440 - FLYOUT_BOARD_GUTTER)
  expect(clampFlyoutWidth(900, 1024)).toBe(1024 - FLYOUT_BOARD_GUTTER) // a stored width shrinks with the window
  expect(clampFlyoutWidth(700, 300)).toBe(300)
})

test('no stored preference → the default share of the board', () => {
  expect(defaultFlyoutWidth(1440)).toBe(Math.round(1440 * FLYOUT_DEFAULT_FRACTION))
  expect(clampFlyoutWidth(0, 1440)).toBe(defaultFlyoutWidth(1440))
  expect(clampFlyoutWidth(Number('abc'), 1440)).toBe(defaultFlyoutWidth(1440))
  expect(defaultFlyoutWidth(1024)).toBe(Math.round(1024 * FLYOUT_DEFAULT_FRACTION))
  expect(defaultFlyoutWidth(700)).toBe(FLYOUT_MIN_W)
})

test('detailsWidth keeps the chat its room, else no column', () => {
  expect(clampDetailsWidth(100)).toBe(DETAILS_MIN_W)
  expect(clampDetailsWidth(NaN)).toBe(DETAILS_DEFAULT_W)
  expect(detailsWidth(400, 1200)).toBe(400)
  expect(detailsWidth(400, 700)).toBe(700 - DETAILS_CHAT_MIN_W)
  expect(detailsWidth(400, DETAILS_CHAT_MIN_W + DETAILS_MIN_W - 1)).toBe(0)
})
