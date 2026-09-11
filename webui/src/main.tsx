import { StrictMode } from 'react'
import { createRoot } from 'react-dom/client'
import { App } from './App'
import { DeviceProvider } from './store/DeviceProvider'
import './styles/base.css'

/**
 * The shell attaches itself to whatever page it is loaded into.
 *
 * In production that page is the media experience, which owns `#app`; the shell
 * creates its own container rather than expecting one, so the page it decorates
 * needs no markup of its own. A standalone dev page can still provide `#root`.
 */
const MOUNT_ID = 'mediabox-shell'

function mountPoint(): HTMLElement {
  const existing = document.getElementById(MOUNT_ID) ?? document.getElementById('root')
  if (existing) return existing
  const container = document.createElement('div')
  container.id = MOUNT_ID
  document.body.appendChild(container)
  return container
}

createRoot(mountPoint()).render(
  <StrictMode>
    <DeviceProvider>
      <App />
    </DeviceProvider>
  </StrictMode>,
)
