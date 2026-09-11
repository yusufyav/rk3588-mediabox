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
import styles from './Diagnostics.module.css'

/**
 * Everything the appliance knows about itself, in one place for whoever is
 * debugging it.
 *
 * This is deliberately the deepest screen in the product. None of it belongs on
 * a media home screen: nobody sits down to watch a film and wants to be told
 * the CPU temperature first.
 */
export function Diagnostics() {
  const state = useDeviceState()
  const system = state.system.data
  const network = state.network.data
  const display = state.display.data
  const kodi = state.kodi.data
  const stremio = state.stremio.data
  const now = Date.now()

  const memoryPercent = percentOf(system?.memory?.usedBytes, system?.memory?.totalBytes)
  const root = system?.storage?.find((item) => item.mountpoint === '/')
  const storagePercent = percentOf(root?.usedBytes, root?.totalBytes)

  return (
    <div className={styles.wrap}>
      <Group title="Sistem" stale={isStale(state.system, CADENCE.system, now)}>
        <Row label="Ana makine" value={orUnknown(system?.hostname)} />
        <Row label="Çekirdek" value={orUnknown(system?.kernel)} />
        <Row label="Mimari" value={orUnknown(system?.architecture)} />
        <Row label="Çalışma süresi" value={formatUptime(system?.uptimeSeconds)} />
        <Row
          label="Yük ortalaması"
          value={(system?.cpu?.loadAverage ?? [])
            .filter((value) => value !== undefined && value !== null)
            .map((value) => Number(value).toFixed(2))
            .join('  ') || '—'}
        />
        <Row label="Çekirdek sayısı" value={orUnknown(system?.cpu?.cores)} />
        <Row label="CPU sıcaklığı" value={formatTemperature(system?.cpu?.temperatureC)} />
        <Row
          label="Bellek"
          value={`${formatBytes(system?.memory?.usedBytes)} / ${formatBytes(
            system?.memory?.totalBytes,
          )}  (${formatPercent(memoryPercent)})`}
        />
        <Row
          label="Kök dosya sistemi"
          value={`${formatBytes(root?.usedBytes)} / ${formatBytes(root?.totalBytes)}  (${formatPercent(
            storagePercent,
          )})`}
        />
      </Group>

      <Group title="Ağ" stale={isStale(state.network, CADENCE.network, now)}>
        <Row label="Durum" value={network?.online ? 'Çevrimiçi' : 'Çevrimdışı'} />
        <Row label="Varsayılan geçit" value={orUnknown(network?.defaultRoute?.gateway)} />
        <Row label="Çıkış arayüzü" value={orUnknown(network?.defaultRoute?.interface)} />
        {network?.interfaces?.map((item) => (
          <Row
            key={item.name}
            label={item.name}
            value={`${item.up ? 'up' : 'down'} · ${(item.ipv4 ?? []).join(', ') || 'adres yok'}${
              item.mac ? ` · ${item.mac}` : ''
            }${item.linkSpeedMbps ? ` · ${item.linkSpeedMbps} Mb/s` : ''}`}
          />
        ))}
      </Group>

      <Group title="Görüntü" stale={isStale(state.display, CADENCE.display, now)}>
        <Row label="Bağlantı" value={orUnknown(display?.connector)} />
        <Row label="Bağlı" value={display?.connected ? 'evet' : 'hayır'} />
        <Row label="Mod" value={orUnknown(display?.mode)} />
        <Row label="Tazeleme" value={display?.refreshHz ? `${display.refreshHz} Hz` : '—'} />
        <Row
          label="Renk derinliği"
          value={display?.colorDepth ? `${display.colorDepth} bit/bileşen` : '—'}
        />
        <Row label="Kolorimetri" value={orUnknown(display?.colorimetry)} />
        <Row label="HDR" value={display?.hdr?.active ? (display.hdr.eotf ?? 'etkin') : 'SDR'} />
      </Group>

      <Group title="Medya düzlemi" stale={isStale(state.kodi, CADENCE.kodi, now)}>
        <Row label="Kodi servisi" value={kodi?.serviceActive ? 'çalışıyor' : 'durdurulmuş'} />
        <Row label="Kodi durumu" value={orUnknown(kodi?.state)} />
        <Row label="Oynatılan" value={orUnknown(kodi?.player?.title)} />
        <Row
          label="Stremio akış sunucusu"
          value={
            stremio?.reachable
              ? `ulaşılabilir${stremio.serverVersion ? ` · ${stremio.serverVersion}` : ''}`
              : 'ulaşılamıyor'
          }
        />
        <Row label="Akış sunucusu yolu" value={orUnknown(stremio?.mount)} />
        <Row
          label="Yayın hedefi"
          value={
            stremio?.castDevice
              ? `${stremio.castDevice.name} (${stremio.castDevice.id}, ${stremio.castDevice.type})`
              : '—'
          }
        />
      </Group>

      <Group title="Kontrol düzlemi" stale={isStale(state.health, CADENCE.health, now)}>
        <Row label="Servis sürümü" value={orUnknown(state.health.data?.version)} />
        <Row label="Servis durumu" value={orUnknown(state.health.data?.status)} />
        <Row label="Olay akışı" value={state.streamConnected ? 'bağlı' : 'yoklama'} />
        <Row
          label="Güç işlemleri"
          value={state.health.data?.actions?.reboot ? 'etkin' : 'devre dışı'}
        />
      </Group>
    </div>
  )
}

function Group({
  title,
  stale,
  children,
}: {
  title: string
  stale: boolean
  children: React.ReactNode
}) {
  return (
    <section className={styles.group}>
      <h3 className={styles.groupTitle}>
        {title}
        {stale && <span className={styles.stale}>eski veri</span>}
      </h3>
      <dl className={styles.rows}>{children}</dl>
    </section>
  )
}

function Row({ label, value }: { label: string; value: string | number }) {
  return (
    <>
      <dt className={styles.key}>{label}</dt>
      <dd className={styles.value}>{value}</dd>
    </>
  )
}
