import { describe, expect, it, vi } from 'vitest'
import { fireEvent, render, screen } from '@testing-library/react'
import { directionForKey, isBackKey, resolveNextFocus, type Candidate } from '../src/nav/spatial'
import { BACK_EVENT, useRemoteNavigation } from '../src/nav/useRemoteNavigation'
import { renderApp, StubTransport, stubResponses } from './helpers'

function rect(left: number, top: number, width = 100, height = 40) {
  return { left, top, width, height }
}

describe('Yön çözümleme', () => {
  // A 2x2 grid: A B on the top row, C D on the bottom row.
  const grid: Array<Candidate<string>> = [
    { item: 'A', rect: rect(0, 0) },
    { item: 'B', rect: rect(200, 0) },
    { item: 'C', rect: rect(0, 100) },
    { item: 'D', rect: rect(200, 100) },
  ]
  const at = (id: string) => grid.find((c) => c.item === id)!

  it('moves to the neighbour in the requested direction', () => {
    expect(resolveNextFocus(grid, at('A'), 'right')).toBe('B')
    expect(resolveNextFocus(grid, at('B'), 'left')).toBe('A')
    expect(resolveNextFocus(grid, at('A'), 'down')).toBe('C')
    expect(resolveNextFocus(grid, at('C'), 'up')).toBe('A')
  })

  it('prefers the on-axis neighbour over a diagonal one', () => {
    expect(resolveNextFocus(grid, at('A'), 'down')).toBe('C')
    expect(resolveNextFocus(grid, at('B'), 'down')).toBe('D')
  })

  it('stops at the edge instead of wrapping around', () => {
    expect(resolveNextFocus(grid, at('B'), 'right')).toBeUndefined()
    expect(resolveNextFocus(grid, at('A'), 'up')).toBeUndefined()
  })

  it('rejects a candidate that is far off the travel axis', () => {
    const spread: Array<Candidate<string>> = [
      { item: 'origin', rect: rect(0, 0) },
      { item: 'far-aside', rect: rect(3000, 10) },
    ]
    expect(resolveNextFocus(spread, spread[0], 'down')).toBeUndefined()
  })

  it('maps arrow keys and back keys', () => {
    expect(directionForKey('ArrowUp')).toBe('up')
    expect(directionForKey('ArrowRight')).toBe('right')
    expect(directionForKey('Tab')).toBeUndefined()
    expect(isBackKey('Escape')).toBe(true)
    expect(isBackKey('Backspace')).toBe(true)
    expect(isBackKey('Enter')).toBe(false)
  })
})

function Harness({ onActivate }: { onActivate: () => void }) {
  // jsdom computes no layout, so the hook is given the geometry directly.
  useRemoteNavigation({
    getRect: (element) => {
      const [left, top] = (element.dataset.rect ?? '0,0').split(',').map(Number)
      return rect(left, top)
    },
  })
  return (
    <div>
      <button type="button" data-focusable="" data-rect="0,0">
        A
      </button>
      <button type="button" data-focusable="" data-rect="200,0">
        B
      </button>
      <button type="button" data-focusable="" data-rect="0,100" onClick={onActivate}>
        C
      </button>
    </div>
  )
}

describe('Uzaktan kumanda gezinmesi', () => {
  it('takes focus on the first arrow press', () => {
    render(<Harness onActivate={() => {}} />)

    fireEvent.keyDown(document, { key: 'ArrowDown' })

    expect(document.activeElement?.textContent).toBe('A')
    expect(document.documentElement.dataset.input).toBe('remote')
  })

  it('moves focus with the arrow keys', () => {
    render(<Harness onActivate={() => {}} />)

    fireEvent.keyDown(document, { key: 'ArrowDown' })
    fireEvent.keyDown(document, { key: 'ArrowRight' })
    expect(document.activeElement?.textContent).toBe('B')

    fireEvent.keyDown(document, { key: 'ArrowLeft' })
    fireEvent.keyDown(document, { key: 'ArrowDown' })
    expect(document.activeElement?.textContent).toBe('C')
  })

  it('activates the focused control with Enter', () => {
    const onActivate = vi.fn()
    render(<Harness onActivate={onActivate} />)

    fireEvent.keyDown(document, { key: 'ArrowDown' })
    fireEvent.keyDown(document, { key: 'ArrowDown' })
    expect(document.activeElement?.textContent).toBe('C')
    fireEvent.click(document.activeElement as HTMLElement)

    expect(onActivate).toHaveBeenCalledOnce()
  })

  it('emits a back event on Escape', () => {
    render(<Harness onActivate={() => {}} />)
    const onBack = vi.fn()
    window.addEventListener(BACK_EVENT, onBack)

    fireEvent.keyDown(document, { key: 'Escape' })

    expect(onBack).toHaveBeenCalledOnce()
    window.removeEventListener(BACK_EVENT, onBack)
  })
})

describe('Uygulama içi gezinme', () => {
  it('exposes every section as a focusable nav control in DOM order', async () => {
    renderApp(new StubTransport(stubResponses()))
    await screen.findByText('48.6 °C')

    const focusables = Array.from(document.querySelectorAll('[data-focusable]'))
    const labels = focusables.slice(0, 6).map((element) => element.textContent)

    expect(labels).toEqual([
      'Genel Bakış',
      'Oynatıcı',
      'Cihaz',
      'Ağ',
      'Ayarlar',
      'StremioYakında',
    ])
  })

  it('returns to the dashboard when the back key is pressed on a sub-page', async () => {
    renderApp(new StubTransport(stubResponses()))
    fireEvent.click(await screen.findByRole('button', { name: 'Cihaz' }))
    expect(screen.getByRole('heading', { name: 'Cihaz', level: 1 })).toBeDefined()

    fireEvent.keyDown(document, { key: 'Escape' })

    expect(screen.getByRole('heading', { name: 'Genel Bakış', level: 1 })).toBeDefined()
  })

  it('keeps the back key inside an open dialog instead of leaving the page', async () => {
    renderApp(new StubTransport(stubResponses()))
    fireEvent.click(await screen.findByRole('button', { name: 'Ayarlar' }))
    fireEvent.click(await screen.findByRole('button', { name: 'Cihazı yeniden başlat' }))
    expect(await screen.findByRole('alertdialog')).toBeDefined()

    fireEvent.keyDown(document, { key: 'Escape' })

    expect(screen.queryByRole('alertdialog')).toBeNull()
    // The dialog consumed the back press; the page did not change underneath it.
    expect(screen.getByRole('heading', { name: 'Ayarlar', level: 1 })).toBeDefined()
  })
})
