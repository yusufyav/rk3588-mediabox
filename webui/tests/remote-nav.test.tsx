import { describe, expect, it, vi } from 'vitest'
import { fireEvent, render } from '@testing-library/react'
import { directionForKey, isBackKey, resolveNextFocus, type Candidate } from '../src/nav/spatial'
import { BACK_EVENT, FOCUSABLE_SELECTOR, useRemoteNavigation } from '../src/nav/useRemoteNavigation'

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

describe('Medya uygulamasıyla ortak odak ağacı', () => {
  it('also treats the media app\'s own controls as reachable', () => {
    // Stremio marks its controls the ordinary way; from the sofa there is one
    // UI, so both kinds have to be candidates.
    const media = document.createElement('div')
    media.setAttribute('tabindex', '0')
    media.textContent = 'Poster'
    document.body.appendChild(media)
    const shellControl = document.createElement('button')
    shellControl.setAttribute('data-focusable', '')
    document.body.appendChild(shellControl)

    const matches = Array.from(document.querySelectorAll(FOCUSABLE_SELECTOR))

    expect(matches).toContain(media)
    expect(matches).toContain(shellControl)
    media.remove()
    shellControl.remove()
  })

  it('does not offer controls that are switched off', () => {
    const disabled = document.createElement('button')
    disabled.disabled = true
    document.body.appendChild(disabled)
    const skipped = document.createElement('div')
    skipped.setAttribute('tabindex', '-1')
    document.body.appendChild(skipped)

    const matches = Array.from(document.querySelectorAll(FOCUSABLE_SELECTOR))

    expect(matches).not.toContain(disabled)
    expect(matches).not.toContain(skipped)
    disabled.remove()
    skipped.remove()
  })
})

describe('Kumandanın sahipliği', () => {
  function MediaRouteHarness() {
    useRemoteNavigation({
      getRect: () => rect(0, 0),
      isMediaKeyRoute: () => true,
    })
    return (
      <button type="button" data-focusable="">
        A
      </button>
    )
  }

  it('leaves the arrow keys to the media app while it is playing', () => {
    // On the Stremio player, arrows seek. Taking them over would break scrubbing.
    render(<MediaRouteHarness />)
    ;(document.activeElement as HTMLElement | null)?.blur()
    const event = new KeyboardEvent('keydown', { key: 'ArrowRight', cancelable: true })

    document.dispatchEvent(event)

    expect(event.defaultPrevented).toBe(false)
    expect(document.activeElement).toBe(document.body)
  })
})

describe('Geri tuşu', () => {
  it('is left to the media app when the shell has nothing open', () => {
    render(<Harness onActivate={() => {}} />)
    const event = new KeyboardEvent('keydown', { key: 'Escape', cancelable: true })

    document.dispatchEvent(event)

    // Nothing consumed the back event, so the key is not claimed.
    expect(event.defaultPrevented).toBe(false)
  })

  it('is claimed when the shell consumes it', () => {
    render(<Harness onActivate={() => {}} />)
    const consume = (custom: Event) => custom.preventDefault()
    window.addEventListener(BACK_EVENT, consume)
    const event = new KeyboardEvent('keydown', { key: 'Escape', cancelable: true })

    document.dispatchEvent(event)

    expect(event.defaultPrevented).toBe(true)
    window.removeEventListener(BACK_EVENT, consume)
  })
})

describe('Yazı alanından çıkış', () => {
  function SearchHarness() {
    useRemoteNavigation({ getRect: (el) => rect(0, el.tagName === 'INPUT' ? 0 : 100) })
    return (
      <div>
        <input type="search" defaultValue="ara" />
        <button type="button" data-focusable="">
          Poster
        </button>
      </div>
    )
  }

  it('lets a remote leave a focused search box', () => {
    // The media app opens with its search box focused; a remote has no other
    // way out of it.
    render(<SearchHarness />)
    const input = document.querySelector('input') as HTMLInputElement
    input.focus()

    fireEvent.keyDown(document, { key: 'ArrowDown' })

    expect(document.activeElement?.textContent).toBe('Poster')
  })

  it('leaves left and right to the caret', () => {
    render(<SearchHarness />)
    const input = document.querySelector('input') as HTMLInputElement
    input.focus()
    const event = new KeyboardEvent('keydown', { key: 'ArrowRight', cancelable: true })

    document.dispatchEvent(event)

    expect(document.activeElement).toBe(input)
    expect(event.defaultPrevented).toBe(false)
  })
})
