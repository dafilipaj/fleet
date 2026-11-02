// Static Tailwind class maps (tokens in index.css) — kept out of component files so
// Fast Refresh works and Tailwind sees every class name literally.
import type { StatusKey } from '@/lib/sessions'
import { hostColorSlot, type CtxLevel } from '@/lib/format'

export const STATUS_DOT: Record<StatusKey, string> = {
  waiting: 'bg-status-waiting ring-3 ring-status-waiting/20',
  busy: 'bg-status-busy',
  idle: 'bg-status-idle',
  unknown: 'bg-status-unknown',
}

export const STATUS_TEXT: Record<StatusKey, string> = {
  waiting: 'text-status-waiting',
  busy: 'text-status-busy',
  idle: 'text-status-idle',
  unknown: 'text-status-unknown',
}

// One stable colour per host name (slots --host-0..3).
const HOST_BADGE = [
  'text-host-0 border-host-0/30 bg-host-0/10',
  'text-host-1 border-host-1/30 bg-host-1/10',
  'text-host-2 border-host-2/30 bg-host-2/10',
  'text-host-3 border-host-3/30 bg-host-3/10',
]
const HOST_DOT = ['bg-host-0', 'bg-host-1', 'bg-host-2', 'bg-host-3']
const HOST_RING = ['border-host-0', 'border-host-1', 'border-host-2', 'border-host-3']

export const hostBadgeClass = (host: string | null | undefined) => HOST_BADGE[hostColorSlot(host)]
export const hostDotClass = (host: string | null | undefined) => HOST_DOT[hostColorSlot(host)]
export const hostRingClass = (host: string | null | undefined) => HOST_RING[hostColorSlot(host)]

// Context-window usage bands (ctxLevel): text + meter fill.
export const CTX_TEXT: Record<CtxLevel, string> = {
  low: 'text-dimmer',
  warn: 'text-status-waiting',
  hot: 'text-status-error',
}
export const CTX_FILL: Record<CtxLevel, string> = {
  low: 'bg-dimmer',
  warn: 'bg-status-waiting',
  hot: 'bg-status-error',
}
