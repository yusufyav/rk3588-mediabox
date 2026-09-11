import { ENDPOINTS } from './endpoints'
import type {
  CastRequest,
  CastSession,
  DisplayResponse,
  HealthResponse,
  KodiResponse,
  NetworkResponse,
  StremioStatus,
  SystemResponse,
} from './types'

export class ApiError extends Error {
  readonly status: number
  readonly path: string
  /** True when the request never reached mediaboxd (offline, DNS, timeout). */
  readonly unreachable: boolean

  constructor(message: string, path: string, status = 0) {
    super(message)
    this.name = 'ApiError'
    this.path = path
    this.status = status
    this.unreachable = status === 0
  }
}

export interface RequestOptions {
  method?: 'GET' | 'POST'
  body?: unknown
  signal?: AbortSignal
}

/**
 * Anything that can answer an API call. Swapping this is how mock mode works,
 * so nothing above the transport ever knows whether a real box is attached.
 */
export interface Transport {
  request<T>(path: string, options?: RequestOptions): Promise<T>
  /**
   * Subscribe to push updates. Returns an unsubscribe function. Implementations
   * that cannot push simply never call `onEvent`; the data layer keeps polling.
   */
  subscribe?(handlers: StreamHandlers): () => void
}

export interface StreamEvent {
  type: string
  payload?: unknown
}

export interface StreamHandlers {
  onEvent: (event: StreamEvent) => void
  onStatus: (connected: boolean) => void
}

const DEFAULT_TIMEOUT_MS = 8000

/** The UI may live under /ui/, while the API is always rooted at /api/v1/. */
export function resolveApiBase(): string {
  const override = import.meta.env?.VITE_API_BASE
  if (override) return override.endsWith('/') ? override : `${override}/`
  const base = typeof document !== 'undefined' ? document.baseURI : 'http://localhost/'
  return new URL('/api/v1/', base).toString()
}

export class HttpTransport implements Transport {
  constructor(
    private readonly baseUrl: string = resolveApiBase(),
    private readonly timeoutMs: number = DEFAULT_TIMEOUT_MS,
  ) {}

  async request<T>(path: string, options: RequestOptions = {}): Promise<T> {
    const { method = 'GET', body, signal } = options
    const url = new URL(path, this.baseUrl).toString()
    const controller = new AbortController()
    const timer = setTimeout(() => controller.abort(), this.timeoutMs)
    if (signal) signal.addEventListener('abort', () => controller.abort(), { once: true })

    let response: Response
    try {
      response = await fetch(url, {
        method,
        signal: controller.signal,
        headers: body === undefined ? undefined : { 'content-type': 'application/json' },
        body: body === undefined ? undefined : JSON.stringify(body),
      })
    } catch (error) {
      const reason = error instanceof Error ? error.message : String(error)
      throw new ApiError(`MediaBox servisine ulaşılamadı (${reason})`, path, 0)
    } finally {
      clearTimeout(timer)
    }

    if (!response.ok) {
      throw new ApiError(`${method} ${path} → HTTP ${response.status}`, path, response.status)
    }
    if (response.status === 204) return undefined as T
    const text = await response.text()
    if (!text) return undefined as T
    try {
      return JSON.parse(text) as T
    } catch {
      throw new ApiError(`${path} geçersiz JSON döndürdü`, path, response.status)
    }
  }

