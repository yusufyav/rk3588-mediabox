import { useState } from 'react'
import { Button } from '../components/Button'
import { ConfirmDialog } from '../components/ConfirmDialog'
import { Diagnostics } from './Diagnostics'
import { Panel } from './Panel'
import { useDevice, useDeviceState } from '../store/DeviceProvider'
import { orUnknown } from '../lib/format'
import styles from './SettingsPanel.module.css'

type SectionId =
  | 'playback'
  | 'network'
  | 'bluetooth'
  | 'cec'
  | 'display'
  | 'audio'
  | 'system'
  | 'diagnostics'

const SECTIONS: Array<{ id: SectionId; label: string }> = [
  { id: 'playback', label: 'Oynatma' },
  { id: 'network', label: 'Ağ' },
  { id: 'bluetooth', label: 'Bluetooth' },
  { id: 'cec', label: 'HDMI / CEC' },
  { id: 'display', label: 'Ekran' },
  { id: 'audio', label: 'Ses' },
  { id: 'system', label: 'Sistem' },
  { id: 'diagnostics', label: 'Tanılama' },
]

/**
 * Device settings, secondary to the media experience by construction: it is a
 * panel reached from one control, not a place the product opens into.
 *
 * Sections the backend cannot actually drive yet say so in plain words rather
 * than rendering switches that do nothing.
 */
export function SettingsPanel({ onClose }: { onClose: () => void }) {
  const [section, setSection] = useState<SectionId>('playback')

  return (
    <Panel title="Ayarlar" onClose={onClose}>
      <div className={styles.layout}>
        <nav className={styles.nav} aria-label="Ayar bölümleri">
          {SECTIONS.map((item) => (
            <button
              key={item.id}
              type="button"
              data-focusable=""
              className={styles.navItem}
              aria-current={section === item.id ? 'true' : undefined}
              onClick={() => setSection(item.id)}
            >
              {item.label}
            </button>
          ))}
        </nav>
        <section className={styles.section} aria-live="polite">
          {section === 'playback' && <Playback />}
          {section === 'network' && <Network />}
          {section === 'bluetooth' && (
            <Unsupported
              title="Bluetooth"
              detail="Bu sürümde Bluetooth cihaz yönetimi için servis tarafı desteği yok. Eşleştirme ekranı, gerçekten eşleştirme yapabildiğinde eklenecek."
            />
          )}
          {section === 'cec' && (
            <Unsupported
              title="HDMI / CEC"
              detail="CEC sahipliği mimari olarak mediaboxd'ye ait, ancak bu sürümde CEC kontrol yüzeyi henüz uygulanmadı. Uzaktan kumanda tuşları çekirdek tarafından zaten normal giriş olayına çevriliyor."
            />
          )}
          {section === 'display' && <Display />}
          {section === 'audio' && (
            <Unsupported
              title="Ses"
              detail="Ses çıkışı ve passthrough seçimleri Kodi tarafında yönetiliyor. MediaBox üzerinden ses ayarı için servis tarafı desteği henüz yok."
            />
          )}
          {section === 'system' && <System />}
          {section === 'diagnostics' && <Diagnostics />}
        </section>
      </div>
    </Panel>
  )
}

function Unsupported({ title, detail }: { title: string; detail: string }) {
  return (
    <div className={styles.block}>
      <h3 className={styles.heading}>{title}</h3>
      <p className={styles.unsupported}>{detail}</p>
    </div>
  )
}

function Playback() {
  const { store, api } = useDevice()
  const state = useDeviceState()
  const kodi = state.kodi.data
  const actions = store.actions
  const busy = state.pendingAction !== undefined
  const run = (name: string, call: () => Promise<void>) => () => void store.runAction(name, call)

  return (
    <div className={styles.block}>
      <h3 className={styles.heading}>Oynatma motoru</h3>
      <p className={styles.lead}>
        Tam oynatma Kodi üzerinde çalışır: donanım kod çözme, HDR ve ses çıkışı ona aittir.
        Tarayıcıdaki ön izleme bunun yerine geçmez.
      </p>
      <dl className={styles.rows}>
        <dt>Durum</dt>
        <dd>{describeKodi(kodi?.state)}</dd>
        <dt>Stremio akış sunucusu</dt>
        <dd>
          {state.stremio.data?.reachable
            ? `Çalışıyor${state.stremio.data.serverVersion ? ` · ${state.stremio.data.serverVersion}` : ''}`
            : 'Ulaşılamıyor'}
        </dd>
        <dt>TV hedefi</dt>
        <dd>{orUnknown(state.stremio.data?.castDevice?.name)}</dd>
      </dl>
      <div className={styles.actions}>
        <Button
          disabled={busy || actions.kodiStart === false}
          disabledReason={actions.reason}
          onClick={run('kodiStart', api.startKodi)}
        >
          Başlat
        </Button>
        <Button
          disabled={busy || actions.kodiRestart === false}
          disabledReason={actions.reason}
          onClick={run('kodiRestart', api.restartKodi)}
        >
          Yeniden başlat
        </Button>
        <Button
          variant="danger"
          disabled={busy || actions.kodiStop === false}
          disabledReason={actions.reason}
          onClick={run('kodiStopService', api.stopKodi)}
        >
          Durdur
        </Button>
      </div>
      {state.lastActionError && <p className={styles.error}>{state.lastActionError.message}</p>}
    </div>
  )
}

