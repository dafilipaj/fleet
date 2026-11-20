import type { AroundNavHandler } from 'wouter'

/**
 * ⌘-click (Ctrl-click, middle click) on anything that navigates opens the target in a new window
 * instead: a browser tab, or a second Fleet.app window (the desktop shell keeps the server's own
 * pages in the app). Covers `<Link>`s (the browser opens their href itself) and buttons and rows
 * whose click handler calls `navigate`, which the router routes through `aroundNav` below.
 *
 * A capture listener marks a modified click; the first navigation in the same task consumes the
 * mark, so a navigation that follows later (after a fetch) or a redirect it causes stays put.
 */

let pending = false

/** Does this click ask for a new window? */
export const wantsNewWindow = (e: Pick<MouseEvent, 'metaKey' | 'ctrlKey' | 'button'>) =>
  e.metaKey || e.ctrlKey || e.button === 1

/** Take the mark of a modified click in this task, if any (true once). */
export const takeNewWindow = () => {
  const p = pending
  pending = false
  return p
}

const mark = (e: MouseEvent) => {
  if (!wantsNewWindow(e)) return
  pending = true
  setTimeout(() => (pending = false))
}

/** Listen for modified clicks (once, at startup). */
export const installNewWindowClicks = () => {
  addEventListener('click', mark, true)
  addEventListener('auxclick', mark, true)
}

/** The URL of hash route `to` (`/review/…`) on this page. */
export const hashRouteUrl = (to: string, base = location.href) => {
  const url = new URL(base)
  const [hash, search] = to.replace(/^#?\/?/, '').split('?')
  url.hash = `/${hash}`
  if (search !== undefined) url.search = search
  return url.href
}

/** The router's `aroundNav`: a navigation from a modified click opens a new window. */
export const newWindowNav: AroundNavHandler = (navigate, to, opts) => {
  if (takeNewWindow()) {
    window.open(hashRouteUrl(String(to)), '_blank')
    return
  }
  navigate(to, opts)
}
