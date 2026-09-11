import { describe, expect, it } from 'vitest'
import { fireEvent, screen, waitFor } from '@testing-library/react'
import { renderApp, StubTransport, stubResponses } from './helpers'
import { ENDPOINTS } from '../src/api/endpoints'
import * as fx from '../src/mocks/fixtures'

async function openSettings(transport: StubTransport) {
  renderApp(transport)
  fireEvent.click(await screen.findByRole('button', { name: 'Ayarlar' }))
  return screen.findByRole('button', { name: 'Cihazı yeniden başlat' })
}

/** Scoped lookup: the page and the dialog deliberately share visible labels. */
function inDialog(root: HTMLElement, label: string): HTMLElement {
  const match = Array.from(root.querySelectorAll('button')).find(
    (button) => button.textContent?.trim() === label,
  )
  if (!match) throw new Error(`Dialog içinde "${label}" düğmesi yok`)
  return match
}

describe('Sistem gücü onayı', () => {
  it('never reboots on a single click: a confirmation dialog is required', async () => {
    const transport = new StubTransport(stubResponses())
    fireEvent.click(await openSettings(transport))

    expect(await screen.findByRole('alertdialog')).toBeDefined()
    expect(screen.getByText('Cihaz yeniden başlatılsın mı?')).toBeDefined()
    expect(transport.calledPaths('POST')).not.toContain(ENDPOINTS.systemReboot)
  })

  it('sends the reboot only after the dialog is confirmed', async () => {
    const transport = new StubTransport(stubResponses())
    fireEvent.click(await openSettings(transport))

    const dialog = await screen.findByRole('alertdialog')
    fireEvent.click(inDialog(dialog, 'Yeniden başlat'))

    await waitFor(() => expect(transport.calledPaths('POST')).toContain(ENDPOINTS.systemReboot))
  })

  it('cancels a shutdown without touching the device', async () => {
    const transport = new StubTransport(stubResponses())
    await openSettings(transport)
    fireEvent.click(screen.getByRole('button', { name: 'Cihazı kapat' }))

    const dialog = await screen.findByRole('alertdialog')
    fireEvent.click(inDialog(dialog, 'Vazgeç'))

    await waitFor(() => expect(screen.queryByRole('alertdialog')).toBeNull())
    expect(transport.calledPaths('POST')).not.toContain(ENDPOINTS.systemShutdown)
  })

  it('focuses cancel so a stray Enter cannot power the box down', async () => {
    await openSettings(new StubTransport(stubResponses()))
    fireEvent.click(screen.getByRole('button', { name: 'Cihazı kapat' }))

    const dialog = await screen.findByRole('alertdialog')
    expect(document.activeElement).toBe(inDialog(dialog, 'Vazgeç'))
  })

  it('disables power actions the backend does not report as available', async () => {
    const health = { ...fx.health, actions: { kodiRestart: true } }
    const transport = new StubTransport({ ...stubResponses(), [ENDPOINTS.health]: health })
    const reboot = await openSettings(transport)

    expect(reboot.hasAttribute('disabled')).toBe(true)
    expect(reboot.getAttribute('title')).toBe('Bu işlemi MediaBox servisi bildirmiyor')
  })

  it('honours an explicit disabled flag with the reason the backend gave', async () => {
    const health = {
      ...fx.health,
      actions: { reboot: false, reason: 'polkit kuralı bu işlemi engelliyor' },
    }
    const transport = new StubTransport({ ...stubResponses(), [ENDPOINTS.health]: health })
    const reboot = await openSettings(transport)

    expect(reboot.hasAttribute('disabled')).toBe(true)
    expect(reboot.getAttribute('title')).toBe('polkit kuralı bu işlemi engelliyor')
  })

  it('disables every control when the backend is unreachable', async () => {
    const transport = new StubTransport(stubResponses())
    transport.request = async () => {
      throw Object.assign(new Error('offline'), { unreachable: true, name: 'ApiError' })
    }
    renderApp(transport)
    fireEvent.click(await screen.findByRole('button', { name: 'Ayarlar' }))

    const reboot = await screen.findByRole('button', { name: 'Cihazı yeniden başlat' })
    expect(reboot.hasAttribute('disabled')).toBe(true)
    expect(reboot.getAttribute('title')).toBe('MediaBox servisine ulaşılamıyor')
  })

  it('drives the Kodi service endpoints from the settings page', async () => {
    const transport = new StubTransport(stubResponses())
    await openSettings(transport)

    // Controls lock while a command is in flight, so the second click has to
    // wait for the first to settle.
    fireEvent.click(screen.getByRole('button', { name: 'Kodi servisini başlat' }))
    await waitFor(() => expect(transport.calledPaths('POST')).toContain(ENDPOINTS.kodiServiceStart))

    const restart = screen.getByRole('button', { name: 'Kodi servisini yeniden başlat' })
    await waitFor(() => expect(restart.hasAttribute('disabled')).toBe(false))
    fireEvent.click(restart)

    await waitFor(() => expect(transport.calledPaths('POST')).toContain(ENDPOINTS.kodiServiceRestart))
    expect(transport.calledPaths('POST')).not.toContain(ENDPOINTS.systemReboot)
  })
})
