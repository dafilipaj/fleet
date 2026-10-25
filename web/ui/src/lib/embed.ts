// The embed view (`#/embed?host=<h>&dir=<d>[&theme=dark|light]`): Fleet's session chat for one
// directory, framed by another app (an iframe panel). Pure helpers; screens/EmbedScreen.tsx is the view.

import type { FleetResponse, Host, Session } from '@/api/types'
import { allSessions, byLastActivity } from '@/lib/sessions'

export type EmbedTheme = 'dark' | 'light'

export interface EmbedParams {
  host: string
  /** As given: absolute or `~/…` on `host`. */
  dir: string
  /** null: follow the OS (and never touch the viewer's Fleet theme setting). */
  theme: EmbedTheme | null
  /** The framing app's label (`app`, else the dir's last folder): put on the sessions it spawns (`embed`). */
  app: string
}

const APP_RE = /^[A-Za-z0-9_-]{1,40}$/

/** The dir's last folder as an app label (`~/Code/fin` → `fin`); `app` when it has no usable name. */
function defaultApp(dir: string): string {
  const last = (dir.replace(/\/+$/, '').split('/').pop() ?? '').replace(/[^A-Za-z0-9_-]/g, '').slice(0, 40)
  return last || 'app'
}

/** The params from a query string (`host=…&dir=…&theme=…`); null without host / dir. */
function fromQuery(query: string): EmbedParams | null {
  const q = new URLSearchParams(query)
  const host = q.get('host')?.trim() ?? ''
  const dir = q.get('dir')?.trim() ?? ''
  if (!host || !dir) return null
  const t = q.get('theme')
  const app = q.get('app')?.trim() ?? ''
  return { host, dir, theme: t === 'dark' || t === 'light' ? t : null, app: APP_RE.test(app) ? app : defaultApp(dir) }
}

/** `#/embed?host=…&dir=…&theme=…` → its params; null for any other route or without host / dir. */
export function parseEmbedHash(hash: string): EmbedParams | null {
  const m = /^#?\/embed\/?(?:\?(.*))?$/.exec(hash)
  return m ? fromQuery(m[1] ?? '') : null
}

export function embedHash(p: EmbedParams): string {
  const q = new URLSearchParams({ host: p.host, dir: p.dir, app: p.app })
  if (p.theme) q.set('theme', p.theme)
  return `/embed?${q}`
}

/**
 * The embed params this page was opened with: `#/embed?…`, or the page's own query string (the
 * hash router moves a route's query there — `/?host=…&dir=…#/embed` — and keeps it while the frame
 * moves on to `#/s/…`). Fleet's own pages never have one.
 */
export function readEmbed(loc: Pick<Location, 'hash' | 'search'> = window.location): EmbedParams | null {
  return parseEmbedHash(loc.hash) ?? fromQuery(loc.search.replace(/^\?/, ''))
}

const trim = (p: string) => (p.length > 1 ? p.replace(/\/+$/, '') : p)

/** `root` itself or a path beneath it (a worktree inside the repo, a subdirectory). */
export function isInside(path: string | null | undefined, root: string): boolean {
  if (!path) return false
  const p = trim(path)
  const r = trim(root)
  return p === r || p.startsWith(r === '/' ? '/' : `${r}/`)
}

export type ResolvedDir = { ok: true; path: string } | { ok: false; error: string }

/**
 * `dir` as an absolute path on `host`, allowed only when it is one of the host's spawnDirs (offered
 * or hidden) or beneath one (the server checks it again on spawn). `~` needs the host's `home`; a server too
 * old to send it still matches a spawn dir that ends with the same path.
 */
export function resolveEmbedDir(dir: string, host: Pick<Host, 'home' | 'spawnDirs' | 'hiddenSpawnDirs'> | null | undefined): ResolvedDir {
  const roots = [...(host?.spawnDirs ?? []), ...(host?.hiddenSpawnDirs ?? [])].map((d) => trim(d.path))
  let abs: string | null = null
  if (dir.startsWith('/')) abs = trim(dir)
  else if (dir === '~' || dir.startsWith('~/')) {
    const rest = dir.slice(1)
    if (host?.home) abs = trim(host.home + rest)
    else abs = roots.find((r) => rest.length > 1 && r.endsWith(trim(rest))) ?? null
  } else return { ok: false, error: `dir must be absolute or start with ~/: ${dir}` }
  if (!abs) return { ok: false, error: `cannot resolve ${dir} on this host` }
  if (abs.split('/').includes('..')) return { ok: false, error: `dir must not contain ..: ${dir}` }
  if (!roots.some((r) => isInside(abs, r))) return { ok: false, error: `${dir} is not one of this host's start directories (or inside one)` }
  return { ok: true, path: abs }
}

/** The host's sessions working in `root` (or beneath it), most recently active first. */
export function sessionsIn(fleet: FleetResponse | null | undefined, host: string, root: string): Session[] {
  return allSessions(fleet)
    .filter((s) => s.host === host && isInside(s.cwd, root))
    .sort(byLastActivity)
}