function Network() {
  const state = useDeviceState()
  const network = state.network.data
  return (
    <div className={styles.block}>
      <h3 className={styles.heading}>Ağ</h3>
      <p className={styles.lead}>
        Bu sürümde ağ yapılandırması salt okunurdur; Wi-Fi bağlanma ve statik adres yazma
        işlemleri henüz servis tarafında desteklenmiyor.
      </p>
      <dl className={styles.rows}>
        <dt>Bağlantı</dt>
        <dd>{network?.online ? 'Çevrimiçi' : 'Çevrimdışı'}</dd>
        <dt>Varsayılan geçit</dt>
        <dd>{orUnknown(network?.defaultRoute?.gateway)}</dd>
      </dl>
      {network?.interfaces
        ?.filter((item) => item.type !== 'loopback')
        .map((item) => (
          <dl className={styles.rows} key={item.name}>
            <dt>{item.name}</dt>
            <dd>
              {(item.ipv4 ?? []).join(', ') || '—'}
              {item.ssid ? ` · ${item.ssid}` : ''}
              {item.up ? '' : ' · bağlı değil'}
            </dd>
          </dl>
        ))}
    </div>
  )
}

function Display() {
  const state = useDeviceState()
  const display = state.display.data
  return (
    <div className={styles.block}>
      <h3 className={styles.heading}>Ekran</h3>
      <p className={styles.lead}>
        Mod, renk derinliği ve HDR durumu kabul edilmiş görüntü zincirine aittir ve buradan
        değiştirilmez; gösterilen değerler sürücüden okunur.
      </p>
      <dl className={styles.rows}>
        <dt>Bağlantı</dt>
        <dd>
          {orUnknown(display?.connector)}
          {display?.connected === false ? ' · bağlı değil' : ''}
        </dd>
        <dt>Mod</dt>
        <dd>{orUnknown(display?.mode)}</dd>
        <dt>Renk</dt>
        <dd>{orUnknown(display?.colorimetry)}</dd>
        <dt>HDR</dt>
        <dd>{display?.hdr?.active ? (display.hdr.eotf ?? 'Etkin') : 'SDR'}</dd>
      </dl>
    </div>
  )
}

function System() {
  const { store, api } = useDevice()
  const state = useDeviceState()
  const actions = store.actions
  const [confirm, setConfirm] = useState<'reboot' | 'shutdown' | null>(null)

  return (
    <div className={styles.block}>
      <h3 className={styles.heading}>Sistem</h3>
      <dl className={styles.rows}>
        <dt>Cihaz</dt>
        <dd>{orUnknown(state.system.data?.hostname)}</dd>
        <dt>Servis sürümü</dt>
        <dd>{orUnknown(state.health.data?.version)}</dd>
      </dl>
      <div className={styles.actions}>
        <Button
          disabled={actions.reboot !== true}
          disabledReason={actions.reason}
          onClick={() => setConfirm('reboot')}
        >
          Yeniden başlat
        </Button>
        <Button
          variant="danger"
          disabled={actions.shutdown !== true}
          disabledReason={actions.reason}
          onClick={() => setConfirm('shutdown')}
        >
          Kapat
        </Button>
      </div>
      {confirm && (
        <ConfirmDialog
          title={confirm === 'reboot' ? 'Cihaz yeniden başlatılsın mı?' : 'Cihaz kapatılsın mı?'}
          message="Oynatma duracak ve cihaz kısa süre erişilemez olacak."
          confirmLabel={confirm === 'reboot' ? 'Yeniden başlat' : 'Kapat'}
          onCancel={() => setConfirm(null)}
          onConfirm={() => {
            void store.runAction(confirm, confirm === 'reboot' ? api.reboot : api.shutdown)
            setConfirm(null)
          }}
        />
      )}
    </div>
  )
}

function describeKodi(state?: string): string {
  switch (state) {
    case 'playing':
      return 'Oynatıyor'
    case 'paused':
      return 'Duraklatıldı'
    case 'idle':
      return 'Hazır'
    case 'offline':
      return 'Çalışmıyor'
    default:
      return '—'
  }
}
