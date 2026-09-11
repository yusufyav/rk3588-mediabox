import { useEffect, useRef } from 'react'
import { BACK_EVENT } from '../nav/useRemoteNavigation'
import { Button } from './Button'
import styles from './ConfirmDialog.module.css'

export interface ConfirmDialogProps {
  title: string
  message: string
  confirmLabel: string
  cancelLabel?: string
  danger?: boolean
  onConfirm: () => void
  onCancel: () => void
}

/**
 * Confirmation gate for destructive actions (reboot / shutdown).
 *
 * `data-nav-scope="dialog"` tells the remote navigator to consider only the
 * dialog's controls while it is open — the back key always dismisses it, so the
 * user is never stuck inside.
 */
export function ConfirmDialog({
  title,
  message,
  confirmLabel,
  cancelLabel = 'Vazgeç',
  danger = true,
  onConfirm,
  onCancel,
}: ConfirmDialogProps) {
  const cancelRef = useRef<HTMLButtonElement>(null)

  useEffect(() => {
    // Cancel takes focus: an accidental Enter must not reboot the box.
    cancelRef.current?.focus()
    const onBack = () => onCancel()
    window.addEventListener(BACK_EVENT, onBack)
    return () => window.removeEventListener(BACK_EVENT, onBack)
  }, [onCancel])

  return (
    <div className={styles.backdrop} onPointerDown={(e) => e.target === e.currentTarget && onCancel()}>
      <div
        className={styles.dialog}
        role="alertdialog"
        aria-modal="true"
        aria-labelledby="confirm-title"
        aria-describedby="confirm-message"
        data-nav-scope="dialog"
      >
        <h2 className={styles.title} id="confirm-title">
          {title}
        </h2>
        <p className={styles.message} id="confirm-message">
          {message}
        </p>
        <div className={styles.actions}>
          <Button ref={cancelRef} variant="quiet" onClick={onCancel}>
            {cancelLabel}
          </Button>
          <Button variant={danger ? 'danger' : 'primary'} onClick={onConfirm}>
            {confirmLabel}
          </Button>
        </div>
      </div>
    </div>
  )
}
