import { useEffect } from 'react'
import { Button } from './components/Button'
import { Shell } from './components/Shell'
import { Banner } from './components/Status'
import { useLayoutClass } from './nav/useLayoutClass'
import { BACK_EVENT, useRemoteNavigation } from './nav/useRemoteNavigation'
import { useRoute } from './nav/router'
import { Device } from './pages/Device'
import { Home } from './pages/Home'
import { Network } from './pages/Network'
import { Player } from './pages/Player'
import { Settings } from './pages/Settings'
import { Stremio } from './pages/Stremio'
import { useDevice, useDeviceState } from './store/DeviceProvider'

export function App() {
  const [route, navigate] = useRoute()
  const { store, mock } = useDevice()
  const state = useDeviceState()

  useLayoutClass()
  useRemoteNavigation()

  // Back from anywhere returns to the dashboard; an open dialog consumes the
  // event first, so this only fires when nothing is layered on top.
  useEffect(() => {
    const onBack = () => {
      if (document.querySelector('[data-nav-scope="dialog"]')) return
      if (route !== 'home') navigate('home')
    }
    window.addEventListener(BACK_EVENT, onBack)
    return () => window.removeEventListener(BACK_EVENT, onBack)
  }, [route, navigate])

  const connection = describeConnection(state.backendReachable, state.streamConnected)

  return (
    <Shell
      route={route}
      onNavigate={navigate}
      onRefresh={() => void store.refreshAll()}
      connectionLabel={connection.label}
      connectionTone={connection.tone}
      streamLabel={state.streamConnected ? 'Olay akışı' : 'Yoklama'}
      mock={mock}
    >
      {!state.backendReachable && (
        <Banner
          title="MediaBox servisine ulaşılamıyor"
          detail="Gösterilen değerler son başarılı okumaya aittir. Bağlantı kurulunca otomatik güncellenir."
          action={
            <Button variant="quiet" onClick={() => void store.refreshAll()}>
              Yeniden dene
            </Button>
          }
        />
      )}

      {renderRoute(route)}
    </Shell>
  )
}

function renderRoute(route: string) {
  switch (route) {
    case 'player':
      return <Player />
    case 'device':
      return <Device />
    case 'network':
      return <Network />
    case 'settings':
      return <Settings />
    case 'stremio':
      return <Stremio />
    default:
      return <Home />
  }
}

function describeConnection(
  reachable: boolean,
  streaming: boolean,
): { label: string; tone: 'ok' | 'warn' | 'danger' } {
  if (!reachable) return { label: 'Bağlantı yok', tone: 'danger' }
  if (streaming) return { label: 'Canlı', tone: 'ok' }
  return { label: 'Yoklama', tone: 'warn' }
}
