import { describe, expect, it } from 'vitest'
import { screen } from '@testing-library/react'
import { renderApp, StubTransport, stubResponses } from './helpers'
import * as fx from '../src/mocks/fixtures'

describe('Kodi kapalı durumu', () => {
  it('states plainly that Kodi is down instead of showing dead controls', async () => {
    renderApp(new StubTransport(stubResponses(fx.kodiOffline)))

    expect(await screen.findByText(/Kodi çalışmıyor/)).toBeDefined()
    expect(screen.getAllByText('Kodi kapalı').length).toBeGreaterThan(0)
    expect(screen.queryByRole('button', { name: 'Duraklat' })).toBeNull()
    expect(screen.queryByRole('button', { name: '+30s' })).toBeNull()
  })

  it('points the user at the Settings controls that can recover it', async () => {
    renderApp(new StubTransport(stubResponses(fx.kodiOffline)))

    expect(await screen.findByText(/Ayarlar sayfasından başlatabilirsiniz/)).toBeDefined()
  })

  it('disables transport controls when Kodi is running but idle', async () => {
    renderApp(new StubTransport(stubResponses(fx.kodiIdle)))

    const play = await screen.findByRole('button', { name: 'Oynat' })
    expect(play.hasAttribute('disabled')).toBe(true)
    expect(play.getAttribute('title')).toBe('Oynatılan içerik yok')
    expect(screen.getByText('Oynatılan içerik yok')).toBeDefined()
  })
})
