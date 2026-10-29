import { useEffect } from 'react'
import { toast } from 'sonner'

import { copyWithToast } from '@/lib/clipboard'
import { APP_ERROR_EVENT, type AppError } from '@/lib/shortcuts'

/** Show Fleet.app's `fleet:error` events as error toasts; one with a command offers to copy it. */
export function useAppErrors() {
  useEffect(() => {
    const onError = (e: Event) => {
      const d = (e as CustomEvent<AppError>).detail
      if (!d?.title) return
      const command = d.command
      toast.error(d.title, {
        description: d.message,
        duration: 10_000,
        action: command ? { label: 'Copy command', onClick: () => void copyWithToast(command) } : undefined,
      })
    }
    window.addEventListener(APP_ERROR_EVENT, onError)
    return () => window.removeEventListener(APP_ERROR_EVENT, onError)
  }, [])
}
