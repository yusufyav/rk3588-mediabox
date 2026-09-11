import type { ReactNode } from 'react'
import { ROUTE_LABELS, ROUTES, type RouteId } from '../nav/router'
import { Button } from './Button'
import { StatusPill } from './Status'
import styles from './Shell.module.css'

export function Shell({
  route,
  onNavigate,
  onRefresh,
  connectionLabel,
  connectionTone,
  streamLabel,
  mock,
  children,
}: {
  route: RouteId
  onNavigate: (route: RouteId) => void
  onRefresh: () => void
  connectionLabel: string
  connectionTone: 'ok' | 'warn' | 'danger'
  streamLabel: string
  mock: boolean
  children: ReactNode
}) {
  return (
    <div className={styles.shell}>
      {/*
        The sidebar comes first in DOM order so a remote starts on navigation and
        moves right into the page, matching how the layout reads on screen.
      */}
      <aside className={styles.sidebar}>
        <div className={styles.brand}>
          <span className={styles.brandName}>MediaBox</span>
          <span className={styles.brandSub}>RK3588</span>
        </div>

        <nav className={styles.nav} aria-label="Ana menü">
          {ROUTES.map((id) => (
            <button
              key={id}
              type="button"
              data-focusable=""
              className={styles.navItem}
              aria-current={route === id ? 'page' : undefined}
              onClick={() => onNavigate(id)}
            >
              {ROUTE_LABELS[id]}
              {id === 'stremio' && <span className={styles.navBadge}>Yakında</span>}
            </button>
          ))}
        </nav>

        <div className={styles.sidebarFooter}>
          <span>Bağlantı: {connectionLabel}</span>
          <span>Güncelleme: {streamLabel}</span>
        </div>
      </aside>

      <div className={styles.main}>
        <header className={styles.topbar}>
          <h1 className={styles.pageTitle}>{ROUTE_LABELS[route]}</h1>
          <div className={styles.topbarRight}>
            {mock && <span className={styles.mockNotice}>Mock mod</span>}
            <StatusPill tone={connectionTone}>{connectionLabel}</StatusPill>
            <Button variant="quiet" onClick={onRefresh}>
              Yenile
            </Button>
          </div>
        </header>

        <main className={styles.content}>{children}</main>
      </div>
    </div>
  )
}
