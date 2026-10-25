import { createContext, useContext } from 'react'

import { DEFAULT_SEND_DELAY } from '@/lib/outbox'
import { DEFAULT_TEXT_SIZE } from '@/lib/prefs'

/** Global viewer preferences (the Settings screen), persisted per viewer in localStorage. */
export interface PrefsState {
  /** One of TEXT_SIZES: scales the whole UI (root font-size). */
  textSize: number
  setTextSize: (n: number) => void
  /** Hide assistant narration between tool calls in the chat. */
  hideNotes: boolean
  setHideNotes: (v: boolean) => void
  /** Composer undo window in ms (0 = send at once), one of SEND_DELAYS. */
  sendDelay: number
  setSendDelay: (ms: number) => void
}

export const PrefsContext = createContext<PrefsState>({
  textSize: DEFAULT_TEXT_SIZE,
  setTextSize: () => {},
  hideNotes: false,
  setHideNotes: () => {},
  sendDelay: DEFAULT_SEND_DELAY,
  setSendDelay: () => {},
})

export function usePrefs(): PrefsState {
  return useContext(PrefsContext)
}
