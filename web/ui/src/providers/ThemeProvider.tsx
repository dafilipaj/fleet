import { useEffect, useMemo, useState, useSyncExternalStore, type ReactNode } from 'react'

import { THEME_KEY, ThemeContext, resolveTheme, type ResolvedTheme, type ThemeChoice } from '@/hooks/useTheme'
import { BASE_PALETTE, DEFAULT_PALETTE, PALETTE_KEY, parsePalette, themeColor, type PaletteId } from '@/lib/palettes'
import { storage } from '@/lib/storage'

const LIGHT_QUERY = '(prefers-color-scheme: light)'

function subscribe(cb: () => void) {
  const mq = window.matchMedia(LIGHT_QUERY)
  mq.addEventListener('change', cb)
  return () => mq.removeEventListener('change', cb)
}
const prefersLight = () => window.matchMedia(LIGHT_QUERY).matches

function readChoice(): ThemeChoice {
  const v = storage.get(THEME_KEY)
  return v === 'dark' || v === 'light' ? v : 'system'
}

/**
 * Toggles the `dark` class on <html> (shadcn's convention) and sets `data-palette` (none for
 * Graphite, the base tokens). index.html sets both before first paint with the same rules, so there is no flash.
 */
export function ThemeProvider({ children, forced }: { children: ReactNode; forced?: ResolvedTheme | null }) {
  const [theme, setThemeState] = useState<ThemeChoice>(readChoice)
  const light = useSyncExternalStore(subscribe, prefersLight, () => false)
  // `forced` (the embed view's `theme`): the framing app's mode, without touching the viewer's choice.
  const resolved = forced ?? resolveTheme(theme, light)
  const [palette, setPaletteState] = useState<PaletteId>(() => parsePalette(storage.get(PALETTE_KEY)))

  useEffect(() => {
    const root = document.documentElement
    root.classList.toggle('dark', resolved === 'dark')
    root.style.colorScheme = resolved
    if (palette === BASE_PALETTE) delete root.dataset.palette
    else root.dataset.palette = palette
    // The PWA status bar / overscroll colour: the palette's --background.
    document.querySelector('meta[name="theme-color"]')?.setAttribute('content', themeColor(palette, resolved))
  }, [resolved, palette])

  const value = useMemo(
    () => ({
      theme,
      resolved,
      setTheme: (t: ThemeChoice) => {
        setThemeState(t)
        if (t === 'system') storage.remove(THEME_KEY)
        else storage.set(THEME_KEY, t)
      },
      palette,
      setPalette: (p: PaletteId) => {
        setPaletteState(p)
        if (p === DEFAULT_PALETTE) storage.remove(PALETTE_KEY)
        else storage.set(PALETTE_KEY, p)
      },
    }),
    [theme, resolved, palette],
  )
  return <ThemeContext.Provider value={value}>{children}</ThemeContext.Provider>
}
