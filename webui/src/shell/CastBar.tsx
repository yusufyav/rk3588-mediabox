import { useState } from 'react'
import { Button } from '../components/Button'
import { formatClock } from '../lib/format'
import type { PreviewState } from '../stremio/core'
import styles from './CastBar.module.css'

type Phase = 'idle' | 'sending' | 'sent' | 'failed'

/**
 * The preview-to-TV handoff, shown while Stremio's player is on screen.
 *
 * What is on screen at that moment is the preview: the browser playing the
 * stream so you can check it is the right thing, at the right quality, before
 * committing the television to it. This is the control that commits it.
 *
 * The position travels with the stream. Upstream's own cast menu always sends
 * position zero, so this sends the same action with the live playhead filled in.
 */
export function CastBar({
  deviceId,
  preview,
  onCast,
  onOpenNowPlaying,
}: {
  deviceId: string
  preview: PreviewState | null
  onCast: (deviceId: string) => Promise<PreviewState>
  onOpenNowPlaying: () => void
}) {
  const [phase, setPhase] = useState<Phase>('idle')
  const [error, setError] = useState<string | null>(null)
  const [handedAt, setHandedAt] = useState<number | null>(null)

  const ready = Boolean(preview?.source)
  const positionSeconds = (preview?.timeMs ?? 0) / 1000

  const cast = async () => {
    setPhase('sending')
    setError(null)
    try {
      const sent = await onCast(deviceId)
      setHandedAt(sent.timeMs)
      setPhase('sent')
      onOpenNowPlaying()
    } catch (failure) {
      setError(failure instanceof Error ? failure.message : String(failure))
      setPhase('failed')
    }
  }

  return (
    <div className={styles.bar} data-testid="cast-bar">
      <div className={styles.label}>
        <span className={styles.previewMark}>Ön izleme</span>
        {positionSeconds > 0 && <span className={styles.position}>{formatClock(positionSeconds)}</span>}
      </div>

      {phase === 'sent' ? (
        <button
          type="button"
          data-focusable=""
          className={styles.sent}
          onClick={onOpenNowPlaying}
        >
          TV'ye aktarıldı
          {handedAt !== null && handedAt > 0 && ` · ${formatClock(handedAt / 1000)}`}
        </button>
      ) : (
        <Button
          variant="primary"
          className={styles.action}
          disabled={!ready || phase === 'sending'}
          disabledReason={ready ? undefined : 'Stremio henüz bir kaynak çözmedi'}
          onClick={() => void cast()}
        >
          {phase === 'sending' ? 'Aktarılıyor…' : "Kodi'de Oynat"}
        </Button>
      )}

      {error && <span className={styles.error}>{error}</span>}
    </div>
  )
}
