import { describe, expect, it } from 'vitest'
import { render, screen } from '@testing-library/react'
import { App } from '../src/App'
import { MockTransport } from '../src/mocks/mockTransport'
import { ENDPOINTS } from '../src/api/endpoints'
import { ApiClient } from '../src/api/client'
import { DeviceProvider, scenarioFromSearch, shouldUseMock } from '../src/store/DeviceProvider'

/** `push: false` keeps the fixture clock out of the test run. */
function mock(scenario: Parameters<typeof scenarioFromSearch> extends never ? never : string) {
  return new MockTransport(scenario as never, false)
}

function renderMock(scenario: string) {
  return render(
    <DeviceProvider transport={mock(scenario)} mock>
      <App />
    </DeviceProvider>,
  )
}

describe('Mock mod seçimi', () => {
  it('reads the mode and scenario from the query string', () => {
    expect(shouldUseMock('?mock=1')).toBe(true)
    expect(shouldUseMock('?mock=0')).toBe(false)
    expect(shouldUseMock('')).toBe(false)
    expect(scenarioFromSearch('?scenario=kodi-offline')).toBe('kodi-offline')
    expect(scenarioFromSearch('?scenario=nonsense')).toBe('playing')
  })
})

describe('Mock senaryoları', () => {
  it('serves a playing box and marks the UI as mock', async () => {
    renderMock('playing')

    expect(await screen.findByText('Mock mod')).toBeDefined()
    expect((await screen.findAllByText(/Blade Runner 2049/)).length).toBeGreaterThan(0)
  })

  it('serves an idle box', async () => {
    renderMock('idle')

    expect(await screen.findByRole('button', { name: 'Oynat' })).toBeDefined()
    expect(screen.getByText('Oynatılan içerik yok')).toBeDefined()
  })

  it('serves a box whose Kodi is down', async () => {
    renderMock('kodi-offline')

    expect(await screen.findByText(/Kodi çalışmıyor/)).toBeDefined()
  })

  it('serves an unreachable backend and shows the service error', async () => {
    renderMock('backend-down')

    expect(await screen.findByText('MediaBox servisine ulaşılamıyor')).toBeDefined()
    expect(screen.getByRole('button', { name: 'Yeniden dene' })).toBeDefined()
  })
})

describe('Mock backend davranışı', () => {
  it('moves the playback position when a seek is issued', async () => {
    const api = new ApiClient(mock('playing'))
    const before = (await api.getKodi()).player?.position ?? 0

    await api.seek(before - 30)

    expect((await api.getKodi()).player?.position).toBe(before - 30)
  })

  it('refuses playback commands while Kodi is offline', async () => {
    const api = new ApiClient(mock('kodi-offline'))

    await expect(api.playPause()).rejects.toThrow('Kodi çalışmıyor')
  })

  it('reflects a service stop in the reported state', async () => {
    const transport = mock('playing')
    const api = new ApiClient(transport)

    await api.stopKodi()

    expect((await api.getKodi()).state).toBe('offline')
    expect(transport).toBeInstanceOf(MockTransport)
    expect(ENDPOINTS.kodiServiceStop).toBe('kodi/stop-service')
  })
})
