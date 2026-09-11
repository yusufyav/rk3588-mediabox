import { useEffect, useRef, type ReactNode } from 'react'
import styles from './Panel.module.css'

/**
 * A surface the appliance layer puts over the media experience.
 *
 * While one is open, remote navigation is scoped to it (`data-nav-scope`), so
 * arrow keys cannot drive a control hidden behind it. Back always dismisses it,
 * so the scoping never becomes a trap.
 */
export function Panel({
  title,
  onClose,
  children,
  width = 'wide',
}: {
  title: string
  onClose: () => void
  children: ReactNode
  width?: 'wide' | 'narrow'
}) {
  const container = useRef<HTMLDivElement>(null)

  useEffect(() => {
    // Take focus so a remote lands inside the panel rather than behind it.
    const first = container.current?.querySelector<HTMLElement>('[data-focusable]')
    first?.focus()
  }, [])

  return (
    <div className={styles.scrim} onClick={onClose}>
      <aside
        ref={container}
        className={styles.panel}
        data-width={width}
        data-nav-scope="dialog"
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onClick={(event) => event.stopPropagation()}
      >
        <header className={styles.head}>
          <h2 className={styles.title}>{title}</h2>
          <button
            type="button"
            data-focusable=""
            className={styles.close}
            aria-label="Kapat"
            onClick={onClose}
          >
            ✕
          </button>
        </header>
        <div className={styles.body}>{children}</div>
      </aside>
    </div>
  )
}
