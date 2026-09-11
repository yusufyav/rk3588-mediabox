import { useCallback, useEffect, useState } from 'react'
import { Button } from '../components/Button'
import { Banner } from '../components/Status'
import { useLayoutClass } from '../nav/useLayoutClass'
import { BACK_EVENT, useRemoteNavigation } from '../nav/useRemoteNavigation'
import { useDevice, useDeviceState } from '../store/DeviceProvider'
import { CastBar } from './CastBar'
import { NowPlaying } from './NowPlaying'
import { SettingsPanel } from './SettingsPanel'
import { useStremio } from './useStremio'
import styles from './Shell.module.css'

export type Panel = 'none' | 'settings' | 'nowPlaying'

/**
 * The appliance layer over the media experience.
 *
 * The media experience is the Stremio app in this same document; this renders
 * on top of it and owns only what the appliance is responsible for: handing a
 * stream to the TV, what the TV is currently playing, and device settings.
 *
 * It deliberately shows no system telemetry. Temperatures, kernel versions and
 * link states live in Settings → Diagnostics, which is where someone goes to
 * debug a box, not where someone goes to watch something.
 */
export function Shell() {
  const layout = useLayoutClass()
  const { store, mock } = useDevice()
  const state = useDeviceState()
  const [panel, setPanel] = useState<Panel>('none')
  const mount = state.health.data?.media?.serverMount ?? '/server/'
  const castDeviceId = state.health.data?.media?.castDeviceId ?? 'mediabox-tv'
  const stremio = useStremio(mount)

  useRemoteNavigation()

  const close = useCallback(() => setPanel('none'), [])

  // Back closes whatever the appliance layer put on screen, then gets out of
  // the way so the media app can handle its own back navigation.
  useEffect(() => {
    const onBack = (event: Event) => {
      if (panel === 'none') return
      event.preventDefault()
      setPanel('none')
    }
    window.addEventListener(BACK_EVENT, onBack)
    return () => window.removeEventListener(BACK_EVENT, onBack)
  }, [panel])

  const kodi = state.kodi.data
  const playingOnTv = kodi?.state === 'playing' || kodi?.state === 'paused'

  return (
    <div className={styles.shell} data-layout={layout} data-mediabox-shell="">
      <div className={styles.cluster}>
        {mock && <span className={styles.mockMark}>Mock mod</span>}
        {playingOnTv && (
          <Button
            className={styles.pill}
            aria-label="TV'de oynatılanı göster"
            onClick={() => setPanel(panel === 'nowPlaying' ? 'none' : 'nowPlaying')}
          >
            <span className={styles.pillDot} data-state={kodi?.state} />
            <span className={styles.pillText}>{kodi?.player?.title ?? "TV'de oynatılıyor"}</span>
          </Button>
        )}
        <Button
          className={styles.iconButton}
          aria-label="MediaBox ayarları"
          title="MediaBox ayarları"
          onClick={() => setPanel(panel === 'settings' ? 'none' : 'settings')}
        >
          <GearIcon />
        </Button>
      </div>

      {stremio.onPlayer && (
        <CastBar
          deviceId={castDeviceId}
          preview={stremio.preview}
          onCast={stremio.castToKodi}
          onOpenNowPlaying={() => setPanel('nowPlaying')}
        />
      )}

      {!state.backendReachable && (
        <div className={styles.alert}>
          <Banner
            title="MediaBox servisine ulaşılamıyor"
            detail="Cihaz kontrolleri şu an kullanılamıyor. Bağlantı kurulunca otomatik düzelir."
            action={
              <Button variant="quiet" onClick={() => void store.refreshAll()}>
                Yeniden dene
              </Button>
            }
          />
        </div>
      )}

      {panel === 'nowPlaying' && <NowPlaying onClose={close} />}
      {panel === 'settings' && <SettingsPanel onClose={close} />}
    </div>
  )
}

function GearIcon() {
  return (
    <svg viewBox="0 0 24 24" width="22" height="22" aria-hidden="true" focusable="false">
      <path
        fill="currentColor"
        d="M12 15.5A3.5 3.5 0 1 1 15.5 12 3.5 3.5 0 0 1 12 15.5Zm7.4-2.1a7.6 7.6 0 0 0 0-2.8l2-1.5-2-3.4-2.3 1a7.7 7.7 0 0 0-2.4-1.4L14.4 3H9.6l-.3 2.3A7.7 7.7 0 0 0 6.9 6.7l-2.3-1-2 3.4 2 1.5a7.6 7.6 0 0 0 0 2.8l-2 1.5 2 3.4 2.3-1a7.7 7.7 0 0 0 2.4 1.4l.3 2.3h4.8l.3-2.3a7.7 7.7 0 0 0 2.4-1.4l2.3 1 2-3.4Z"
      />
    </svg>
  )
}
