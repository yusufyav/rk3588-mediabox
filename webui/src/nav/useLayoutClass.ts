import { useEffect, useState } from 'react'

export type LayoutClass = 'tv' | 'desktop' | 'tablet' | 'mobile'

/**
 * A TV is a wide, 16:9-ish viewport viewed from across the room, so it gets the
 * largest type scale rather than the most information. `?layout=tv` forces the
 * class for kiosk browsers that report an unhelpful viewport.
 */
export function classifyViewport(width: number, height: number): LayoutClass {
  const aspect = height > 0 ? width / height : 16 / 9
  if (width >= 1600 && aspect >= 1.6) return 'tv'
  if (width >= 1024) return 'desktop'
  if (width >= 640) return 'tablet'
  return 'mobile'
}

function forcedLayout(search: string): LayoutClass | undefined {
  const value = new URLSearchParams(search).get('layout')
  const allowed: LayoutClass[] = ['tv', 'desktop', 'tablet', 'mobile']
  return allowed.includes(value as LayoutClass) ? (value as LayoutClass) : undefined
}

export function useLayoutClass(): LayoutClass {
  const [layout, setLayout] = useState<LayoutClass>(() => read())

  useEffect(() => {
    const onResize = () => setLayout(read())
    window.addEventListener('resize', onResize)
    return () => window.removeEventListener('resize', onResize)
  }, [])

  useEffect(() => {
    document.documentElement.dataset.layout = layout
  }, [layout])

  return layout
}

function read(): LayoutClass {
  if (typeof window === 'undefined') return 'desktop'
  return forcedLayout(window.location.search) ?? classifyViewport(window.innerWidth, window.innerHeight)
}
