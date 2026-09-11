import { describe, expect, it } from 'vitest'
import { screen, waitFor } from '@testing-library/react'
import { classifyViewport } from '../src/nav/useLayoutClass'
import { renderApp, setViewport, StubTransport, stubResponses } from './helpers'

describe('Yerleşim sınıfları', () => {
  it('classifies the four supported form factors', () => {
    expect(classifyViewport(1920, 1080)).toBe('tv')
    expect(classifyViewport(3840, 2160)).toBe('tv')
    expect(classifyViewport(1440, 900)).toBe('desktop')
    // A wide but short desktop window is not a TV: it is read from arm's length.
    expect(classifyViewport(1280, 800)).toBe('desktop')
    expect(classifyViewport(834, 1112)).toBe('tablet')
    expect(classifyViewport(390, 844)).toBe('mobile')
  })

  it('treats a tall 1600px window as desktop rather than TV', () => {
    expect(classifyViewport(1600, 1200)).toBe('desktop')
    expect(classifyViewport(1600, 900)).toBe('tv')
  })
})

describe('Mobil yerleşim', () => {
  it('renders the full dashboard on a phone viewport', async () => {
    setViewport(390, 844)
    renderApp(new StubTransport(stubResponses()))

    await waitFor(() => expect(document.documentElement.dataset.layout).toBe('mobile'))

    // Navigation, status and telemetry all survive the collapse to one column.
    expect(screen.getByRole('button', { name: 'Ayarlar' })).toBeDefined()
    expect(screen.getByText('48.6 °C')).toBeDefined()
    expect(screen.getAllByText('Oynatılıyor').length).toBeGreaterThan(0)
    expect(screen.getByRole('button', { name: 'Duraklat' })).toBeDefined()
  })

  it('switches to the TV class when the viewport grows', async () => {
    setViewport(390, 844)
    renderApp(new StubTransport(stubResponses()))
    await waitFor(() => expect(document.documentElement.dataset.layout).toBe('mobile'))

    setViewport(1920, 1080)

    await waitFor(() => expect(document.documentElement.dataset.layout).toBe('tv'))
    expect(screen.getByRole('heading', { name: 'Genel Bakış', level: 1 })).toBeDefined()
  })
})
