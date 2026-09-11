import type { ReactNode } from 'react'
import styles from './Card.module.css'

export function Card({
  title,
  aside,
  children,
  className,
}: {
  title?: string
  aside?: ReactNode
  children: ReactNode
  className?: string
}) {
  return (
    <section className={[styles.card, className ?? ''].filter(Boolean).join(' ')}>
      {(title || aside) && (
        <header className={styles.header}>
          {title && <h2 className={styles.title}>{title}</h2>}
          {aside}
        </header>
      )}
      <div className={styles.body}>{children}</div>
    </section>
  )
}

export function Metric({
  label,
  value,
  hint,
}: {
  label: string
  value: ReactNode
  hint?: ReactNode
}) {
  return (
    <div className={styles.metric}>
      <span className={styles.metricLabel}>{label}</span>
      <span className={styles.metricValue}>{value}</span>
      {hint && <span className={styles.metricHint}>{hint}</span>}
    </div>
  )
}

export function Rows({ children }: { children: ReactNode }) {
  return <div className={styles.rows}>{children}</div>
}

export function Row({
  label,
  value,
  mono,
}: {
  label: string
  value: ReactNode
  mono?: boolean
}) {
  return (
    <div className={styles.row}>
      <span className={styles.rowLabel}>{label}</span>
      <span className={[styles.rowValue, mono ? styles.mono : ''].filter(Boolean).join(' ')}>
        {value}
      </span>
    </div>
  )
}

/** Usage bar; turns amber past 75% and red past 90% so a full disk is obvious. */
export function UsageBar({ percent }: { percent: number }) {
  const clamped = Math.min(Math.max(percent, 0), 100)
  const tone = clamped >= 90 ? 'danger' : clamped >= 75 ? 'warn' : 'ok'
  return (
    <div className={styles.bar} role="presentation">
      <div className={styles.barFill} data-tone={tone} style={{ width: `${clamped}%` }} />
    </div>
  )
}
