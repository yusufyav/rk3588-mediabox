import { render } from '@testing-library/react'
import type { ReactElement } from 'react'
import { ApiError, type RequestOptions, type StreamHandlers, type Transport } from '../src/api/client'
import { ENDPOINTS } from '../src/api/endpoints'
import * as fx from '../src/mocks/fixtures'
import { App } from '../src/App'
import { DeviceProvider } from '../src/store/DeviceProvider'

export interface RecordedCall {
  path: string
  method: string
  body?: unknown
}

/**
 * Deterministic transport for tests: no timers, no mutation, and every request
 * is recorded so a test can assert which endpoint a control actually hit.
 */
export class StubTransport implements Transport {
  readonly calls: RecordedCall[] = []
  private handlers = new Set<StreamHandlers>()

  constructor(
    private responses: Partial<Record<string, unknown>> = {},
    private readonly failWith?: ApiError,
  ) {}

  setResponse(path: string, value: unknown) {
    this.responses = { ...this.responses, [path]: value }
  }

  async request<T>(path: string, options: RequestOptions = {}): Promise<T> {
    this.calls.push({ path, method: options.method ?? 'GET', body: options.body })
    if (this.failWith) throw this.failWith
    if (path in this.responses) return this.responses[path] as T
    return undefined as T
  }

  subscribe(handlers: StreamHandlers): () => void {
    this.handlers.add(handlers)
    handlers.onStatus(false)
    return () => this.handlers.delete(handlers)
  }

  calledPaths(method?: string): string[] {
    return this.calls.filter((c) => !method || c.method === method).map((c) => c.path)
  }
}

export function stubResponses(kodi: unknown = fx.kodiPlaying): Record<string, unknown> {
  return {
    [ENDPOINTS.health]: fx.health,
    [ENDPOINTS.system]: fx.system,
    [ENDPOINTS.network]: fx.network,
    [ENDPOINTS.display]: fx.display,
    [ENDPOINTS.kodi]: kodi,
  }
}

export function renderApp(transport: Transport) {
  return render(
    <DeviceProvider transport={transport}>
      <App />
    </DeviceProvider>,
  )
}

export function renderWithProvider(ui: ReactElement, transport: Transport) {
  return render(<DeviceProvider transport={transport}>{ui}</DeviceProvider>)
}

/** Drives the viewport classifier, which reads `window.innerWidth/Height`. */
export function setViewport(width: number, height: number) {
  Object.defineProperty(window, 'innerWidth', { value: width, configurable: true })
  Object.defineProperty(window, 'innerHeight', { value: height, configurable: true })
  window.dispatchEvent(new Event('resize'))
}
