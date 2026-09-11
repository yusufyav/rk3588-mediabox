import { useEffect, useState } from 'react'
import { Button } from '../components/Button'
import { formatClock } from '../lib/format'
import { useDevice, useDeviceState } from '../store/DeviceProvider'
import type { CastSession } from '../api/types'
import { Panel } from './Panel'
import styles from './NowPlaying.module.css'

const SEEK_STEPS = [-30, -10, 10, 30] as const

/**
 * What the television is playing, and the handful of controls that belong with
 * it. Playback itself is Kodi's; this only sends it instructions.
 */
export function NowPlaying({ onClose }: { onClose: () => void }) {
  const { store, api } = useDevice()
  const state = useDeviceState()
  const [session, setSession] = useState<CastSession | null>(null)

  useEffect(() => {
    let cancelled = false
    void api
      .getCastSession()
      .then((value) => {
        if (!cancelled) setSession(value)
      })
      .catch(() => undefined)
    return () => {
      cancelled = true
    }
  }, [api])

  const kodi = state.kodi.data
  const player = kodi?.player
  const hasMedia = kodi?.state === 'playing' || kodi?.state === 'paused'
  const position = player?.position ?? 0
  const duration = player?.duration ?? 0
  const percent = duration > 0 ? Math.min((position / duration) * 100, 100) : 0
  const busy = state.pendingAction !== undefined
  const run = (name: string, call: () => Promise<void>) => () => void store.runAction(name, call)

  return (
    <Panel title="TV'de oynatılıyor" onClose={onClose} width="narrow">
      {!hasMedia ? (
        <p className={styles.idle}>
          {kodi?.state === 'offline'
            ? 'Kodi çalışmıyor. Ayarlar → Sistem bölümünden başlatabilirsiniz.'
            : 'Şu anda TV’de oynatılan bir içerik yok.'}
        </p>
      ) : (
        <>
          <h3 className={styles.title}>{player?.title ?? 'Adsız içerik'}</h3>

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

          <div className={styles.controls}>
            <Button variant="primary" disabled={busy} onClick={run('playpause', api.playPause)}>
              {kodi?.state === 'playing' ? 'Duraklat' : 'Oynat'}
            </Button>
            <Button disabled={busy} onClick={run('stop', api.stopPlayback)}>
              Durdur
            </Button>
          </div>

          <div className={styles.seek}>
            {SEEK_STEPS.map((step) => (
              <Button
                key={step}
                disabled={busy}
                aria-label={`${step > 0 ? 'İleri' : 'Geri'} ${Math.abs(step)} saniye`}
                onClick={run(`seek${step}`, () =>
                  api.seek(
                    Math.min(Math.max(position + step, 0), duration || Number.MAX_SAFE_INTEGER),
                  ),
                )}
              >
                {step > 0 ? `+${step}s` : `${step}s`}
              </Button>
            ))}
          </div>
        </>
      )}

      {session && (
        <dl className={styles.handoff}>
          <dt>Kaynak</dt>
          <dd className={styles.source} title={session.source}>
            {session.source}
          </dd>
          <dt>Devralınan konum</dt>
          <dd>{formatClock(session.resumeSeconds)}</dd>
        </dl>
      )}

      {state.lastActionError && (
        <p className={styles.error}>{state.lastActionError.message}</p>
      )}
    </Panel>
  )
}
