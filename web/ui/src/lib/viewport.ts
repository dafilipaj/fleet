/**
 * iOS Safari: 100dvh does not shrink for the software keyboard, visualViewport does.
 * Keep `--app-h` (used by the `h-app` / `min-h-app` utilities) in sync so a bottom
 * composer stays above the keyboard, and `--app-top` (the visual viewport's offset
 * into the layout viewport — iOS scrolls the page when an input focuses) so a fixed
 * full-screen layout (`fixed-app`) follows the visible area, and `--app-bottom` (how far the
 * visible area's bottom sits above the layout viewport's — the keyboard) so a bottom-anchored
 * fixed element (a drawer) sits on top of it. Call once at startup.
 */
export function installViewportHeight(): void {
  const sync = () => {
    const vv = window.visualViewport
    const height = vv ? vv.height : window.innerHeight
    const top = vv ? Math.max(0, vv.offsetTop) : 0
    const root = document.documentElement.style
    root.setProperty('--app-h', `${Math.round(height)}px`)
    root.setProperty('--app-top', `${Math.round(top)}px`)
    root.setProperty('--app-bottom', `${Math.max(0, Math.round(window.innerHeight - top - height))}px`)
  }
  window.visualViewport?.addEventListener('resize', sync)
  window.visualViewport?.addEventListener('scroll', sync)
  window.addEventListener('resize', sync)
  window.addEventListener('orientationchange', sync)
  sync()
}

/**
 * No pinch zoom on touch screens. iOS ignores `user-scalable=no` and only partly honours
 * `touch-action`, so also cancel WebKit's gesture events (the pinch itself). Touch devices only:
 * a Mac trackpad pinch fires the same events and desktop keeps its zoom.
 */
export function installNoPinchZoom(): void {
  if (!window.matchMedia?.('(pointer: coarse)').matches) return
  const cancel = (e: Event) => e.preventDefault()
  for (const type of ['gesturestart', 'gesturechange', 'gestureend']) {
    document.addEventListener(type, cancel, { passive: false })
  }
  // Two-finger touchmove: the pinch on browsers without gesture events.
  document.addEventListener(
    'touchmove',
    (e) => {
      if (e.touches.length > 1) e.preventDefault()
    },
    { passive: false },
  )
}
