import { Card, Metric, Row, Rows, UsageBar } from '../components/Card'
import { PlayerControls } from '../components/PlayerControls'
import { KodiPill, Skeleton, StaleMark, StatusPill } from '../components/Status'
import {
  formatBytes,
  formatPercent,
  formatTemperature,
  formatUptime,
  orUnknown,
  percentOf,
  temperatureTone,
} from '../lib/format'
import { CADENCE, isStale } from '../store/deviceStore'
import { useDeviceState } from '../store/DeviceProvider'
import styles from './Pages.module.css'

/**
 * The dashboard answers one question from across the room: is the box healthy
 * and what is it doing? Detail lives on the dedicated pages, not here.
 */
export function Home() {
  const state = useDeviceState()
  const kodi = state.kodi.data
  const system = state.system.data
  const network = state.network.data
  const display = state.display.data

  const memoryPercent = percentOf(system?.memory?.usedBytes, system?.memory?.totalBytes)
  const rootFs = system?.storage?.[0]
  const storagePercent = percentOf(rootFs?.usedBytes, rootFs?.totalBytes)
  const defaultInterface = network?.interfaces?.find(
    (item) => item.name === network?.defaultRoute?.interface,
  )

  return (
    <div className={styles.stack}>
      <section className={styles.hero}>
        <div className={styles.heroText}>
          <span className={styles.note}>MediaBox durumu</span>
          <span className={styles.heroTitle}>
            {kodi?.state === 'playing' || kodi?.state === 'paused'
              ? (kodi.player?.title ?? 'Adsız içerik')
              : orUnknown(system?.hostname ?? 'MediaBox')}
          </span>
          <div className={styles.heroMeta}>
            <KodiPill state={kodi?.state} />
            <StatusPill tone={state.backendReachable ? 'ok' : 'danger'}>
              {state.backendReachable ? 'Servis çalışıyor' : 'Servis erişilemiyor'}
            </StatusPill>
            {system?.uptimeSeconds !== undefined && (
              <StatusPill tone="neutral">Çalışma süresi {formatUptime(system.uptimeSeconds)}</StatusPill>
            )}
          </div>
        </div>
        <div className={styles.heroPills}>
          {display?.connected && (
            <StatusPill tone="info">
              {orUnknown(display.mode)}
              {display.refreshHz ? ` @ ${display.refreshHz.toFixed(2)} Hz` : ''}
            </StatusPill>
          )}
          {display?.hdr?.active !== undefined && (
            <StatusPill tone={display.hdr.active ? 'ok' : 'neutral'}>
              {display.hdr.active ? 'HDR etkin' : 'SDR'}
            </StatusPill>
          )}
        </div>
      </section>

      <div className={styles.grid}>
        <Card
          title="Oynatıcı"
          className={styles.wide}
          aside={
            isStale(state.kodi, CADENCE.kodi) ? <StaleMark at={state.kodi.updatedAt} /> : undefined
          }
        >
          {state.kodi.loading && !kodi ? <Skeleton height="6rem" /> : <PlayerControls kodi={kodi} />}
        </Card>

        <Card
          title="Sistem"
          aside={
            isStale(state.system, CADENCE.system) ? (
              <StaleMark at={state.system.updatedAt} />
            ) : undefined
          }
        >
          {state.system.loading && !system ? (
            <Skeleton height="5rem" />
          ) : (
            <div className={styles.metricRow}>
              <Metric
                label="CPU sıcaklığı"
                value={
                  <span style={{ color: toneColor(temperatureTone(system?.cpu?.temperatureC)) }}>
                    {formatTemperature(system?.cpu?.temperatureC)}
                  </span>
                }
                hint={system?.cpu?.cores ? `${system.cpu.cores} çekirdek` : undefined}
              />
              <Metric
                label="Bellek"
                value={formatPercent(memoryPercent)}
                hint={`${formatBytes(system?.memory?.usedBytes)} / ${formatBytes(system?.memory?.totalBytes)}`}
              />
              <Metric
                label="Depolama"
                value={formatPercent(storagePercent)}
                hint={rootFs ? `${rootFs.mountpoint}` : undefined}
              />
            </div>
          )}
          {memoryPercent !== undefined && <UsageBar percent={memoryPercent} />}
        </Card>

        <Card
          title="Ağ"
          aside={
            isStale(state.network, CADENCE.network) ? (
              <StaleMark at={state.network.updatedAt} />
            ) : undefined
          }
        >
          {state.network.loading && !network ? (
            <Skeleton height="5rem" />
          ) : (
            <Rows>
              <Row
                label="Durum"
                value={
                  <StatusPill tone={network?.online ? 'ok' : 'warn'}>
                    {network?.online ? 'Bağlı' : 'Bağlantı yok'}
                  </StatusPill>
                }
              />
              <Row label="Arayüz" value={orUnknown(network?.defaultRoute?.interface)} mono />
              <Row label="IPv4" value={orUnknown(defaultInterface?.ipv4?.[0])} mono />
              <Row label="Ağ geçidi" value={orUnknown(network?.defaultRoute?.gateway)} mono />
            </Rows>
          )}
        </Card>

        <Card
          title="Görüntü"
          aside={
            isStale(state.display, CADENCE.display) ? (
              <StaleMark at={state.display.updatedAt} />
            ) : undefined
          }
        >
          {state.display.loading && !display ? (
            <Skeleton height="5rem" />
          ) : (
            <Rows>
              <Row label="Konnektör" value={orUnknown(display?.connector)} mono />
              <Row
                label="Mod"
                value={`${orUnknown(display?.mode)}${display?.refreshHz ? ` @ ${display.refreshHz.toFixed(2)} Hz` : ''}`}
                mono
              />
              <Row
                label="Renk"
                value={`${orUnknown(display?.colorimetry)}${display?.colorDepth ? ` · ${display.colorDepth} bit` : ''}`}
              />
              <Row label="HDR" value={orUnknown(display?.hdr?.eotf ?? (display?.hdr?.active ? 'Etkin' : 'Kapalı'))} />
            </Rows>
          )}
        </Card>
      </div>
    </div>
  )
}

function toneColor(tone: 'ok' | 'warn' | 'danger' | 'neutral') {
  switch (tone) {
    case 'danger':
      return 'var(--danger)'
    case 'warn':
      return 'var(--warn)'
    default:
      return undefined
  }
}
