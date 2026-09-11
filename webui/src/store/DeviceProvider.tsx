import {
  createContext,
  useContext,
  useEffect,
  useMemo,
  useSyncExternalStore,
  type ReactNode,
} from 'react'
import { ApiClient, HttpTransport, type Transport } from '../api/client'
import { MockTransport } from '../mocks/mockTransport'
import type { ScenarioName } from '../mocks/fixtures'
import { DeviceStore, type DeviceState } from './deviceStore'

interface DeviceContextValue {
  store: DeviceStore
  api: ApiClient
  mock: boolean
}

const DeviceContext = createContext<DeviceContextValue | null>(null)

/** `?mock=1` or `VITE_MOCK=1` swaps in the in-memory backend. */
export function shouldUseMock(search = typeof location === 'undefined' ? '' : location.search) {
  const params = new URLSearchParams(search)
  if (params.has('mock')) return params.get('mock') !== '0'
  return import.meta.env?.VITE_MOCK === '1'
}

export function scenarioFromSearch(
  search = typeof location === 'undefined' ? '' : location.search,
): ScenarioName {
  const value = new URLSearchParams(search).get('scenario')
  const allowed: ScenarioName[] = ['playing', 'idle', 'kodi-offline', 'backend-down']
  return allowed.includes(value as ScenarioName) ? (value as ScenarioName) : 'playing'
}

export function createTransport(): { transport: Transport; mock: boolean } {
  if (shouldUseMock()) {
    return { transport: new MockTransport(scenarioFromSearch()), mock: true }
  }
  return { transport: new HttpTransport(), mock: false }
}

export function DeviceProvider({
  children,
  transport,
  mock,
}: {
  children: ReactNode
  /** Injected by tests and by mock mode; defaults to the real HTTP transport. */
  transport?: Transport
  mock?: boolean
}) {
  const value = useMemo<DeviceContextValue>(() => {
    const resolved = transport ? { transport, mock: mock ?? false } : createTransport()
    const api = new ApiClient(resolved.transport)
    return { store: new DeviceStore(api), api, mock: resolved.mock }
  }, [transport, mock])

  useEffect(() => {
    value.store.start()
    return () => value.store.stop()
  }, [value])

  return <DeviceContext.Provider value={value}>{children}</DeviceContext.Provider>
}

export function useDevice(): DeviceContextValue {
  const context = useContext(DeviceContext)
  if (!context) throw new Error('useDevice must be used inside <DeviceProvider>')
  return context
}

/** Subscribes the calling component to the single central snapshot. */
export function useDeviceState(): DeviceState {
  const { store } = useDevice()
  return useSyncExternalStore(store.subscribe, store.getSnapshot, store.getSnapshot)
}
