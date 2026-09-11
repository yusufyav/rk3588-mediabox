/**
 * Wire types for the mediaboxd HTTP API (contract v1).
 *
 * These names are the real mediaboxd v1 wire schema. Optional hardware fields
 * remain tolerant because not every Linux target exposes every sensor.
 */

export type KodiState = 'playing' | 'paused' | 'idle' | 'offline'

export interface HealthResponse {
  status: 'ok' | 'degraded' | 'error'
  version?: string
  uptimeSeconds?: number
  /** Optional capability map; absent means "assume nothing is permitted". */
  actions?: ActionAvailability
  /** Which media surfaces the backend is wired for. */
  media?: MediaCapabilities
}

export interface MediaCapabilities {
  /** The Stremio mount is served and proxied by this backend. */
  stremio?: boolean
  /** Streams can be handed to Kodi through the backend. */
  castToKodi?: boolean
  /** Path the streaming server is reachable under, e.g. `/server/`. */
  serverMount?: string
  /** Id of the cast device the backend registers with Stremio. */
  castDeviceId?: string
}

export interface StremioStatus {
  enabled: boolean
  reachable: boolean
  mount?: string
  serverVersion?: string | null
  castDevice?: { id: string; name: string; type: string }
}

/** The handoff the backend is currently responsible for. */
export interface CastSession {
  /** The stream URL as Stremio resolved it, against the proxy mount. */
  source: string
  /** The same stream as handed to Kodi: same path, loopback origin. */
  kodiSource: string
  resumeSeconds: number
  deviceId: string
  /** Epoch seconds. */
  startedAt: number
}

export interface SystemResponse {
  hostname?: string
  kernel?: string
  architecture?: string
  uptimeSeconds?: number
  cpu?: {
    temperatureC?: number
    loadAverage?: number[]
    cores?: number
  }
  memory?: {
    totalBytes?: number
    usedBytes?: number
    availableBytes?: number
  }
  storage?: Array<{
    mountpoint: string
    totalBytes?: number
    usedBytes?: number
  }>
}

export interface NetworkInterface {
  name: string
  type?: 'ethernet' | 'wifi' | 'loopback' | 'other'
  up?: boolean
  ipv4?: string[]
  mac?: string
  /** Wi-Fi only. */
  ssid?: string
  signalPercent?: number
  linkSpeedMbps?: number
}

export interface NetworkResponse {
  interfaces?: NetworkInterface[]
  defaultRoute?: {
    interface?: string
    gateway?: string
  }
  online?: boolean
}

export interface KodiPlayer {
  title?: string
  /** Seconds. */
  position?: number
  duration?: number
  mediaType?: string
  /** Set by the data layer, not the backend: epoch ms of the sample. */
  sampledAt?: number
}

export interface KodiResponse {
  /** Systemd unit state, independent of whether Kodi answers JSON-RPC. */
  serviceActive?: boolean
  state: KodiState
  player?: KodiPlayer
  version?: string
}

export interface DisplayResponse {
  connector?: string
  connected?: boolean
  mode?: string
  refreshHz?: number
  colorDepth?: number
  colorimetry?: string
  hdr?: {
    supported?: boolean
    active?: boolean
    eotf?: string
  }
}

/**
 * Which privileged actions mediaboxd is willing to run. A `false` (or absent)
 * entry must render as a disabled control with a reason, never as a button
 * that fails at click time.
 */
export interface ActionAvailability {
  kodiStart?: boolean
  kodiStop?: boolean
  kodiRestart?: boolean
  reboot?: boolean
  shutdown?: boolean
  reason?: string
}

export interface SeekRequest {
  /** Absolute playback position in seconds. */
  seconds: number
}

export interface OpenRequest {
  url: string
  resume_seconds?: number
}

export interface CastRequest {
  source: string
  /** Playback position in milliseconds, matching the upstream cast contract. */
  time?: number
  deviceId?: string
}
