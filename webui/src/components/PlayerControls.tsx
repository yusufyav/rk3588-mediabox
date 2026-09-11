import type { KodiResponse } from '../api/types'
import { formatClock } from '../lib/format'
import { useDevice, useDeviceState } from '../store/DeviceProvider'
import { Button } from './Button'
import { Banner, Empty } from './Status'
import styles from './PlayerControls.module.css'

const SEEK_STEPS = [-30, -10, 10, 30] as const

export function PlayerControls({
  kodi,
  size = 'default',
  showProgress = true,
}: {
  kodi?: KodiResponse
  size?: 'default' | 'large'
  showProgress?: boolean
}) {
  const { store, api } = useDevice()
  const { pendingAction, lastActionError } = useDeviceState()

  if (!kodi || kodi.state === 'offline') {
    return (
      <Empty>
        Kodi çalışmıyor. Oynatma kontrolleri kullanılamaz — Ayarlar sayfasından başlatabilirsiniz.
      </Empty>
    )
  }

  const player = kodi.player
  const hasMedia = kodi.state === 'playing' || kodi.state === 'paused'
  const position = player?.position ?? 0
  const duration = player?.duration ?? 0
  const percent = duration > 0 ? Math.min((position / duration) * 100, 100) : 0
  const busy = pendingAction !== undefined

  const run = (name: string, call: () => Promise<void>) => () => void store.runAction(name, call)

  return (
    <div className={styles.wrap}>
      <div className={styles.now}>
        <span className={styles.nowLabel}>Şu an</span>
        <span className={styles.nowTitle}>
          {hasMedia ? (player?.title ?? 'Adsız içerik') : 'Oynatılan içerik yok'}
        </span>
      </div>

      {showProgress && hasMedia && (
        <div className={styles.progress}>
          <div
            className={styles.track}
            role="progressbar"
            aria-label="Oynatma ilerlemesi"
            aria-valuemin={0}
            aria-valuemax={duration || 100}
            aria-valuenow={position}
          >
            <div className={styles.fill} style={{ width: `${percent}%` }} />
          </div>
          <div className={styles.times}>
            <span>{formatClock(position)}</span>
            <span>{duration > 0 ? formatClock(duration) : '--:--'}</span>
          </div>
        </div>
      )}

      <div className={styles.controls}>
        <Button
          variant="primary"
          size={size}
          className={styles.primaryControl}
          disabled={busy || !hasMedia}
          disabledReason={hasMedia ? undefined : 'Oynatılan içerik yok'}
          onClick={run('playpause', api.playPause)}
        >
          {kodi.state === 'playing' ? 'Duraklat' : 'Oynat'}
        </Button>
        <Button
          size={size}
          disabled={busy || !hasMedia}
          disabledReason={hasMedia ? undefined : 'Oynatılan içerik yok'}
          onClick={run('stop', api.stopPlayback)}
        >
          Durdur
        </Button>
        <span className={styles.spacer} />
        {SEEK_STEPS.map((step) => (
          <Button
            key={step}
            size={size}
            disabled={busy || !hasMedia}
            disabledReason={hasMedia ? undefined : 'Oynatılan içerik yok'}
            aria-label={`${step > 0 ? 'İleri' : 'Geri'} ${Math.abs(step)} saniye`}
            onClick={run(`seek${step}`, () => api.seek(step))}
          >
            {step > 0 ? `+${step}s` : `${step}s`}
          </Button>
        ))}
      </div>

      {lastActionError && (
        <Banner
          title="Komut başarısız"
          detail={lastActionError.message}
          action={
            <Button variant="quiet" aria-label="Hata mesajını kapat" onClick={store.clearActionError}>
              Kapat
            </Button>
          }
        />
      )}
    </div>
  )
}
