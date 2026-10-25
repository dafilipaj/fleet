import { createContext, useContext } from 'react'

import type { PrsResponse, PullRequest, Session } from '@/api/types'
import type { PrLinks } from '@/lib/prs'

export interface PrsState {
  /** Whose `gh` login and checkouts the PRs come from (default: this UI's server). */
  host: string | null
  data: PrsResponse | null
  error: string | null
  loading: boolean
  /** Fetch from GitHub now. */
  refresh: () => Promise<void>
  refreshing: boolean
  /** Read again now (the CLI's cache — cheap; re-joins the local worktrees). */
  reload: () => void
  /** Its @mentions of you so far are seen (on `host`), then read again. */
  markSeen: (p: PullRequest) => Promise<void>
  links: PrLinks
}

const EMPTY: PrsState = {
  host: null,
  data: null,
  error: null,
  loading: false,
  refresh: async () => {},
  refreshing: false,
  reload: () => {},
  markSeen: async () => {},
  links: { bySession: new Map(), byPr: new Map() },
}

export const PrsContext = createContext<PrsState>(EMPTY)

/** The PRs that need you (providers/PrsProvider.tsx). */
export const usePrs = () => useContext(PrsContext)

/** The PR a session works on (its cwd is that PR's worktree), if any. */
export function useSessionPr(s: Pick<Session, 'host' | 'session_id'>): PullRequest | null {
  return useContext(PrsContext).links.bySession.get(`${s.host}/${s.session_id}`) ?? null
}
