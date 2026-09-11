import { useEffect } from 'react'
import { directionForKey, isBackKey, resolveNextFocus, type Candidate, type Rect } from './spatial'

export const BACK_EVENT = 'mediabox:back'

/**
 * What a remote can land on.
 *
 * The appliance shell marks its own controls with `data-focusable`, but the
 * media experience sharing this document is upstream Stremio, which marks its
 * controls the ordinary way — `tabindex="0"` on a div, or a real button. Both
 * have to be reachable, because from the sofa there is only one UI.
 */
export const FOCUSABLE_SELECTOR = [
  '[data-focusable]',
  'a[href]',
  'button',
  'input',
  'select',
  'textarea',
  '[tabindex]',
]
  .map((selector) => `${selector}:not([disabled]):not([aria-disabled="true"]):not([tabindex="-1"])`)
  .join(',')

function defaultGetRect(element: HTMLElement): Rect {
  const r = element.getBoundingClientRect()
  return { left: r.left, top: r.top, width: r.width, height: r.height }
}

export interface RemoteNavigationOptions {
  /** Overridable so tests can supply layout geometry jsdom will not compute. */
  getRect?: (element: HTMLElement) => Rect
  root?: () => ParentNode
  /**
   * Routes where the media app owns the arrow keys and must keep them. On the
   * Stremio player, arrows seek; taking them over would break scrubbing.
   */
  isMediaKeyRoute?: () => boolean
}

function defaultIsMediaKeyRoute(): boolean {
  return typeof location !== 'undefined' && location.hash.startsWith('#/player')
}

/**
 * Arrow keys move focus, Enter activates, Escape/Backspace goes back.
 *
 * Navigation is scoped to an open panel when one is present so a remote cannot
 * drive a control hidden behind it; the panel always stays escapable with the
 * back key, so this scoping never becomes an inescapable focus trap.
 */
export function useRemoteNavigation(options: RemoteNavigationOptions = {}) {
  const {
    getRect = defaultGetRect,
    root = () => document,
    isMediaKeyRoute = defaultIsMediaKeyRoute,
  } = options

  useEffect(() => {
    function scope(): { container: ParentNode; scoped: boolean } {
      const node = root()
      const panel = (node as ParentNode).querySelector<HTMLElement>('[data-nav-scope="dialog"]')
      return panel ? { container: panel, scoped: true } : { container: node, scoped: false }
    }

    function collect(container: ParentNode): HTMLElement[] {
      return Array.from(container.querySelectorAll<HTMLElement>(FOCUSABLE_SELECTOR)).filter(
        (element) => !element.hasAttribute('hidden') && isVisible(element, getRect),
      )
    }

    function onKeyDown(event: KeyboardEvent) {
      if (event.defaultPrevented || event.altKey || event.ctrlKey || event.metaKey) return

      if (isBackKey(event.key)) {
        const target = event.target as HTMLElement | null
        // Backspace must keep editing text; Escape still works everywhere.
        if (event.key === 'Backspace' && isTextEntry(target)) return
        const custom = new CustomEvent(BACK_EVENT, { cancelable: true })
        const consumed = !window.dispatchEvent(custom)
        // Only claim the key if the appliance layer actually used it; otherwise
        // the media app gets its own back behaviour.
        if (consumed) event.preventDefault()
        return
      }

      const direction = directionForKey(event.key)
      if (!direction) return
      if (isTextEntry(event.target as HTMLElement | null)) return

      const { container, scoped } = scope()
      if (!scoped && isMediaKeyRoute()) return

      document.documentElement.dataset.input = 'remote'

      const elements = collect(container)
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
  }, [getRect, root, isMediaKeyRoute])
}

/**
 * A media app renders far more elements than it shows. Anything with no box is
 * not somewhere a remote should be able to land.
 */
function isVisible(element: HTMLElement, getRect: (element: HTMLElement) => Rect): boolean {
  const rect = getRect(element)
  return rect.width > 0 && rect.height > 0
}

function isTextEntry(element: HTMLElement | null): boolean {
  if (!element) return false
  const tag = element.tagName
  return tag === 'INPUT' || tag === 'TEXTAREA' || element.isContentEditable
}
