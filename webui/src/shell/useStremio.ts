import { useCallback, useEffect, useRef, useState } from 'react'
import {
  castToDevice,
  ensureStreamingServer,
  isPlayerRoute,
  pausePreview,
  readPreview,
  streamingServerUrl,
  waitForCore,
  type CoreTransport,
  type PreviewState,
} from '../stremio/core'

export type CoreStatus = 'waiting' | 'ready' | 'absent'

export interface StremioBridgeState {
  status: CoreStatus
  core: CoreTransport | null
  /** Whether MediaBox had to point Stremio at its own streaming-server mount. */
  serverUrlApplied: boolean
  /** True while Stremio is showing its player, i.e. a preview is on screen. */
  onPlayer: boolean
  preview: PreviewState | null
}

const PREVIEW_POLL_MS = 500

/**
 * Binds the appliance shell to the Stremio app sharing this document.
 *
 * Nothing here reaches into Stremio's internals: it waits for the transport
 * Stremio itself publishes, then uses only documented core actions.
 */
export function useStremio(mount = '/'): StremioBridgeState & {
  castToKodi: (deviceId: string) => Promise<PreviewState>
} {
  const [status, setStatus] = useState<CoreStatus>('waiting')
  const [serverUrlApplied, setServerUrlApplied] = useState(false)
  const [onPlayer, setOnPlayer] = useState(() => isPlayerRoute())
  const [preview, setPreview] = useState<PreviewState | null>(null)
  const coreRef = useRef<CoreTransport | null>(null)

  useEffect(() => {
    let cancelled = false
    void (async () => {
      const core = await waitForCore()
      if (cancelled) return
      if (!core) {
        setStatus('absent')
        return
      }
      coreRef.current = core
      setStatus('ready')
      try {
        const outcome = await ensureStreamingServer(core, streamingServerUrl(location.origin, mount))
        if (!cancelled) setServerUrlApplied(outcome === 'adopted')
      } catch {
        // A refused settings update must not take the shell down with it; the
        // user can still set the server URL in Stremio's own settings screen.
      }
    })()
    return () => {
      cancelled = true
    }
  }, [mount])

  useEffect(() => {
    const onHashChange = () => setOnPlayer(isPlayerRoute())
    window.addEventListener('hashchange', onHashChange)
    return () => window.removeEventListener('hashchange', onHashChange)
  }, [])

  // The playhead only exists on the media element, so it is sampled rather than
  // subscribed to. This runs only while a preview is actually on screen.
  useEffect(() => {
    if (!onPlayer || status !== 'ready') {
      setPreview(null)
      return
    }
    let cancelled = false
    const sample = async () => {
      const core = coreRef.current
      if (!core) return
      const next = await readPreview(core)
      if (!cancelled) setPreview(next)
    }
    void sample()
    const timer = setInterval(() => void sample(), PREVIEW_POLL_MS)
    return () => {
      cancelled = true
      clearInterval(timer)
    }
  }, [onPlayer, status])

  const castToKodi = useCallback(async (deviceId: string): Promise<PreviewState> => {
    const core = coreRef.current
    if (!core) throw new Error('Stremio çekirdeği hazır değil')
    const current = await readPreview(core)
    if (!current.source) throw new Error('Oynatılan bir kaynak bulunamadı')
    // One torrent engine must not feed the preview and Kodi at two different
    // positions, so the preview stops before the handoff is dispatched.
    pausePreview()
    await castToDevice(core, deviceId, current.source, current.timeMs)
    return current
  }, [])

  return { status, core: coreRef.current, serverUrlApplied, onPlayer, preview, castToKodi }
}
