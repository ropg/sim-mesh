/* A size on disk as every row says it: 1.2 GB, 48 MB, 5.1 kB, 312 B, and a
 * dash for one not known yet. */
export function sizeText(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined) return '—'
  for (const [unit, scale] of [['GB', 1e9], ['MB', 1e6], ['kB', 1e3]] as const) {
    if (bytes >= scale) {
      const v = bytes / scale
      return `${v < 10 ? v.toFixed(1) : v.toFixed(0)} ${unit}`
    }
  }
  return `${bytes} B`
}
