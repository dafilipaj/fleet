import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'

import App from '@/App'
import { installNewWindowClicks } from '@/lib/newWindow'
import { installNoPinchZoom, installViewportHeight } from '@/lib/viewport'
import './index.css'

installViewportHeight()
installNoPinchZoom()
// ⌘-click on anything that navigates opens a new window (see the router's `aroundNav`).
installNewWindowClicks()

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
