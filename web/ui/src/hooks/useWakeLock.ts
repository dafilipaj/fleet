import { useEffect } from 'react'

type Lock = { release: () => Promise<void> }
type WakeLockApi = { request: (t: 'screen') => Promise<Lock> }

/**
 * Keep the screen on while `enabled` (voice mode: a phone in a car holder must not lock, or
 * polling and spoken replies stop). The browser drops the lock when the page hides; it is taken
 * again when the page shows.
 */
export function useWakeLock(enabled: boolean) {
  useEffect(() => {
    const wl = (navigator as Navigator & { wakeLock?: WakeLockApi }).wakeLock
    if (!enabled || !wl) return
    let lock: Lock | null = null
    let live = true
    const take = () => {
      if (document.visibilityState !== 'visible') return
      wl.request('screen').then(
        (l) => {
          if (live) lock = l
          else void l.release().catch(() => {})
        },
        () => {},
      )
    }
    take()
    document.addEventListener('visibilitychange', take)
    return () => {
      live = false
      document.removeEventListener('visibilitychange', take)
      void lock?.release().catch(() => {})
    }
  }, [enabled])
}
