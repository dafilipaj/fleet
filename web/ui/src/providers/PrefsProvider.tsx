import { useEffect, useMemo, type ReactNode } from 'react'

import { PrefsContext } from '@/hooks/usePrefs'
import { usePersistentState } from '@/hooks/usePersistentState'
import {
  DEFAULT_TEXT_SIZE,
  HIDE_NOTES_KEY,
  SEND_DELAY_KEY,
  TEXT_SIZE_KEY,
  parseTextSize,
  rootFontSize,
} from '@/lib/prefs'
import { DEFAULT_SEND_DELAY, parseSendDelay } from '@/lib/outbox'

const parseBool01 = (raw: string) => (raw === '1' || raw === '0' ? raw : undefined)

/**
 * Text size, progress notes and the send delay for the whole app. The text size sets the
 * root font-size on <html> (index.html applies the same rule before first paint), so every rem
 * scales with it.
 */
export function PrefsProvider({ children }: { children: ReactNode }) {
  const [textSize, setTextSize] = usePersistentState<number>(TEXT_SIZE_KEY, DEFAULT_TEXT_SIZE, parseTextSize)
  const [hideNotesRaw, setHideNotesRaw] = usePersistentState<string>(HIDE_NOTES_KEY, '0', parseBool01)
  const [sendDelay, setSendDelay] = usePersistentState<number>(SEND_DELAY_KEY, DEFAULT_SEND_DELAY, parseSendDelay)

  useEffect(() => {
    const root = document.documentElement
    root.style.fontSize = rootFontSize(textSize)
    root.dataset.textSize = String(textSize)
  }, [textSize])

  const value = useMemo(
    () => ({
      textSize,
      setTextSize,
      hideNotes: hideNotesRaw === '1',
      setHideNotes: (v: boolean) => setHideNotesRaw(v ? '1' : '0'),
      sendDelay,
      setSendDelay,
    }),
    [textSize, setTextSize, hideNotesRaw, setHideNotesRaw, sendDelay, setSendDelay],
  )
  return <PrefsContext.Provider value={value}>{children}</PrefsContext.Provider>
}
