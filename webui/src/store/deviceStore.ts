import { ApiClient, ApiError } from '../api/client'
import type {
  ActionAvailability,
  DisplayResponse,
  HealthResponse,
  KodiResponse,
  NetworkResponse,
  SystemResponse,
} from '../api/types'

export interface Resource<T> {
  data?: T
  error?: ApiError
  /** Epoch ms of the last successful sample; undefined until the first one. */
  updatedAt?: number
  loading: boolean
}

export interface DeviceState {
  health: Resource<HealthResponse>
  system: Resource<SystemResponse>
  network: Resource<NetworkResponse>
  kodi: Resource<KodiResponse>
  display: Resource<DisplayResponse>
  /** True while a push stream is delivering updates. */
  streamConnected: boolean
  /** False once every resource is failing with an unreachable error. */
  backendReachable: boolean
  /** Last failed command, shown inline instead of failing silently. */
  lastActionError?: { message: string; at: number }
  pendingAction?: string
}

type ResourceKey = 'health' | 'system' | 'network' | 'kodi' | 'display'

/** Poll cadences in ms. Kodi is fast because the player scrubber rides on it. */
export const CADENCE: Record<ResourceKey, number> = {
  kodi: 1500,
  system: 5000,
  health: 10_000,
  network: 10_000,
  display: 10_000,
}

/** When a push stream is live, polling drops to a slow safety net. */
const STREAM_BACKOFF = 4

/** A sample older than cadence × this factor is rendered as stale, never as truth. */
export const STALE_FACTOR = 4

const EMPTY: Resource<never> = { loading: true }

function initialState(): DeviceState {
  return {
    health: { ...EMPTY },
    system: { ...EMPTY },
    network: { ...EMPTY },
    kodi: { ...EMPTY },
    display: { ...EMPTY },
    streamConnected: false,
    backendReachable: true,
  }
}

export function isStale(resource: Resource<unknown>, cadenceMs: number, now = Date.now()): boolean {
  if (resource.updatedAt === undefined) return false
  return now - resource.updatedAt > cadenceMs * STALE_FACTOR
}

/**
 * The one and only place that talks to the API on a timer.
 *
 * Components subscribe to the snapshot; none of them owns an interval, so the
 * request rate stays constant no matter how many cards are mounted.
 */
export class DeviceStore {
  private state = initialState()
  private kodiTick = 0
  private listeners = new Set<() => void>()
  private timers: Array<ReturnType<typeof setInterval>> = []
  private unsubscribeStream?: () => void
  private started = false

  constructor(private readonly api: ApiClient) {}

  getSnapshot = (): DeviceState => this.state

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener)
    return () => this.listeners.delete(listener)
  }

  start() {
    if (this.started) return
    this.started = true

    void this.refreshAll()

    const fetchers: Record<ResourceKey, () => Promise<void>> = {
      health: () => this.load('health', this.api.getHealth),
      system: () => this.load('system', this.api.getSystem),
      network: () => this.load('network', this.api.getNetwork),
      kodi: () => this.load('kodi', this.api.getKodi),
      display: () => this.load('display', this.api.getDisplay),
    }

    for (const key of Object.keys(fetchers) as ResourceKey[]) {
      this.timers.push(
        setInterval(() => {
          // The stream is authoritative for Kodi, so polling becomes a slow
          // safety net instead of a second source of truth.
          if (key === 'kodi' && this.state.streamConnected) {
            this.kodiTick = (this.kodiTick + 1) % STREAM_BACKOFF
            if (this.kodiTick !== 0) return
          }
          void fetchers[key]()
        }, CADENCE[key]),
      )
    }

    this.unsubscribeStream = this.api.subscribe({
      onStatus: (connected) => this.patch({ streamConnected: connected }),
      onEvent: (event) => {
        if (event.type === 'kodi' && event.payload) {
          this.setResource('kodi', event.payload as KodiResponse)
        } else if (event.type === 'system' && event.payload) {
          this.setResource('system', event.payload as SystemResponse)
        } else if (event.type === 'display' && event.payload) {
          this.setResource('display', event.payload as DisplayResponse)
        } else if (event.type === 'network' && event.payload) {
          this.setResource('network', event.payload as NetworkResponse)
        }
      },
    })
  }

  stop() {
    this.timers.forEach(clearInterval)
    this.timers = []
    this.unsubscribeStream?.()
    this.unsubscribeStream = undefined
    this.started = false
  }

  refreshAll = async (): Promise<void> => {
    await Promise.all([
      this.load('health', this.api.getHealth),
      this.load('system', this.api.getSystem),
      this.load('network', this.api.getNetwork),
      this.load('kodi', this.api.getKodi),
      this.load('display', this.api.getDisplay),
    ])
  }

  refreshKodi = () => this.load('kodi', this.api.getKodi)

  /** Available actions as reported by the backend; absent capability = disabled. */
  get actions(): ActionAvailability {
    return this.state.health.data?.actions ?? {}
  }

  /**
   * Run a command, surface any failure in the snapshot, then re-read the state
   * the command affected so the UI never shows a stale optimistic value.
   */
  runAction = async (name: string, call: () => Promise<void>): Promise<boolean> => {
    this.patch({ pendingAction: name, lastActionError: undefined })
    try {
      await call()
      await this.refreshKodi()
      this.patch({ pendingAction: undefined })
      return true
    } catch (error) {
      const message = error instanceof Error ? error.message : String(error)
      this.patch({ pendingAction: undefined, lastActionError: { message, at: Date.now() } })
      return false
    }
  }

  clearActionError = () => this.patch({ lastActionError: undefined })

  private async load<T>(key: ResourceKey, fetcher: (signal?: AbortSignal) => Promise<T>) {
    try {
      const data = await fetcher()
      this.setResource(key, data as never)
    } catch (error) {
      const apiError =
        error instanceof ApiError ? error : new ApiError(String(error), key, 0)
      this.patch({
        [key]: { ...this.state[key], error: apiError, loading: false },
      } as Partial<DeviceState>)
      this.recomputeReachability()
    }
  }

  private setResource<K extends ResourceKey>(key: K, data: DeviceState[K]['data']) {
    this.patch({
      [key]: { data, error: undefined, updatedAt: Date.now(), loading: false },
    } as Partial<DeviceState>)
    this.recomputeReachability()
  }

  private recomputeReachability() {
    const keys: ResourceKey[] = ['health', 'system', 'network', 'kodi', 'display']
    const touched = keys.filter((k) => this.state[k].error || this.state[k].updatedAt)
    const reachable =
      touched.length === 0 || touched.some((k) => !this.state[k].error?.unreachable)
    if (reachable !== this.state.backendReachable) this.patch({ backendReachable: reachable })
  }

  private patch(partial: Partial<DeviceState>) {
    this.state = { ...this.state, ...partial }
    this.listeners.forEach((listener) => listener())
  }
}
