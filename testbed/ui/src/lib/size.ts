/* A size on disk as every row says it: 1.2 GB, 48 MB, 5.1 kB, 312 B, and a
 * dash for one not known yet. A value and its unit never part at a line
 * break: they are joined by a no-break space. */
export const NBSP = ' '

export function sizeText(bytes: number | null | undefined): string {
  if (bytes === null || bytes === undefined) return '—'
  for (const [unit, scale] of [['GB', 1e9], ['MB', 1e6], ['kB', 1e3]] as const) {
    if (bytes >= scale) {
      const v = bytes / scale
      return `${v < 10 ? v.toFixed(1) : v.toFixed(0)}${NBSP}${unit}`
    }
  }
  return `${bytes}${NBSP}B`
}

const UNIT = /(\d)[ ](?=(?:dBi|dBm|dB|GHz|MHz|kHz|Hz|km|cm|mm|m|TB|GB|MB|kB|B|ms|s|min|h|nodes?|pairs?|cells?|files?)(?![\p{L}\d]))/gu

/** Text from elsewhere (an index, a source file) with each number joined
 *  to the unit after it. */
export function keepUnits(text: string): string {
  return text.replace(UNIT, `$1${NBSP}`)
}
