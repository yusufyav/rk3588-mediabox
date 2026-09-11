import { useEffect } from 'react'
import { directionForKey, isBackKey, resolveNextFocus, type Candidate, type Rect } from './spatial'

export const BACK_EVENT = 'mediabox:back'
export const FOCUSABLE_SELECTOR = '[data-focusable]:not([disabled]):not([aria-disabled="true"])'

function defaultGetRect(element: HTMLElement): Rect {
  const r = element.getBoundingClientRect()
  return { left: r.left, top: r.top, width: r.width, height: r.height }
}

export interface RemoteNavigationOptions {
  /** Overridable so tests can supply layout geometry jsdom will not compute. */
  getRect?: (element: HTMLElement) => Rect
  root?: () => ParentNode
}

/**
 * Arrow keys move focus, Enter activates, Escape/Backspace goes back.
 *
 * Navigation is scoped to an open dialog when one is present so a remote cannot
 * drive a control hidden behind it; the dialog always stays escapable with the
 * back key, so this scoping never becomes an inescapable focus trap.
 */
export function useRemoteNavigation(options: RemoteNavigationOptions = {}) {
  const { getRect = defaultGetRect, root = () => document } = options

  useEffect(() => {
    function collect(): HTMLElement[] {
      const scope = root()
      const dialog = (scope as ParentNode).querySelector<HTMLElement>('[data-nav-scope="dialog"]')
      const container: ParentNode = dialog ?? scope
      return Array.from(container.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
        (element) => !element.hasAttribute('hidden'),
      )
    }

    function onKeyDown(event: KeyboardEvent) {
      if (event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey) return

      if (isBackKey(event.key)) {
        const target = event.target as HTMLElement | null
        // Backspace must keep editing text; Escape still works everywhere.
        if (event.key === 'Backspace' && isTextEntry(target)) return
        event.preventDefault()
        window.dispatchEvent(new CustomEvent(BACK_EVENT))
        return
      }

      const direction = directionForKey(event.key)
      if (!direction) return
      if (isTextEntry(event.target as HTMLElement | null)) return

      document.documentElement.dataset.input = 'remote'

      const elements = collect()
      if (elements.length === 0) return

      const active = document.activeElement as HTMLElement | null
      const currentElement = active && elements.includes(active) ? active : undefined

      if (!currentElement) {
        event.preventDefault()
        elements[0].focus()
        return
      }

      const candidates: Array<Candidate<HTMLElement>> = elements.map((element) => ({
        item: element,
        rect: getRect(element),
      }))
      const current = candidates.find((candidate) => candidate.item === currentElement)!
      const next = resolveNextFocus(candidates, current, direction)
      if (next) {
        event.preventDefault()
        next.focus()
      }
    }

    function onPointerDown() {
      document.documentElement.dataset.input = 'pointer'
    }

    document.addEventListener('keydown', onKeyDown)
    document.addEventListener('pointerdown', onPointerDown)
    return () => {
      document.removeEventListener('keydown', onKeyDown)
      document.removeEventListener('pointerdown', onPointerDown)
    }
  }, [getRect, root])
}

function isTextEntry(element: HTMLElement | null): boolean {
  if (!element) return false
  const tag = element.tagName
  return tag === 'INPUT' || tag === 'TEXTAREA' || element.isContentEditable
}
