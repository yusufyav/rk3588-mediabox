import type { ReactNode } from 'react'
import type { KodiState } from '../api/types'
import styles from './Status.module.css'

export type Tone = 'ok' | 'info' | 'warn' | 'danger' | 'neutral'

export function StatusPill({ tone = 'neutral', children }: { tone?: Tone; children: ReactNode }) {
  return (
    <span className={styles.pill} data-tone={tone}>
      <span className={styles.dot} aria-hidden="true" />
      {children}
    </span>
  )
}

const KODI_LABEL: Record<KodiState, { label: string; tone: Tone }> = {
  playing: { label: 'Oynatılıyor', tone: 'ok' },
  paused: { label: 'Duraklatıldı', tone: 'info' },
  idle: { label: 'Boşta', tone: 'info' },
  offline: { label: 'Kodi kapalı', tone: 'danger' },
}

export function KodiPill({ state }: { state?: KodiState }) {
  if (!state) return <StatusPill tone="neutral">Bilinmiyor</StatusPill>
  const { label, tone } = KODI_LABEL[state]
  return <StatusPill tone={tone}>{label}</StatusPill>
}

export function Banner({
  title,
  detail,
  tone = 'danger',
  action,
}: {
  title: string
  detail?: ReactNode
  tone?: 'danger' | 'warn'
  action?: ReactNode
}) {
  return (
    <div className={styles.banner} data-tone={tone} role="alert">
      <div className={styles.bannerText}>
        <span className={styles.bannerTitle}>{title}</span>
        {detail && <span className={styles.bannerDetail}>{detail}</span>}
      </div>
      {action}
    </div>
  )
}

export function Skeleton({ height = '1.5rem', width = '100%' }: { height?: string; width?: string }) {
  return <div className={styles.skeleton} style={{ height, width }} aria-hidden="true" />
}

/** Marks a sample the data layer could not refresh, so nothing reads as current. */
export function StaleMark({ at }: { at?: number }) {
  const seconds = at ? Math.round((Date.now() - at) / 1000) : undefined
  return (
    <span className={styles.stale}>
      Eski veri{seconds !== undefined ? ` · ${seconds} sn` : ''}
    </span>
  )
}

export function Empty({ children }: { children: ReactNode }) {
  return <p className={styles.empty}>{children}</p>
}
