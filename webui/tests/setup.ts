import { afterEach } from 'vitest'
import { cleanup } from '@testing-library/react'

afterEach(() => {
  cleanup()
  document.documentElement.removeAttribute('data-input')
  document.documentElement.removeAttribute('data-layout')
  if (typeof location !== 'undefined') location.hash = ''
})
