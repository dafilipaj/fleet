import { afterEach, describe, expect, test, vi } from 'vitest'

import { hashRouteUrl, newWindowNav, takeNewWindow, wantsNewWindow } from './newWindow'

describe('wantsNewWindow', () => {
  test('⌘, Ctrl and the middle button ask for a new window', () => {
    expect(wantsNewWindow({ metaKey: true, ctrlKey: false, button: 0 })).toBe(true)
    expect(wantsNewWindow({ metaKey: false, ctrlKey: true, button: 0 })).toBe(true)
    expect(wantsNewWindow({ metaKey: false, ctrlKey: false, button: 1 })).toBe(true)
    expect(wantsNewWindow({ metaKey: false, ctrlKey: false, button: 0 })).toBe(false)
  })
})

describe('hashRouteUrl', () => {
  const base = 'http://127.0.0.1:7777/#/s/ws/abc'
  test('puts the route in the hash of this page', () => {
    expect(hashRouteUrl('/review/ws/abc', base)).toBe('http://127.0.0.1:7777/#/review/ws/abc')
    expect(hashRouteUrl('review/ws/abc', base)).toBe('http://127.0.0.1:7777/#/review/ws/abc')
  })
  test('a query goes to the search part', () => {
    expect(hashRouteUrl('/prs?x=1', base)).toBe('http://127.0.0.1:7777/?x=1#/prs')
  })
})

describe('newWindowNav', () => {
  afterEach(() => vi.unstubAllGlobals())

  test('a plain navigation stays in the window', () => {
    const navigate = vi.fn()
    newWindowNav(navigate, '/prs', { replace: true })
    expect(navigate).toHaveBeenCalledWith('/prs', { replace: true })
  })

  test('the first navigation after a ⌘-click opens a window, the next one stays', () => {
    const open = vi.fn()
    vi.stubGlobal('window', { open })
    vi.stubGlobal('location', { href: 'http://127.0.0.1:7777/#/' })
    const listeners: Record<string, (e: MouseEvent) => void> = {}
    vi.stubGlobal('addEventListener', (t: string, f: (e: MouseEvent) => void) => (listeners[t] = f))
    return import('./newWindow').then(({ installNewWindowClicks }) => {
      installNewWindowClicks()
      listeners.click({ metaKey: true, ctrlKey: false, button: 0 } as MouseEvent)
      const navigate = vi.fn()
      newWindowNav(navigate, '/review/ws/abc')
      expect(open).toHaveBeenCalledWith('http://127.0.0.1:7777/#/review/ws/abc', '_blank')
      expect(navigate).not.toHaveBeenCalled()
      newWindowNav(navigate, '/s/ws/abc')
      expect(navigate).toHaveBeenCalledWith('/s/ws/abc', undefined)
      expect(takeNewWindow()).toBe(false)
    })
  })
})
