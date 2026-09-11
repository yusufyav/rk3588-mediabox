const UNKNOWN = '—'

export function formatBytes(bytes?: number): string {
  if (bytes === undefined || !Number.isFinite(bytes)) return UNKNOWN
  const units = ['B', 'KB', 'MB', 'GB', 'TB']
  let value = bytes
  let unit = 0
  while (value >= 1024 && unit < units.length - 1) {
    value /= 1024
    unit += 1
  }
  const decimals = value >= 100 || unit === 0 ? 0 : 1
  return `${value.toFixed(decimals)} ${units[unit]}`
}

/** Playback clock: h:mm:ss once the item is an hour long, m:ss below that. */
export function formatClock(seconds?: number): string {
  if (seconds === undefined || !Number.isFinite(seconds) || seconds < 0) return '--:--'
  const total = Math.floor(seconds)
  const h = Math.floor(total / 3600)
  const m = Math.floor((total % 3600) / 60)
  const s = total % 60
  const pad = (n: number) => String(n).padStart(2, '0')
  return h > 0 ? `${h}:${pad(m)}:${pad(s)}` : `${m}:${pad(s)}`
}

/** Uptime in human terms; a media box is expected to run for weeks. */
export function formatUptime(seconds?: number): string {
  if (seconds === undefined || !Number.isFinite(seconds) || seconds < 0) return UNKNOWN
  const days = Math.floor(seconds / 86_400)
  const hours = Math.floor((seconds % 86_400) / 3600)
  const minutes = Math.floor((seconds % 3600) / 60)
  if (days > 0) return `${days} gün ${hours} sa`
  if (hours > 0) return `${hours} sa ${minutes} dk`
  return `${minutes} dk`
}

export function formatTemperature(celsius?: number): string {
  if (celsius === undefined || !Number.isFinite(celsius)) return UNKNOWN
  return `${celsius.toFixed(1)} °C`
}

export function percentOf(used?: number, total?: number): number | undefined {
  if (used === undefined || total === undefined || total <= 0) return undefined
  return (used / total) * 100
}

export function formatPercent(value?: number): string {
  if (value === undefined || !Number.isFinite(value)) return UNKNOWN
  return `%${value.toFixed(0)}`
}

export function orUnknown(value?: string | number | null): string {
  if (value === undefined || value === null || value === '') return UNKNOWN
  return String(value)
}

/** CPU temperature tone thresholds for the RK3588 under a media workload. */
export function temperatureTone(celsius?: number): 'ok' | 'warn' | 'danger' | 'neutral' {
  if (celsius === undefined) return 'neutral'
  if (celsius >= 85) return 'danger'
  if (celsius >= 70) return 'warn'
  return 'ok'
}
