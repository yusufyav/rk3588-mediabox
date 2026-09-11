import { useCallback, useEffect, useState } from 'react'

export const ROUTES = ['home', 'player', 'device', 'network', 'settings', 'stremio'] as const
export type RouteId = (typeof ROUTES)[number]

export const ROUTE_LABELS: Record<RouteId, string> = {
  home: 'Genel Bakış',
  player: 'Oynatıcı',
  device: 'Cihaz',
  network: 'Ağ',
  settings: 'Ayarlar',
  stremio: 'Stremio',
}

function parse(hash: string): RouteId {
  const id = hash.replace(/^#\/?/, '').split('?')[0]
  return (ROUTES as readonly string[]).includes(id) ? (id as RouteId) : 'home'
}

/**
 * Hash routing keeps the bundle servable from any static mount point: mediaboxd
 * never has to rewrite unknown paths back to index.html.
 */
export function useRoute(): [RouteId, (route: RouteId) => void] {
  const [route, setRoute] = useState<RouteId>(() =>
    parse(typeof location === 'undefined' ? '' : location.hash),
  )

  useEffect(() => {
    const onHashChange = () => setRoute(parse(location.hash))
    window.addEventListener('hashchange', onHashChange)
    return () => window.removeEventListener('hashchange', onHashChange)
  }, [])

  const navigate = useCallback((next: RouteId) => {
    if (typeof location !== 'undefined') location.hash = `#/${next}`
    setRoute(next)
  }, [])

  return [route, navigate]
}
