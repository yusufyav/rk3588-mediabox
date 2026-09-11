import { describe, expect, it } from 'vitest'
import { screen, waitFor } from '@testing-library/react'
import { renderApp, StubTransport, stubResponses } from './helpers'
import * as fx from '../src/mocks/fixtures'
import { formatBytes } from '../src/lib/format'

describe('Genel Bakış', () => {
  it('renders device telemetry from a single central fetch pass', async () => {
    const transport = new StubTransport(stubResponses())
    renderApp(transport)

    // The title shows twice by design: the hero headline and the player card.
    expect((await screen.findAllByText('Blade Runner 2049 (2017) — 4K HDR10')).length).toBe(2)
    expect(screen.getByText('48.6 °C')).toBeDefined()
    expect(screen.getAllByText('Oynatılıyor').length).toBeGreaterThan(0)
    expect(screen.getByText('192.168.1.42')).toBeDefined()
    expect(screen.getByText('HDMI-A-1')).toBeDefined()

    // Every resource is read exactly once on mount: no component owns a timer.
    const gets = transport.calledPaths('GET')
    expect(gets.filter((p) => p === 'system')).toHaveLength(1)
    expect(gets.filter((p) => p === 'kodi')).toHaveLength(1)
  })

  it('shows no connection warning while the backend answers', async () => {
    renderApp(new StubTransport(stubResponses()))
    await screen.findByText('48.6 °C')

    expect(screen.queryByText('MediaBox servisine ulaşılamıyor')).toBeNull()
  })

  it('formats uptime and memory usage for across-the-room reading', async () => {
    renderApp(new StubTransport(stubResponses()))

    await waitFor(() => expect(screen.getByText(/Çalışma süresi 2 gün/)).toBeDefined())
    const used = formatBytes(fx.system.memory!.usedBytes)
    const total = formatBytes(fx.system.memory!.totalBytes)
    expect(screen.getByText(`${used} / ${total}`)).toBeDefined()
  })
})
