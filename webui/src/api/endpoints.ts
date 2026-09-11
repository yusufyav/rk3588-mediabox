/**
 * Single place where every backend path lives.
 *
 * Contract assumptions beyond the frozen M1A list (documented in docs/webui.md):
 *  - `GET /api/v1/health` may carry an `actions` capability map.
 *  - `POST /api/v1/system/reboot` and `/api/v1/system/shutdown` back the
 *    Settings power controls.
 * Both are treated as optional: if they are missing the UI disables the
 * affected control instead of failing.
 */
export const ENDPOINTS = {
  health: 'health',
  system: 'system',
  network: 'network',
  kodi: 'kodi',
  display: 'display',

  kodiPlayPause: 'kodi/playpause',
  kodiStop: 'kodi/stop',
  kodiSeek: 'kodi/seek',
  kodiOpen: 'kodi/open',

  kodiServiceStart: 'kodi/start',
  kodiServiceStop: 'kodi/stop-service',
  kodiServiceRestart: 'kodi/restart',

  systemReboot: 'system/reboot',
  systemShutdown: 'system/shutdown',
} as const

export type EndpointKey = keyof typeof ENDPOINTS
