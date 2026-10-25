import { useLayoutEffect, useRef, type RefObject } from 'react'

const DURATION = 280
const EASING = 'cubic-bezier(0.2, 0.8, 0.2, 1)'

/**
 * FLIP-animate the `[data-flip]` children of `container` when `order` (any string that
 * changes with their order) changes: each row glides from where it was to where it is now,
 * a new row fades in. Positions are read in the container's scroll space, so scrolling
 * never animates. Nothing moves under `prefers-reduced-motion`.
 */
export function useFlip(container: RefObject<HTMLElement | null>, order: string) {
  const last = useRef(new Map<string, number>())
  const lastOrder = useRef<string | null>(null)

  useLayoutEffect(() => {
    const root = container.current
    if (!root) return
    const base = root.getBoundingClientRect().top - root.scrollTop
    const now = new Map<string, number>()
    const els = root.querySelectorAll<HTMLElement>('[data-flip]')
    for (const el of els) now.set(el.dataset.flip!, el.getBoundingClientRect().top - base)

    const reorder = lastOrder.current !== null && lastOrder.current !== order
    const still = window.matchMedia?.('(prefers-reduced-motion: reduce)').matches
    if (reorder && !still) {
      for (const el of els) {
        const from = last.current.get(el.dataset.flip!)
        const to = now.get(el.dataset.flip!)!
        if (from == null) el.animate([{ opacity: 0 }, { opacity: 1 }], { duration: DURATION, easing: EASING })
        else if (Math.abs(from - to) > 1)
          el.animate([{ transform: `translateY(${from - to}px)` }, { transform: 'none' }], { duration: DURATION, easing: EASING })
      }
    }
    last.current = now
    lastOrder.current = order
  })
}
