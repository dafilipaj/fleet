import { createContext, useContext, useLayoutEffect, useMemo, useRef, useState } from 'react'

export type VoiceState = 'idle' | 'recording' | 'transcribing'

/** Where a take's text goes: the field (or the operator) whose mic opened the recorder. */
export interface VoiceTarget {
  /** Words the transcriber should expect (session name, repo). */
  hint: string
  /** Shown on the recorder screen and when an unsent take is offered again ("Operator"). */
  label: string
  onText: (text: string) => void
  /** Hold the transcript CONFIRM_MS, cancellable, before `onText` (the operator: it sends at once). */
  confirm?: boolean
}

export interface VoiceOwner {
  id: string
  target: { current: VoiceTarget }
}

/** The app's one recorder (providers/VoiceProvider.tsx), shared by every mic button. */
export interface VoiceRecorder {
  /** Which owner the recorder works for, null when it is closed. */
  ownerId: string | null
  state: VoiceState
  startedAt: number | null
  start: (owner: VoiceOwner) => void
  stop: () => void
  cancel: () => void
  /** Mounted owners: a take offered again goes to the newest one. */
  register: (owner: VoiceOwner) => () => void
}

export const VoiceContext = createContext<VoiceRecorder>({
  ownerId: null,
  state: 'idle',
  startedAt: null,
  start: () => {},
  stop: () => {},
  cancel: () => {},
  register: () => () => {},
})

let seq = 0

/**
 * A mic button's handle on the shared recorder: `start` opens the full-screen recorder, the
 * transcript goes to `onText` (after a cancellable pause with `confirm`). The state is this
 * button's: 'idle' while the recorder works for another one.
 */
export function useVoice({ onText, hint, label = 'Voice', confirm = false }: { onText: (text: string) => void; hint: string; label?: string; confirm?: boolean }) {
  const recorder = useContext(VoiceContext)
  const target = useRef<VoiceTarget>({ onText, hint, label, confirm })
  const [owner] = useState<VoiceOwner>(() => ({ id: `voice-${(seq += 1)}`, target }))
  const ref = useRef(recorder)
  useLayoutEffect(() => {
    target.current = { onText, hint, label, confirm }
    ref.current = recorder
  })
  const { register } = recorder
  useLayoutEffect(() => register(owner), [register, owner])
  const mine = recorder.ownerId === owner.id
  return useMemo(
    () => ({
      state: mine ? recorder.state : ('idle' as VoiceState),
      startedAt: mine ? recorder.startedAt : null,
      start: () => ref.current.start(owner),
      stop: () => ref.current.stop(),
      cancel: () => ref.current.cancel(),
    }),
    [mine, recorder.state, recorder.startedAt, owner],
  )
}
