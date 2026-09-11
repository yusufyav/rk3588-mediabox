export type Direction = 'up' | 'down' | 'left' | 'right'

export interface Rect {
  left: number
  top: number
  width: number
  height: number
}

export interface Candidate<T> {
  item: T
  rect: Rect
}

function center(rect: Rect) {
  return { x: rect.left + rect.width / 2, y: rect.top + rect.height / 2 }
}

/**
 * Geometric focus resolution for D-pad / arrow-key navigation.
 *
 * Kept free of DOM APIs so it can be unit-tested: callers supply rectangles.
 * The winner is the closest candidate that actually lies in the requested
 * direction, with movement across the travel axis penalised so a press of
 * "down" does not jump sideways across the screen.
 */
export function resolveNextFocus<T>(
  candidates: Array<Candidate<T>>,
  current: Candidate<T>,
  direction: Direction,
): T | undefined {
  const from = center(current.rect)
  const horizontal = direction === 'left' || direction === 'right'
  const sign = direction === 'right' || direction === 'down' ? 1 : -1

  let best: T | undefined
  let bestScore = Number.POSITIVE_INFINITY

  for (const candidate of candidates) {
    if (candidate.item === current.item) continue
    const to = center(candidate.rect)
    const along = horizontal ? (to.x - from.x) * sign : (to.y - from.y) * sign
    const across = horizontal ? Math.abs(to.y - from.y) : Math.abs(to.x - from.x)

    // Must make real progress along the travel axis, and must not be so far
    // off-axis that the move reads as a random jump.
    if (along <= 1) continue
    if (across > along * 4 + 120) continue

    const score = along + across * 2
    if (score < bestScore) {
      bestScore = score
      best = candidate.item
    }
  }

  return best
}

export function directionForKey(key: string): Direction | undefined {
  switch (key) {
    case 'ArrowUp':
      return 'up'
    case 'ArrowDown':
      return 'down'
    case 'ArrowLeft':
      return 'left'
    case 'ArrowRight':
      return 'right'
    default:
      return undefined
  }
}

/** Keys that mean "go back" on a remote or keyboard. */
export function isBackKey(key: string): boolean {
  return key === 'Escape' || key === 'Backspace' || key === 'GoBack'
}
