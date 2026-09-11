import type { ReactNode } from 'react'
import { Card, Metric, Row, Rows, UsageBar } from '../components/Card'
import { KodiPill, Skeleton, StaleMark, StatusPill } from '../components/Status'
import {
  formatBytes,
  formatPercent,
  formatTemperature,
  formatUptime,
  orUnknown,
  percentOf,
} from '../lib/format'
import { CADENCE, isStale } from '../store/deviceStore'
import { useDeviceState } from '../store/DeviceProvider'
import styles from './Pages.module.css'

/**
 * Device facts. Sections are independent cards so upcoming sensors (GPU, NPU,
 * CEC, Bluetooth) are added by appending a card, not by reworking this page.
 */
export function Device() {
  const state = useDeviceState()
  const system = state.system.data
  const kodi = state.kodi.data
  const display = state.display.data
  const memoryPercent = percentOf(system?.memory?.usedBytes, system?.memory?.totalBytes)

  return (
    <div className={styles.grid}>
      <Card
        title="Kimlik"
        aside={
          isStale(state.system, CADENCE.system) ? <StaleMark at={state.system.updatedAt} /> : undefined
        }
      >
        {state.system.loading && !system ? (
          <Skeleton height="7rem" />
        ) : (
          <Rows>
            <Row label="Sunucu adı" value={orUnknown(system?.hostname)} mono />
            <Row label="Çekirdek" value={orUnknown(system?.kernel)} mono />
            <Row label="Mimari" value={orUnknown(system?.architecture)} mono />
            <Row label="Çalışma süresi" value={formatUptime(system?.uptimeSeconds)} />
          </Rows>
        )}
      </Card>

      <Card title="İşlemci ve bellek">
        <div className={styles.metricRow}>
          <Metric label="Sıcaklık" value={formatTemperature(system?.cpu?.temperatureC)} />
          <Metric
            label="Yük ortalaması"
            value={system?.cpu?.loadAverage?.map((v) => v.toFixed(2)).join(' · ') ?? '—'}
            hint={system?.cpu?.cores ? `${system.cpu.cores} çekirdek` : undefined}
          />
        </div>
        <Rows>
          <Row label="Kullanılan bellek" value={formatBytes(system?.memory?.usedBytes)} />
          <Row label="Toplam bellek" value={formatBytes(system?.memory?.totalBytes)} />
          <Row label="Kullanım" value={formatPercent(memoryPercent)} />
        </Rows>
        {memoryPercent !== undefined && <UsageBar percent={memoryPercent} />}
      </Card>

      <Card title="Depolama">
        {system?.storage?.length ? (
          <div className={styles.stack}>
            {system.storage.map((volume) => {
              const percent = percentOf(volume.usedBytes, volume.totalBytes)
              return (
                <div key={volume.mountpoint} className={styles.stack}>
                  <Row
                    label={volume.mountpoint}
                    value={`${formatBytes(volume.usedBytes)} / ${formatBytes(volume.totalBytes)} (${formatPercent(percent)})`}
                  />
                  {percent !== undefined && <UsageBar percent={percent} />}
                </div>
              )
            })}
          </div>
        ) : (
          <p className={styles.note}>Depolama bilgisi bildirilmedi.</p>
        )}
      </Card>

      <Card title="Kodi">
        <Rows>
          <Row label="Durum" value={<KodiPill state={kodi?.state} />} />
          <Row
            label="Servis"
            value={
              <StatusPill tone={kodi?.serviceActive ? 'ok' : 'danger'}>
                {kodi?.serviceActive ? 'active' : 'inactive'}
              </StatusPill>
            }
          />
          <Row label="Sürüm" value={orUnknown(kodi?.version)} />
        </Rows>
      </Card>

      <Card title="Görüntü">
        <Rows>
          <Row label="Konnektör" value={orUnknown(display?.connector)} mono />
          <Row
            label="Bağlantı"
            value={
              <StatusPill tone={display?.connected ? 'ok' : 'warn'}>
                {display?.connected ? 'Bağlı' : 'Bağlı değil'}
              </StatusPill>
            }
          />
          <Row label="Mod" value={orUnknown(display?.mode)} mono />
          <Row label="Tazeleme" value={display?.refreshHz ? `${display.refreshHz.toFixed(2)} Hz` : '—'} />
          <Row label="Renk derinliği" value={display?.colorDepth ? `${display.colorDepth} bit` : '—'} />
          <Row label="Kolorimetri" value={orUnknown(display?.colorimetry)} />
          <Row label="HDR" value={orUnknown(display?.hdr?.eotf)} />
        </Rows>
      </Card>

      <Card title="Sonraki dönüm noktaları">
        <Pending items={['GPU kullanımı', 'NPU durumu', 'HDMI-CEC', 'Bluetooth']} />
      </Card>
    </div>
  )
}

/** Reserved rows: the card layout is already correct when the data arrives. */
function Pending({ items }: { items: string[] }): ReactNode {
  return (
    <Rows>
      {items.map((item) => (
        <Row key={item} label={item} value={<StatusPill tone="neutral">Henüz yok</StatusPill>} />
      ))}
    </Rows>
  )
}
