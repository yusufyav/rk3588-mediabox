import { afterEach, beforeEach, describe, expect, it } from 'vitest'
import { fireEvent, screen, waitFor } from '@testing-library/react'
import {
  castToDevice,
  ensureStreamingServer,
  isPlayerRoute,
  pausePreview,
  readPreview,
  shouldAdoptServerUrl,
  streamingServerUrl,
} from '../src/stremio/core'
import { attachPreviewVideo, FakeCore, renderApp, StubTransport, stubResponses } from './helpers'

afterEach(() => {
  FakeCore.uninstall()
  document.querySelectorAll('video').forEach((element) => element.remove())
  location.hash = ''
})

describe('Akış sunucusu adresi', () => {
  it('resolves against the appliance origin, not the browser loopback', () => {
    expect(streamingServerUrl('http://10.27.27.25:8787')).toBe('http://10.27.27.25:8787/')
  })

  it('replaces an untouched upstream default', () => {
    const desired = 'http://10.27.27.25:8787/'
    expect(shouldAdoptServerUrl('http://127.0.0.1:11470/', desired)).toBe(true)
    expect(shouldAdoptServerUrl('http://localhost:11470/', desired)).toBe(true)
    expect(shouldAdoptServerUrl(undefined, desired)).toBe(true)
  })

  it('leaves a server the user deliberately chose alone', () => {
    const desired = 'http://10.27.27.25:8787/'
    expect(shouldAdoptServerUrl('http://192.168.1.9:11470/', desired)).toBe(false)
  })

  it('does nothing when it is already pointed here', () => {
    const desired = 'http://10.27.27.25:8787/'
    expect(shouldAdoptServerUrl(desired, desired)).toBe(false)
  })

  it('applies the change through the core action the settings screen uses', async () => {
    const core = new FakeCore()
    const outcome = await ensureStreamingServer(core, 'http://box:8787/')

    expect(outcome).toBe('adopted')
    const ctx = core.actions('Ctx')
    const kinds = ctx.map((action) => (action.args as { action: string }).action)
    expect(kinds).toEqual(['AddServerUrl', 'UpdateSettings'])
    const settings = (ctx[1].args as { args: Record<string, unknown> }).args
    expect(settings.streamingServerUrl).toBe('http://box:8787/')
  })

  it('keeps every other setting intact when it changes the server', async () => {
    const core = new FakeCore({
      ctx: {
        profile: {
          settings: { streamingServerUrl: 'http://127.0.0.1:11470/', subtitlesSize: 125 },
        },
      },
    })
    await ensureStreamingServer(core, 'http://box:8787/')

    const update = core.actions('Ctx')[1].args as { args: Record<string, unknown> }
    expect(update.args.subtitlesSize).toBe(125)
  })
})

describe('Ön izleme durumu', () => {
  it('reads the source from core, so it is what Stremio itself would cast', async () => {
    const core = new FakeCore({
      ctx: { profile: { settings: {} } },
      player: {
        selected: {
          stream: { deepLinks: { externalPlayer: { streaming: 'http://box/abc/0' } } },
        },
      },
    })
    attachPreviewVideo(42.5)

    const preview = await readPreview(core)

    expect(preview.source).toBe('http://box/abc/0')
    expect(preview.timeMs).toBe(42_500)
  })

  it('falls back to the media element when core has no player yet', async () => {
    const core = new FakeCore({ ctx: { profile: { settings: {} } }, player: {} })
    attachPreviewVideo(3, 'http://box/xyz/1')

    expect((await readPreview(core)).source).toBe('http://box/xyz/1')
  })

  it('recognises the player route', () => {
    expect(isPlayerRoute('#/player/abc')).toBe(true)
    expect(isPlayerRoute('#/board')).toBe(false)
  })

  it('stops the preview on request', () => {
    const video = attachPreviewVideo(10)
    pausePreview()
    expect(video.paused).toBe(true)
  })
})

describe('Kodi devri', () => {
  it('casts through the upstream action, on the documented contract', async () => {
    const core = new FakeCore()
    await castToDevice(core, 'mediabox-tv', 'http://box/abc/0', 12_500)

    const [action] = core.actions('StreamingServer')
    expect(action.args).toEqual({
      action: 'PlayOnDevice',
      args: { device: 'mediabox-tv', source: 'http://box/abc/0', time: 12_500 },
    })
  })

  it('never sends a negative position', async () => {
    const core = new FakeCore()
    await castToDevice(core, 'mediabox-tv', 'http://box/abc/0', -5)
    const [action] = core.actions('StreamingServer')
    expect((action.args as { args: { time: number } }).args.time).toBe(0)
  })
})

describe('Ön izle ve Kodi ayrı aksiyonlardır', () => {
  beforeEach(() => {
    location.hash = '#/player/test'
  })

  it('shows the handoff control only while a preview is on screen', async () => {
    new FakeCore().install()
    location.hash = '#/board'
    renderApp(new StubTransport(stubResponses()))
    await screen.findByRole('button', { name: 'MediaBox ayarları' })

    expect(screen.queryByTestId('cast-bar')).toBeNull()
  })

  it('starts nothing on the television until the control is used', async () => {
    const core = new FakeCore().install()
    attachPreviewVideo(25)
    const transport = new StubTransport(stubResponses())
    renderApp(transport)

    await screen.findByTestId('cast-bar')

    // Opening a preview must not reach Kodi at all.
    expect(core.actions('StreamingServer')).toHaveLength(0)
    expect(transport.calledPaths('POST')).toHaveLength(0)
  })

  it('hands the running preview over with its position', async () => {
    const core = new FakeCore({
      ctx: { profile: { settings: { streamingServerUrl: 'http://box:8787/' } } },
      player: {
        selected: {
          stream: { deepLinks: { externalPlayer: { streaming: 'http://box:8787/abc/0' } } },
        },
      },
    }).install()
    const video = attachPreviewVideo(31)
    renderApp(new StubTransport(stubResponses()))

    // The control stays disabled until Stremio has actually resolved a stream.
    const button = await screen.findByRole('button', { name: "Kodi'de Oynat" })
    await waitFor(() => expect(button).toHaveProperty('disabled', false))
    fireEvent.click(button)

    await waitFor(() => expect(core.actions('StreamingServer')).toHaveLength(1))
    const args = (core.actions('StreamingServer')[0].args as { args: Record<string, unknown> }).args
    expect(args.source).toBe('http://box:8787/abc/0')
    expect(args.time).toBe(31_000)
    // One torrent engine must not feed two readers at two positions.
    expect(video.paused).toBe(true)
  })

  it('refuses to hand over a stream Stremio has not resolved', async () => {
    new FakeCore({ ctx: { profile: { settings: {} } }, player: {} }).install()
    renderApp(new StubTransport(stubResponses()))

    const button = await screen.findByRole('button', { name: "Kodi'de Oynat" })
    expect(button).toHaveProperty('disabled', true)
  })
})
