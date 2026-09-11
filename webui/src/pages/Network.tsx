import { Card, Row, Rows } from '../components/Card'
import { Banner, Empty, Skeleton, StaleMark, StatusPill } from '../components/Status'
import { orUnknown } from '../lib/format'
import { CADENCE, isStale } from '../store/deviceStore'
import { useDeviceState } from '../store/DeviceProvider'
import styles from './Pages.module.css'

/** Read-only in M1: the UI reports the link, it does not configure it. */
export function Network() {
  const state = useDeviceState()
  const network = state.network.data
  const interfaces = (network?.interfaces ?? []).filter((item) => item.type !== 'loopback')

  return (
    <div className={styles.stack}>
      {state.network.error && !state.network.error.unreachable && (
        <Banner title="Ağ bilgisi okunamadı" detail={state.network.error.message} tone="warn" />
      )}

      <Card
        title="Bağlantı"
        aside={
          isStale(state.network, CADENCE.network) ? (
            <StaleMark at={state.network.updatedAt} />
          ) : undefined
        }
      >
        {state.network.loading && !network ? (
          <Skeleton height="4rem" />
        ) : (
          <Rows>
            <Row
              label="Durum"
              value={
                <StatusPill tone={network?.online ? 'ok' : 'warn'}>
                  {network?.online ? 'Çevrimiçi' : 'Çevrimdışı'}
                </StatusPill>
              }
            />
            <Row label="Varsayılan rota" value={orUnknown(network?.defaultRoute?.interface)} mono />
            <Row label="Ağ geçidi" value={orUnknown(network?.defaultRoute?.gateway)} mono />
          </Rows>
        )}
      </Card>

      <div className={styles.grid}>
        {interfaces.length === 0 && !state.network.loading && (
          <Empty>Bildirilen ağ arayüzü yok.</Empty>
        )}
        {interfaces.map((item) => (
          <Card
            key={item.name}
            title={item.name}
            aside={
              <StatusPill tone={item.up ? 'ok' : 'neutral'}>{item.up ? 'up' : 'down'}</StatusPill>
            }
          >
            <Rows>
              <Row label="Tür" value={item.type === 'wifi' ? 'Wi-Fi' : 'Ethernet'} />
              <Row label="IPv4" value={item.ipv4?.length ? item.ipv4.join(', ') : '—'} mono />
              <Row label="MAC" value={orUnknown(item.mac)} mono />
              {item.type === 'wifi' ? (
                <>
                  <Row label="SSID" value={orUnknown(item.ssid)} />
                  <Row
                    label="Sinyal"
                    value={item.signalPercent !== undefined ? `%${item.signalPercent}` : '—'}
                  />
                </>
              ) : (
                <Row
                  label="Bağlantı hızı"
                  value={item.linkSpeedMbps ? `${item.linkSpeedMbps} Mb/s` : '—'}
                />
              )}
            </Rows>
          </Card>
        ))}
      </div>

      <p className={styles.note}>
        Ağ ayarları bu sürümde salt okunurdur. Wi-Fi yapılandırması sonraki dönüm noktasında
        eklenecektir.
      </p>
    </div>
  )
}
