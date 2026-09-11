/**
 * The appliance's side of the conversation with the Stremio web app.
 *
 * The media experience is upstream stremio-web, running unmodified in this same
 * document. It publishes its core transport on `window.core`, which is the only
 * thing this module uses — no patched bundle, no private internals, no iframe.
 *
 * Three jobs:
 *  1. point Stremio at the MediaBox streaming-server mount on this origin;
 *  2. read what is playing in the preview, and where it has got to;
 *  3. hand that stream to Kodi through Stremio's own cast action.
 */

export interface CoreTransport {
  getState(model: string): Promise<unknown>
  dispatch(action: unknown, model?: string): Promise<void>
}

declare global {
  interface Window {
    core?: CoreTransport | null
  }
}

/** Defaults stremio-core ships with; only these are silently replaced. */
const DEFAULT_SERVER_URLS = ['http://127.0.0.1:11470/', 'http://localhost:11470/']

export function streamingServerUrl(origin = location.origin, mount = '/'): string {
  return new URL(mount, origin).toString()
}

function sameUrl(a: string | undefined, b: string): boolean {
  if (!a) return false
  try {
    return new URL(a).href === new URL(b).href
  } catch {
    return false
  }
}

export function shouldAdoptServerUrl(current: string | undefined, desired: string): boolean {
  if (sameUrl(current, desired)) return false
  // A URL the user deliberately chose in Stremio's own settings is left alone;
  // only an untouched default is replaced.
  if (!current) return true
  return DEFAULT_SERVER_URLS.some((value) => sameUrl(current, value))
}

export async function waitForCore(timeoutMs = 60_000, step = 100): Promise<CoreTransport | null> {
  const deadline = Date.now() + timeoutMs
  while (Date.now() < deadline) {
    if (window.core) return window.core
    await new Promise((resolve) => setTimeout(resolve, step))
  }
  return null
}

interface CtxState {
  profile?: { settings?: Record<string, unknown> }
}

/**
 * Make Stremio resolve stream URLs against this origin.
 *
 * stremio-core's default points at the browser's own loopback, which is correct
 * for the desktop app and wrong for every device that reaches the appliance over
 * the network. Setting it is a supported action — it is the same one Stremio's
 * settings screen dispatches — so nothing here depends on modified upstream code.
 */
export async function ensureStreamingServer(
  core: CoreTransport,
  desired = streamingServerUrl(),
): Promise<'adopted' | 'kept'> {
  const ctx = (await core.getState('ctx')) as CtxState | undefined
  const settings = ctx?.profile?.settings
  if (!settings) return 'kept'
  const current = settings.streamingServerUrl as string | undefined
  if (!shouldAdoptServerUrl(current, desired)) return 'kept'

  await core.dispatch({ action: 'Ctx', args: { action: 'AddServerUrl', args: desired } })
  await core.dispatch({
    action: 'Ctx',
    args: {
      action: 'UpdateSettings',
      args: { ...settings, streamingServerUrl: desired },
    },
  })
  return 'adopted'
}

export interface PreviewState {
  /** The stream URL Stremio resolved, i.e. exactly what it would cast. */
  source: string | null
  /** Preview position in milliseconds. */
  timeMs: number
  durationMs: number | null
  title: string | null
  playing: boolean
}

/** The `<video>` the preview is running in, when there is one. */
export function previewVideo(root: ParentNode = document): HTMLVideoElement | null {
  return root.querySelector('video')
}

interface PlayerState {
  selected?: { stream?: { deepLinks?: { externalPlayer?: { streaming?: string | null } } } }
  title?: string | null
  metaItem?: { content?: { content?: { name?: string } } }
}

/**
 * What the preview is playing right now.
 *
 * The source comes from core, so it is byte-identical to what Stremio's own
 * "play on device" would send. The position comes from the media element, which
 * is the only place the live playhead actually lives.
 */
export async function readPreview(core: CoreTransport): Promise<PreviewState> {
  let source: string | null = null
  let title: string | null = null
  try {
    const player = (await core.getState('player')) as PlayerState | undefined
    source = player?.selected?.stream?.deepLinks?.externalPlayer?.streaming ?? null
    title = player?.title ?? player?.metaItem?.content?.content?.name ?? null
  } catch {
    // Core may not have a player model yet; the media element still answers.
  }
  const video = previewVideo()
  if (!source && video?.currentSrc) source = video.currentSrc
  const timeMs = video && Number.isFinite(video.currentTime) ? Math.round(video.currentTime * 1000) : 0
  const durationMs =
    video && Number.isFinite(video.duration) && video.duration > 0
      ? Math.round(video.duration * 1000)
      : null
  return {
    source,
    timeMs: Math.max(0, timeMs),
    durationMs,
    title,
    playing: Boolean(video && !video.paused && !video.ended),
  }
}

export function pausePreview(root: ParentNode = document): void {
  const video = previewVideo(root)
  if (video && !video.paused) video.pause()
}

/**
 * Cast through Stremio's own action so the request reaches the backend on the
 * documented `POST {server}/casting/{device}/player` path.
 *
 * Upstream's options menu calls the same action but always sends time 0; this
 * is the same call with the live preview position filled in.
 */
export async function castToDevice(
  core: CoreTransport,
  device: string,
  source: string,
  timeMs: number,
): Promise<void> {
  await core.dispatch({
    action: 'StreamingServer',
    args: {
      action: 'PlayOnDevice',
      args: { device, source, time: Math.max(0, Math.round(timeMs)) },
    },
  })
}

/** True when Stremio is showing its player, i.e. a preview is on screen. */
export function isPlayerRoute(hash = location.hash): boolean {
  return hash.startsWith('#/player')
}
