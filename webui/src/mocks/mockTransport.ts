import { ApiError, type RequestOptions, type StreamHandlers, type Transport } from '../api/client'
import { ENDPOINTS } from '../api/endpoints'
import type { KodiResponse } from '../api/types'
import * as fx from './fixtures'
import type { ScenarioName } from './fixtures'

/**
 * In-memory backend. It keeps mutable playback state so the player controls are
 * genuinely exercised (seek moves the position, play/pause flips the state)
 * without a device attached.
 */
export class MockTransport implements Transport {
  private kodi: KodiResponse
  private listeners = new Set<StreamHandlers>()
  private clock?: ReturnType<typeof setInterval>

  constructor(
    private scenario: ScenarioName = 'playing',
    /** Emit push events like the real WebSocket would. */
    private readonly push = true,
  ) {
    this.kodi = fx.kodiFor(scenario)
    if (this.push && this.scenario !== 'backend-down') this.startClock()
  }

  setScenario(scenario: ScenarioName) {
    this.scenario = scenario
    this.kodi = fx.kodiFor(scenario)
    this.emit()
  }

  private startClock() {
    this.clock = setInterval(() => {
      if (this.kodi.state === 'playing' && this.kodi.player) {
        const duration = this.kodi.player.duration ?? 0
        this.kodi.player.position = Math.min((this.kodi.player.position ?? 0) + 1, duration)
      }
      this.emit()
    }, 1000)
  }

  private emit() {
    for (const listener of this.listeners) {
      listener.onEvent({ type: 'kodi', payload: structuredClone(this.kodi) })
    }
  }

  async request<T>(path: string, options: RequestOptions = {}): Promise<T> {
    await delay(60)
    if (this.scenario === 'backend-down') {
      throw new ApiError('MediaBox servisine ulaşılamadı (mock: backend-down)', path, 0)
    }

    switch (path) {
      case ENDPOINTS.health:
        return fx.health as T
      case ENDPOINTS.system:
        return fx.system as T
      case ENDPOINTS.network:
        return fx.network as T
      case ENDPOINTS.display:
        return fx.display as T
      case ENDPOINTS.kodi:
        return structuredClone(this.kodi) as T
    }

    if (options.method !== 'POST') {
      throw new ApiError(`Mock: bilinmeyen uç nokta ${path}`, path, 404)
    }
    return this.command<T>(path, options)
  }

  private command<T>(path: string, options: RequestOptions): T {
    const offline = this.kodi.state === 'offline'
    const playbackPaths: string[] = [
      ENDPOINTS.kodiPlayPause,
      ENDPOINTS.kodiStop,
      ENDPOINTS.kodiSeek,
      ENDPOINTS.kodiOpen,
    ]
    if (offline && playbackPaths.includes(path)) {
      throw new ApiError('Kodi çalışmıyor', path, 409)
    }

    switch (path) {
      case ENDPOINTS.kodiPlayPause:
        this.kodi.state = this.kodi.state === 'playing' ? 'paused' : 'playing'
        break
      case ENDPOINTS.kodiStop:
        this.kodi = { ...fx.kodiIdle }
        break
      case ENDPOINTS.kodiSeek: {
        const offset = (options.body as { offsetSeconds?: number } | undefined)?.offsetSeconds ?? 0
        const player = this.kodi.player
        if (player) {
          const duration = player.duration ?? 0
          player.position = clamp((player.position ?? 0) + offset, 0, duration)
        }
        break
      }
      case ENDPOINTS.kodiOpen:
        this.kodi = fx.kodiFor('playing')
        break
      case ENDPOINTS.kodiServiceStart:
      case ENDPOINTS.kodiServiceRestart:
        this.kodi = { ...fx.kodiIdle }
        break
      case ENDPOINTS.kodiServiceStop:
        this.kodi = { ...fx.kodiOffline }
        break
      case ENDPOINTS.systemReboot:
      case ENDPOINTS.systemShutdown:
        this.kodi = { ...fx.kodiOffline }
        break
      default:
        throw new ApiError(`Mock: bilinmeyen komut ${path}`, path, 404)
    }
    this.emit()
    return undefined as T
  }

  subscribe(handlers: StreamHandlers): () => void {
    if (this.scenario === 'backend-down') {
      handlers.onStatus(false)
      return () => {}
    }
    this.listeners.add(handlers)
    handlers.onStatus(true)
    return () => {
      this.listeners.delete(handlers)
      if (this.listeners.size === 0 && this.clock) clearInterval(this.clock)
    }
  }
}

function clamp(value: number, min: number, max: number) {
  return Math.min(Math.max(value, min), max)
}

function delay(ms: number) {
  return new Promise((resolve) => setTimeout(resolve, ms))
}
