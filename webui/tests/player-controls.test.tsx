import { describe, expect, it } from 'vitest'
import { fireEvent, screen, waitFor } from '@testing-library/react'
import { renderApp, StubTransport, stubResponses } from './helpers'
import { ENDPOINTS } from '../src/api/endpoints'

async function openPlayerPage(transport: StubTransport) {
  renderApp(transport)
  fireEvent.click(await screen.findByRole('button', { name: 'Oynatıcı' }))
  return screen.findByRole('button', { name: 'Duraklat' })
}

describe('Oynatıcı kontrolleri', () => {
  it('sends play/pause to the contract endpoint', async () => {
    const transport = new StubTransport(stubResponses())
    fireEvent.click(await openPlayerPage(transport))

    await waitFor(() => expect(transport.calledPaths('POST')).toContain(ENDPOINTS.kodiPlayPause))
  })

  it('sends stop to the playback endpoint, not the service endpoint', async () => {
    const transport = new StubTransport(stubResponses())
    await openPlayerPage(transport)
    fireEvent.click(screen.getByRole('button', { name: 'Durdur' }))

    await waitFor(() => expect(transport.calledPaths('POST')).toContain(ENDPOINTS.kodiStop))
    expect(transport.calledPaths('POST')).not.toContain(ENDPOINTS.kodiServiceStop)
  })

  it.each([
    ['Geri 30 saniye', -30],
    ['Geri 10 saniye', -10],
    ['İleri 10 saniye', 10],
    ['İleri 30 saniye', 30],
  ])('sends %s as a relative seek offset', async (label, offset) => {
    const transport = new StubTransport(stubResponses())
    await openPlayerPage(transport)
    fireEvent.click(screen.getByRole('button', { name: label }))

    await waitFor(() => {
      const seek = transport.calls.find((c) => c.path === ENDPOINTS.kodiSeek)
      expect(seek?.body).toEqual({ offsetSeconds: offset })
    })
  })

  it('re-reads Kodi state after a command so nothing stays optimistic', async () => {
    const transport = new StubTransport(stubResponses())
    await openPlayerPage(transport)
    const before = transport.calledPaths('GET').filter((p) => p === ENDPOINTS.kodi).length

    fireEvent.click(screen.getByRole('button', { name: 'Durdur' }))

    await waitFor(() => {
      const after = transport.calledPaths('GET').filter((p) => p === ENDPOINTS.kodi).length
      expect(after).toBeGreaterThan(before)
    })
  })

  it('surfaces a failed command instead of failing silently', async () => {
    const transport = new StubTransport(stubResponses())
    await openPlayerPage(transport)
    transport.request = async () => {
      throw new Error('Kodi JSON-RPC yanıt vermedi')
    }

    fireEvent.click(screen.getByRole('button', { name: 'Durdur' }))

    expect(await screen.findByText('Komut başarısız')).toBeDefined()
    expect(screen.getByText('Kodi JSON-RPC yanıt vermedi')).toBeDefined()
  })

  it('renders the playback clock and progress from the reported position', async () => {
    await openPlayerPage(new StubTransport(stubResponses()))

    const progress = screen.getByRole('progressbar', { name: 'Oynatma ilerlemesi' })
    expect(progress.getAttribute('aria-valuenow')).toBe('1284')
    expect(progress.getAttribute('aria-valuemax')).toBe('9780')
    expect(screen.getAllByText('21:24').length).toBeGreaterThan(0)
    expect(screen.getAllByText('2:43:00').length).toBeGreaterThan(0)
  })
})
