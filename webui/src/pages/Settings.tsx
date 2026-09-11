import { useState } from 'react'
import type { ActionAvailability } from '../api/types'
import { Button } from '../components/Button'
import { Card, Row, Rows } from '../components/Card'
import { ConfirmDialog } from '../components/ConfirmDialog'
import { Banner, KodiPill, StatusPill } from '../components/Status'
import { orUnknown } from '../lib/format'
import { useDevice, useDeviceState } from '../store/DeviceProvider'
import styles from './Pages.module.css'

type ActionKey = keyof Omit<ActionAvailability, 'reason'>

/**
 * Capability policy.
 *
 * Kodi service control is part of the frozen M1A contract, so it is assumed
 * available unless the backend explicitly reports `false`. Power actions are a
 * contract extension, so they stay disabled until the backend explicitly
 * reports `true` — the UI never offers a reboot it cannot perform.
 */
const DEFAULT_WHEN_UNREPORTED: Record<ActionKey, boolean> = {
  kodiStart: true,
  kodiStop: true,
  kodiRestart: true,
  reboot: false,
  shutdown: false,
}

interface PendingConfirm {
  title: string
  message: string
  confirmLabel: string
  run: () => Promise<void>
  name: string
}

export function Settings() {
  const { store, api } = useDevice()
  const state = useDeviceState()
  const [confirm, setConfirm] = useState<PendingConfirm | null>(null)

  const reported = state.health.data?.actions
  const reachable = state.backendReachable && !state.health.error
  const busy = state.pendingAction !== undefined

  function availability(key: ActionKey): { enabled: boolean; reason?: string } {
    if (!reachable) return { enabled: false, reason: 'MediaBox servisine ulaşılamıyor' }
    const value = reported?.[key]
    if (value === true) return { enabled: true }
    if (value === false) {
      return { enabled: false, reason: reported?.reason ?? 'Bu işlem cihaz tarafından kapatıldı' }
    }
    return DEFAULT_WHEN_UNREPORTED[key]
      ? { enabled: true }
      : { enabled: false, reason: 'Bu işlemi MediaBox servisi bildirmiyor' }
  }

  function action(key: ActionKey, name: string, call: () => Promise<void>) {
    const { enabled, reason } = availability(key)
    return {
      disabled: !enabled || busy,
      disabledReason: enabled ? undefined : reason,
      onClick: () => void store.runAction(name, call),
    }
  }

  function confirmAction(key: ActionKey, pending: PendingConfirm) {
    const { enabled, reason } = availability(key)
    return {
      disabled: !enabled || busy,
      disabledReason: enabled ? undefined : reason,
      onClick: () => setConfirm(pending),
    }
  }

  return (
    <div className={styles.stack}>
      {!reachable && (
        <Banner
          title="MediaBox servisine ulaşılamıyor"
          detail="Kontroller, servis yeniden yanıt verene kadar devre dışı bırakıldı."
        />
      )}

      {state.lastActionError && (
        <Banner
          title="İşlem başarısız"
          detail={state.lastActionError.message}
          action={
            <Button variant="quiet" aria-label="Hata mesajını kapat" onClick={store.clearActionError}>
              Kapat
            </Button>
          }
        />
      )}

      <Card title="Kodi servisi" aside={<KodiPill state={state.kodi.data?.state} />}>
        <Rows>
          <Row
            label="Servis durumu"
            value={
              <StatusPill tone={state.kodi.data?.serviceActive ? 'ok' : 'danger'}>
                {state.kodi.data?.serviceActive ? 'active' : 'inactive'}
              </StatusPill>
            }
          />
          <Row label="Sürüm" value={orUnknown(state.kodi.data?.version)} />
        </Rows>
        <div className={styles.actionGroup}>
          {/*
            The visible labels stay short, but the accessible names spell out the
            target: "Yeniden başlat" appears twice on this page and a screen
            reader or a remote must not have to infer which one reboots the box.
          */}
          <Button
            variant="primary"
            aria-label="Kodi servisini başlat"
            {...action('kodiStart', 'kodi-start', api.startKodi)}
          >
            Başlat
          </Button>
          <Button aria-label="Kodi servisini durdur" {...action('kodiStop', 'kodi-stop', api.stopKodi)}>
            Durdur
          </Button>
          <Button
            aria-label="Kodi servisini yeniden başlat"
            {...action('kodiRestart', 'kodi-restart', api.restartKodi)}
          >
            Yeniden başlat
          </Button>
        </div>
      </Card>

      <Card title="Sistem">
        <p className={styles.note}>
          Bu işlemler cihazı kapatır veya yeniden başlatır; devam eden oynatma sonlanır.
        </p>
        <div className={styles.actionGroup}>
          <Button
            variant="danger"
            aria-label="Cihazı yeniden başlat"
            {...confirmAction('reboot', {
              name: 'reboot',
              title: 'Cihaz yeniden başlatılsın mı?',
              message:
                'MediaBox yeniden başlatılacak. Oynatma duracak ve cihaz yaklaşık bir dakika erişilemez olacak.',
              confirmLabel: 'Yeniden başlat',
              run: api.reboot,
            })}
          >
            Yeniden başlat
          </Button>
          <Button
            variant="danger"
            aria-label="Cihazı kapat"
            {...confirmAction('shutdown', {
              name: 'shutdown',
              title: 'Cihaz kapatılsın mı?',
              message:
                'MediaBox kapatılacak. Yeniden açmak için cihazın güç düğmesini kullanmanız gerekir.',
              confirmLabel: 'Kapat',
              run: api.shutdown,
            })}
          >
            Kapat
          </Button>
        </div>
      </Card>

      <Card title="Servis bilgisi">
        <Rows>
          <Row label="mediaboxd" value={orUnknown(state.health.data?.version)} mono />
          <Row
            label="Sağlık"
            value={
              <StatusPill tone={state.health.data?.status === 'ok' ? 'ok' : 'warn'}>
                {orUnknown(state.health.data?.status)}
              </StatusPill>
            }
          />
          <Row
            label="Canlı güncelleme"
            value={
              <StatusPill tone={state.streamConnected ? 'ok' : 'warn'}>
                {state.streamConnected ? 'Olay akışı' : 'Yoklama'}
              </StatusPill>
            }
          />
        </Rows>
      </Card>

      {confirm && (
        <ConfirmDialog
          title={confirm.title}
          message={confirm.message}
          confirmLabel={confirm.confirmLabel}
          onCancel={() => setConfirm(null)}
          onConfirm={() => {
            const pending = confirm
            setConfirm(null)
            void store.runAction(pending.name, pending.run)
          }}
        />
      )}
    </div>
  )
}
