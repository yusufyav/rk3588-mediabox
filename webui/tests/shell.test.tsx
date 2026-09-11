import { afterEach, describe, expect, it } from 'vitest'
import { fireEvent, screen, waitFor } from '@testing-library/react'
import * as fx from '../src/mocks/fixtures'
import { ENDPOINTS } from '../src/api/endpoints'
import { FakeCore, renderApp, StubTransport, stubResponses } from './helpers'

afterEach(() => FakeCore.uninstall())

async function openSettings() {
  fireEvent.click(await screen.findByRole('button', { name: 'MediaBox ayarları' }))
  return screen.findByRole('dialog', { name: 'Ayarlar' })
}

describe('Ürün yüzeyi', () => {
  it('puts no system telemetry on the media surface', async () => {
    renderApp(new StubTransport(stubResponses()))
    await screen.findByRole('button', { name: 'MediaBox ayarları' })

    // The numbers that used to be the home screen must not be on it any more.
    const surface = document.body.textContent ?? ''
    expect(surface).not.toContain('48.6')
    expect(surface).not.toContain(fx.system.kernel as string)
    expect(surface).not.toMatch(/HDMI-A-1/)
    expect(screen.queryByText(/Genel Bakış/)).toBeNull()
  })

  it('offers no admin navigation of its own', async () => {
    renderApp(new StubTransport(stubResponses()))
    await screen.findByRole('button', { name: 'MediaBox ayarları' })

    for (const gone of ['Genel Bakış', 'Oynatıcı', 'Cihaz', 'Ağ']) {
      expect(screen.queryByRole('button', { name: gone })).toBeNull()
    }
  })

  it('never advertises Stremio as a thing that is coming later', async () => {
    renderApp(new StubTransport(stubResponses()))
    await screen.findByRole('button', { name: 'MediaBox ayarları' })

    expect(document.body.textContent).not.toMatch(/Yakında/i)
  })

  it('claims no layout, so the media app underneath keeps the page', async () => {
    const host = document.createElement('div')
    host.id = 'app'
    host.textContent = 'medya uygulaması'
    document.body.prepend(host)

    renderApp(new StubTransport(stubResponses()))
    const shell = await waitFor(() => {
      const found = document.querySelector('[data-mediabox-shell]')
      expect(found).not.toBeNull()
      return found as Element
    })

    // The shell is a sibling of the media app, never a wrapper around it.
    expect(shell.contains(host)).toBe(false)
    expect(host.textContent).toBe('medya uygulaması')
    host.remove()
  })
})

describe('Ayarlar ikincil bir yüzeydir', () => {
  it('is reached from one control rather than being on screen', async () => {
    renderApp(new StubTransport(stubResponses()))

    expect(screen.queryByRole('dialog', { name: 'Ayarlar' })).toBeNull()
    expect(await openSettings()).toBeDefined()
  })

  it('carries the device sections', async () => {
    renderApp(new StubTransport(stubResponses()))
    await openSettings()

    for (const section of ['Oynatma', 'Ağ', 'Bluetooth', 'HDMI / CEC', 'Ekran', 'Ses', 'Sistem', 'Tanılama']) {
      expect(screen.getByRole('button', { name: section })).toBeDefined()
    }
  })

  it('says so plainly where no service exists instead of faking a control', async () => {
    renderApp(new StubTransport(stubResponses()))
    await openSettings()

    fireEvent.click(screen.getByRole('button', { name: 'Bluetooth' }))

    expect(screen.getByText(/servis tarafı desteği yok/)).toBeDefined()
    expect(screen.queryByRole('switch')).toBeNull()
  })

  it('closes on the back key without disturbing what is behind it', async () => {
    renderApp(new StubTransport(stubResponses()))
    await openSettings()

    fireEvent.keyDown(document, { key: 'Escape' })

    await waitFor(() => expect(screen.queryByRole('dialog', { name: 'Ayarlar' })).toBeNull())
  })
})

describe('Tanılama', () => {
  it('is where the telemetry lives now', async () => {
    renderApp(new StubTransport(stubResponses()))
    await openSettings()

    fireEvent.click(screen.getByRole('button', { name: 'Tanılama' }))

    expect(await screen.findByText('48.6 °C')).toBeDefined()
    expect(screen.getByText(fx.system.kernel as string)).toBeDefined()
    expect(screen.getByText(/HDMI-A-1/)).toBeDefined()
  })

  it('reports the streaming server and the cast target', async () => {
    renderApp(new StubTransport(stubResponses()))
    await openSettings()
    fireEvent.click(screen.getByRole('button', { name: 'Tanılama' }))

    expect(await screen.findByText(/4\.21\.0/)).toBeDefined()
    expect(screen.getByText(/mediabox-tv, external/)).toBeDefined()
  })
})

describe('TV kontrolü', () => {
  it('surfaces what the television is playing, and only then', async () => {
    const transport = new StubTransport(stubResponses(fx.kodiIdle))
    renderApp(transport)
    await screen.findByRole('button', { name: 'MediaBox ayarları' })

    expect(screen.queryByRole('button', { name: "TV'de oynatılanı göster" })).toBeNull()
  })

  it('opens the now-playing panel from the pill', async () => {
    renderApp(new StubTransport(stubResponses()))

    fireEvent.click(await screen.findByRole('button', { name: "TV'de oynatılanı göster" }))

    expect(await screen.findByRole('dialog', { name: "TV'de oynatılıyor" })).toBeDefined()
    expect(screen.getByRole('button', { name: 'Duraklat' })).toBeDefined()
  })

  it('sends playback commands to the backend, not to Kodi directly', async () => {
    const transport = new StubTransport(stubResponses())
    renderApp(transport)
    fireEvent.click(await screen.findByRole('button', { name: "TV'de oynatılanı göster" }))
    fireEvent.click(await screen.findByRole('button', { name: 'Duraklat' }))

    await waitFor(() =>
      expect(transport.calledPaths('POST')).toContain(ENDPOINTS.kodiPlayPause),
    )
    expect(transport.calls.every((call) => !call.path.includes('8080'))).toBe(true)
  })
})

describe('Servis erişilemezliği', () => {
  it('says the control service is unreachable without hiding the media app', async () => {
    const { ApiError } = await import('../src/api/client')
    renderApp(new StubTransport({}, new ApiError('yok', 'health', 0)))

    expect(await screen.findByText('MediaBox servisine ulaşılamıyor')).toBeDefined()
    expect(screen.getByRole('button', { name: 'Yeniden dene' })).toBeDefined()
  })
})
