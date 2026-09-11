import { forwardRef, type ButtonHTMLAttributes, type ReactNode } from 'react'
import styles from './Button.module.css'

type Variant = 'default' | 'primary' | 'danger' | 'quiet'

export interface ButtonProps extends ButtonHTMLAttributes<HTMLButtonElement> {
  variant?: Variant
  size?: 'default' | 'large'
  children: ReactNode
  /** Explains a disabled state to the user instead of leaving a dead control. */
  disabledReason?: string
}

/**
 * Every interactive control in the UI is this button, which is also what makes
 * it a remote-navigation candidate (`data-focusable`).
 */
export const Button = forwardRef<HTMLButtonElement, ButtonProps>(function Button(
  { variant = 'default', size = 'default', className, disabled, disabledReason, children, ...rest },
  ref,
) {
  const classes = [
    styles.button,
    variant !== 'default' ? styles[variant] : '',
    size === 'large' ? styles.large : '',
    className ?? '',
  ]
    .filter(Boolean)
    .join(' ')

  return (
    <button
      type="button"
      data-focusable=""
      ref={ref}
      className={classes}
      disabled={disabled}
      title={disabled && disabledReason ? disabledReason : rest.title}
      {...rest}
    >
      {children}
    </button>
  )
})
