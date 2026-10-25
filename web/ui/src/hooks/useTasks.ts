import { createContext, useContext } from 'react'

import type { PullRequest, Session, Task, TasksResponse } from '@/api/types'
import type { TaskLinks } from '@/lib/tasks'

export interface TasksState {
  /** Whose Productive token the tasks come from (default: this UI's server). */
  host: string | null
  data: TasksResponse | null
  error: string | null
  /** The host has no Productive token / organization configured (the error says which). */
  unconfigured: boolean
  loading: boolean
  /** Fetch from Productive now. */
  refresh: () => Promise<void>
  refreshing: boolean
  /** Read again now (the CLI's cache — cheap). */
  reload: () => void
  /** Move a task to another status (id or name) on `host`, then read again. */
  setStatus: (t: Task, status: string) => Promise<void>
  links: TaskLinks
}

const EMPTY: TasksState = {
  host: null,
  data: null,
  error: null,
  unconfigured: false,
  loading: false,
  refresh: async () => {},
  refreshing: false,
  reload: () => {},
  setStatus: async () => {},
  links: { bySession: new Map(), byTask: new Map(), prsByTask: new Map(), byPr: new Map() },
}

export const TasksContext = createContext<TasksState>(EMPTY)

/** Your open Productive tasks (providers/TasksProvider.tsx). */
export const useTasks = () => useContext(TasksContext)

/** The task a session works on (its first prompt links it, or its PR does), if any. */
export function useSessionTask(s: Pick<Session, 'host' | 'session_id'>): Task | null {
  return useContext(TasksContext).links.bySession.get(`${s.host}/${s.session_id}`) ?? null
}

/** The tasks a PR links (known ones). */
export function usePrTasks(p: Pick<PullRequest, 'url'>): Task[] {
  return useContext(TasksContext).links.byPr.get(p.url) ?? []
}