  /**
   * Prefer the WebSocket stream; fall back to SSE. If neither connects the data
   * layer keeps its polling cadence, so failure here is never fatal.
   */
  subscribe(handlers: StreamHandlers): () => void {
    let closed = false
    let ws: WebSocket | undefined
    let sse: EventSource | undefined

    const startSse = () => {
      if (closed || typeof EventSource === 'undefined') return
      try {
        sse = new EventSource(new URL('events', this.baseUrl).toString())
        sse.onopen = () => handlers.onStatus(true)
        sse.onerror = () => handlers.onStatus(false)
        sse.onmessage = (event) => handlers.onEvent(parseEvent(event.data))
      } catch {
        handlers.onStatus(false)
      }
    }

    if (typeof WebSocket !== 'undefined') {
      try {
        const wsUrl = new URL('ws', this.baseUrl)
        wsUrl.protocol = wsUrl.protocol === 'https:' ? 'wss:' : 'ws:'
        ws = new WebSocket(wsUrl.toString())
        ws.onopen = () => handlers.onStatus(true)
        ws.onmessage = (event) => handlers.onEvent(parseEvent(event.data))
        ws.onerror = () => handlers.onStatus(false)
        ws.onclose = () => {
          handlers.onStatus(false)
          if (!closed && !sse) startSse()
        }
      } catch {
        startSse()
      }
    } else {
      startSse()
    }

    return () => {
      closed = true
      ws?.close()
      sse?.close()
    }
  }
}

function parseEvent(raw: unknown): StreamEvent {
  if (typeof raw !== 'string') return { type: 'unknown' }
  try {
    const parsed = JSON.parse(raw) as StreamEvent
    return parsed && typeof parsed.type === 'string' ? parsed : { type: 'unknown' }
  } catch {
    return { type: 'unknown' }
  }
}

/** Typed façade over the transport. Pages and the data layer only use this. */
export class ApiClient {
  constructor(readonly transport: Transport) {}

  getHealth = (signal?: AbortSignal) =>
    this.transport.request<HealthResponse>(ENDPOINTS.health, { signal })

  getSystem = (signal?: AbortSignal) =>
    this.transport.request<SystemResponse>(ENDPOINTS.system, { signal })

  getNetwork = (signal?: AbortSignal) =>
    this.transport.request<NetworkResponse>(ENDPOINTS.network, { signal })

  getKodi = (signal?: AbortSignal) =>
    this.transport.request<KodiResponse>(ENDPOINTS.kodi, { signal })

  getDisplay = (signal?: AbortSignal) =>
    this.transport.request<DisplayResponse>(ENDPOINTS.display, { signal })

  playPause = () => this.transport.request<void>(ENDPOINTS.kodiPlayPause, { method: 'POST' })

  stopPlayback = () => this.transport.request<void>(ENDPOINTS.kodiStop, { method: 'POST' })

  seek = (seconds: number) =>
    this.transport.request<void>(ENDPOINTS.kodiSeek, {
      method: 'POST',
      body: { seconds },
    })

  open = (url: string, resumeSeconds = 0) =>
    this.transport.request<void>(ENDPOINTS.kodiOpen, {
      method: 'POST',
      body: { url, resume_seconds: resumeSeconds },
    })

  startKodi = () => this.transport.request<void>(ENDPOINTS.kodiServiceStart, { method: 'POST' })

  stopKodi = () => this.transport.request<void>(ENDPOINTS.kodiServiceStop, { method: 'POST' })

  restartKodi = () => this.transport.request<void>(ENDPOINTS.kodiServiceRestart, { method: 'POST' })

  getStremio = (signal?: AbortSignal) =>
    this.transport.request<StremioStatus>(ENDPOINTS.stremio, { signal })

  getCastSession = (signal?: AbortSignal) =>
    this.transport
      .request<{ session: CastSession | null }>(ENDPOINTS.cast, { signal })
      .then((response) => response?.session ?? null)

  /** Hand a Stremio-resolved stream to Kodi, carrying the preview position. */
  castToKodi = (request: CastRequest) =>
    this.transport.request<{ status: string; result: CastSession }>(ENDPOINTS.castKodi, {
      method: 'POST',
      body: request,
    })

  reboot = () => this.transport.request<void>(ENDPOINTS.systemReboot, { method: 'POST' })

  shutdown = () => this.transport.request<void>(ENDPOINTS.systemShutdown, { method: 'POST' })

  subscribe(handlers: StreamHandlers): () => void {
    return this.transport.subscribe ? this.transport.subscribe(handlers) : () => {}
  }
}
