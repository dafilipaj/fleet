import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'

import App from '@/App'
import { installNoPinchZoom, installViewportHeight } from '@/lib/viewport'
import './index.css'

installViewportHeight()
installNoPinchZoom()

createRoot(document.getElementById('root')!).render(
  <StrictMode>
    <App />
  </StrictMode>,
)
