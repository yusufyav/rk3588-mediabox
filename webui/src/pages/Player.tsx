import { Card, Row, Rows } from '../components/Card'
import { PlayerControls } from '../components/PlayerControls'
import { Banner, KodiPill, Skeleton, StaleMark } from '../components/Status'
import { formatClock, orUnknown } from '../lib/format'
import { CADENCE, isStale } from '../store/deviceStore'
import { useDeviceState } from '../store/DeviceProvider'
import styles from './Pages.module.css'

/** Full-size transport controls, sized for a remote across a living room. */
export function Player() {
  const state = useDeviceState()
  const kodi = state.kodi.data
  const player = kodi?.player
  const remaining =
    player?.duration !== undefined && player?.position !== undefined
      ? player.duration - player.position
      : undefined

  return (
    <div className={styles.stack}>
      {state.kodi.error && !state.kodi.error.unreachable && (
        <Banner title="Kodi durumu okunamadı" detail={state.kodi.error.message} tone="warn" />
      )}

      <Card
        title="Oynatma kontrolü"
        aside={
          <>
            <KodiPill state={kodi?.state} />
            {isStale(state.kodi, CADENCE.kodi) && <StaleMark at={state.kodi.updatedAt} />}
          </>
        }
      >
        {state.kodi.loading && !kodi ? (
          <Skeleton height="8rem" />
        ) : (
          <PlayerControls kodi={kodi} size="large" />
        )}
      </Card>

      <Card title="İçerik bilgisi">
        <Rows>
          <Row label="Başlık" value={orUnknown(player?.title)} />
          <Row label="Tür" value={orUnknown(player?.mediaType)} />
          <Row label="Konum" value={formatClock(player?.position)} mono />
          <Row label="Süre" value={formatClock(player?.duration)} mono />
          <Row label="Kalan" value={formatClock(remaining)} mono />
          <Row label="Kodi sürümü" value={orUnknown(kodi?.version)} />
        </Rows>
      </Card>
    </div>
  )
}
