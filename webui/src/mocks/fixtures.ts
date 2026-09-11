import type {
  CastSession,
  DisplayResponse,
  HealthResponse,
  KodiResponse,
  NetworkResponse,
  StremioStatus,
  SystemResponse,
} from '../api/types'

/** Scenarios selectable at runtime with `?scenario=` in mock mode. */
export type ScenarioName = 'playing' | 'idle' | 'kodi-offline' | 'backend-down'

export const SCENARIOS: Array<{ id: ScenarioName; label: string }> = [
  { id: 'playing', label: 'Kodi oynatıyor' },
  { id: 'idle', label: 'Kodi boşta' },
  { id: 'kodi-offline', label: 'Kodi kapalı' },
  { id: 'backend-down', label: 'Backend erişilemiyor' },
]

export const health: HealthResponse = {
  media: { stremio: true, castToKodi: true, serverMount: '/', castDeviceId: 'mediabox-tv' },
  status: 'ok',
  version: 'mediaboxd 0.1.0-dev',
  uptimeSeconds: 191_240,
  actions: {
    kodiStart: true,
    kodiStop: true,
    kodiRestart: true,
    reboot: true,
    shutdown: true,
  },
}

export const system: SystemResponse = {
  hostname: 'mediabox',
  kernel: '6.1.99-rk3588',
  architecture: 'aarch64',
  uptimeSeconds: 191_240,
  cpu: { temperatureC: 48.6, loadAverage: [0.42, 0.51, 0.47], cores: 8 },
  memory: {
    totalBytes: 16_777_216_000,
    usedBytes: 3_951_230_976,
    availableBytes: 12_825_985_024,
  },
  storage: [
    { mountpoint: '/', totalBytes: 122_000_000_000, usedBytes: 31_400_000_000 },
    { mountpoint: '/media/library', totalBytes: 3_840_000_000_000, usedBytes: 2_615_000_000_000 },
  ],
}

export const network: NetworkResponse = {
  online: true,
  defaultRoute: { interface: 'eth0', gateway: '192.168.1.1' },
  interfaces: [
    {
      name: 'eth0',
      type: 'ethernet',
      up: true,
      ipv4: ['192.168.1.42'],
      mac: 'c2:41:8a:11:9d:04',
      linkSpeedMbps: 1000,
    },
    {
      name: 'wlan0',
      type: 'wifi',
      up: true,
      ipv4: ['192.168.1.77'],
      mac: '9e:2b:00:5c:31:aa',
      ssid: 'MediaBox-5G',
      signalPercent: 74,
    },
  ],
}

export const display: DisplayResponse = {
  connector: 'HDMI-A-1',
  connected: true,
  mode: '3840x2160',
  refreshHz: 59.94,
  colorDepth: 10,
  colorimetry: 'BT.2020 YCbCr 4:2:0',
  hdr: { supported: true, active: true, eotf: 'SMPTE ST 2084 (PQ)' },
}

export const kodiPlaying: KodiResponse = {
  serviceActive: true,
  state: 'playing',
  version: 'Kodi 21.1 (GBM)',
  player: {
    title: 'Blade Runner 2049 (2017) — 4K HDR10',
    mediaType: 'movie',
    position: 1_284,
    duration: 9_780,
  },
}

export const kodiIdle: KodiResponse = {
  serviceActive: true,
  state: 'idle',
  version: 'Kodi 21.1 (GBM)',
}

export const kodiOffline: KodiResponse = {
  serviceActive: false,
  state: 'offline',
}

export function kodiFor(scenario: ScenarioName): KodiResponse {
  switch (scenario) {
    case 'playing':
      return { ...kodiPlaying, player: { ...kodiPlaying.player } }
    case 'idle':
      return kodiIdle
    default:
      return kodiOffline
  }
}

export const stremio: StremioStatus = {
  enabled: true,
  reachable: true,
  mount: '/',
  serverVersion: '4.21.0',
  castDevice: { id: 'mediabox-tv', name: 'MediaBox TV (Kodi)', type: 'external' },
}

export const castSession: CastSession = {
  source: 'http://mediabox.local/server/2c0f4b1a/0',
  kodiSource: 'http://127.0.0.1:11470/2c0f4b1a/0',
  resumeSeconds: 21.5,
  deviceId: 'mediabox-tv',
  startedAt: 1_757_600_000,
}
