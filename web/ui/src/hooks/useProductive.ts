import { api } from '@/api/client'
import type { ProductiveStatus } from '@/api/types'
import { createLinkStatusStore } from '@/hooks/linkStatus'

/** Productive links (/api/productive), polled while anything shows them. */
const store = createLinkStatusStore<ProductiveStatus>(() => api.productive(), { enabled: false, host: null, links: [], ended: [] })

export const refreshProductive = store.refresh
export const useProductive = store.useStatus
