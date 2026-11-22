import { api } from '@/api/client'
import type { GithubStatus } from '@/api/types'
import { createLinkStatusStore } from '@/hooks/linkStatus'

/** GitHub links (/api/github), polled while anything shows them. */
const store = createLinkStatusStore<GithubStatus>(() => api.github(), { enabled: false, host: null, links: [], ended: [] })

export const refreshGithub = store.refresh
export const useGithub = store.useStatus
