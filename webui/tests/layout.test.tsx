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
  it('keeps the appliance controls usable on a phone', async () => {
    setViewport(390, 844)
    renderApp(new StubTransport(stubResponses()))

    await waitFor(() => expect(document.documentElement.dataset.layout).toBe('mobile'))

    // The same shell, on a touch screen: settings and TV control both reachable.
    expect(screen.getByRole('button', { name: 'MediaBox ayarları' })).toBeDefined()
    expect(screen.getByRole('button', { name: "TV'de oynatılanı göster" })).toBeDefined()
  })

  it('carries the layout class onto the shell so TV spacing is scoped to it', async () => {
    setViewport(1920, 1080)
    renderApp(new StubTransport(stubResponses()))

    const shell = await waitFor(() => {
      const found = document.querySelector('[data-mediabox-shell]')
      expect(found).not.toBeNull()
      return found as HTMLElement
    })

    // The class lives on the shell, not only on <html>: the media app sharing
    // this document must not inherit the appliance's type scale.
    await waitFor(() => expect(shell.dataset.layout).toBe('tv'))
  })

  it('switches class when the viewport grows', async () => {
    setViewport(390, 844)
    renderApp(new StubTransport(stubResponses()))
    await waitFor(() => expect(document.documentElement.dataset.layout).toBe('mobile'))

    setViewport(1920, 1080)

    await waitFor(() => expect(document.documentElement.dataset.layout).toBe('tv'))
  })
})
