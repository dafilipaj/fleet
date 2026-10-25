// Desktop layout constants + pure helpers (unit-tested in layout.test.ts).

export const SIDEBAR_MIN_W = 280
export const SIDEBAR_MAX_W = 560
export const SIDEBAR_DEFAULT_W = 360

/** A sidebar width within bounds; anything non-numeric is the default. */
export function clampSidebarWidth(w: number): number {
  if (!Number.isFinite(w)) return SIDEBAR_DEFAULT_W
  return Math.round(Math.max(SIDEBAR_MIN_W, Math.min(SIDEBAR_MAX_W, w)))
}

// Board view: the open session is a flyout over the board's right edge (the board never
// relayouts). Its width is a viewer pref; the bounds depend on the board's width.
export const FLYOUT_MIN_W = 420
/** Board left visible beside the widest flyout. */
export const FLYOUT_BOARD_GUTTER = 240
export const FLYOUT_DEFAULT_FRACTION = 0.55
export const FLYOUT_KEY_STEP = 32

/** The widest flyout over a board `container` px wide (never below the minimum, never past the board). */
export function flyoutMaxWidth(container: number): number {
  return Math.round(Math.min(container, Math.max(FLYOUT_MIN_W, container - FLYOUT_BOARD_GUTTER)))
}

/** The default flyout width: a fixed share of the board, within bounds. */
export function defaultFlyoutWidth(container: number): number {
  return clampFlyoutWidth(container * FLYOUT_DEFAULT_FRACTION, container)
}

/** A flyout width within bounds for this board; non-numeric or ≤ 0 (= "no preference") is the default. */
export function clampFlyoutWidth(w: number, container: number): number {
  // A 0-wide board (unmounted) makes the default share 0 too: bound it directly, no recursion.
  if (!Number.isFinite(w) || w <= 0) w = container * FLYOUT_DEFAULT_FRACTION
  const max = flyoutMaxWidth(container)
  return Math.round(Math.max(Math.min(FLYOUT_MIN_W, max), Math.min(max, w)))
}

/** Below this width (in rem: the details column is rem-sized too) a flyout lays its details column over the chat. */
export const FLYOUT_DETAILS_BESIDE_REM = 54

/** Does a `width` px pane leave room for the details column beside the chat? */
export function detailsFitBeside(width: number, remPx: number): boolean {
  return width >= FLYOUT_DETAILS_BESIDE_REM * (remPx > 0 ? remPx : 16)
}
