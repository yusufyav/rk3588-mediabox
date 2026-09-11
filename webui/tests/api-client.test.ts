import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest'
import { ApiClient, ApiError, HttpTransport, resolveApiBase } from '../src/api/client'
import { ENDPOINTS } from '../src/api/endpoints'
import { StubTransport } from './helpers'

describe('ApiClient', () => {
  it('maps every command to its contract endpoint', async () => {
    const transport = new StubTransport()
    const api = new ApiClient(transport)

    await api.getHealth()
    await api.getSystem()
    await api.getNetwork()
    await api.getKodi()
    await api.getDisplay()
    await api.playPause()
    await api.stopPlayback()
    await api.seek(-30)
    await api.open('/media/film.mkv')
    await api.startKodi()
    await api.stopKodi()
    await api.restartKodi()

    expect(transport.calledPaths()).toEqual([
      'health',
      'system',
      'network',
      'kodi',
      'display',
      'kodi/playpause',
      'kodi/stop',
      'kodi/seek',
      'kodi/open',
      'kodi/start',
      'kodi/stop-service',
      'kodi/restart',
    ])
  })

  it('separates playback stop from service stop', async () => {
    const transport = new StubTransport()
    const api = new ApiClient(transport)

    await api.stopPlayback()
    await api.stopKodi()

    expect(transport.calls[0].path).toBe(ENDPOINTS.kodiStop)
    expect(transport.calls[1].path).toBe(ENDPOINTS.kodiServiceStop)
  })

  it('sends seek offsets as a JSON body', async () => {
    const transport = new StubTransport()
    await new ApiClient(transport).seek(10)

    expect(transport.calls[0]).toMatchObject({
      path: ENDPOINTS.kodiSeek,
      method: 'POST',
      body: { offsetSeconds: 10 },
    })
  })
})

describe('HttpTransport', () => {
  const originalFetch = globalThis.fetch

  beforeEach(() => {
    vi.stubGlobal('fetch', vi.fn())
  })

  afterEach(() => {
    vi.unstubAllGlobals()
    globalThis.fetch = originalFetch
  })

  it('resolves paths against the api base', async () => {
    const fetchMock = vi.fn(
      async (_url: string, _init?: RequestInit) => new Response('{"status":"ok"}', { status: 200 }),
    )
    vi.stubGlobal('fetch', fetchMock)

    await new HttpTransport('http://box.local/api/v1/').request('health')

    expect(fetchMock.mock.calls[0][0]).toBe('http://box.local/api/v1/health')
  })

  it('reports an unreachable backend distinctly from an HTTP error', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new TypeError('Failed to fetch')
      }),
    )

    const error = await new HttpTransport('http://box.local/api/v1/')
      .request('health')
      .catch((e: unknown) => e)

    expect(error).toBeInstanceOf(ApiError)
    expect((error as ApiError).unreachable).toBe(true)
  })

  it('treats a non-2xx response as a reachable failure', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response('nope', { status: 503 })))

    const error = await new HttpTransport('http://box.local/api/v1/')
      .request('kodi')
      .catch((e: unknown) => e)

    expect((error as ApiError).status).toBe(503)
    expect((error as ApiError).unreachable).toBe(false)
  })

  it('tolerates an empty 204 body', async () => {
    vi.stubGlobal('fetch', vi.fn(async () => new Response(null, { status: 204 })))

    await expect(
      new HttpTransport('http://box.local/api/v1/').request('kodi/playpause', { method: 'POST' }),
    ).resolves.toBeUndefined()
  })

  it('derives the api base from the document so a sub-path mount works', () => {
    expect(resolveApiBase()).toMatch(/\/api\/v1\/$/)
  })
})
