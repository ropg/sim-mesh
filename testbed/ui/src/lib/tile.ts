/* planner-web's tile.bin, read: the page's own for a point (lib/planner.ts
 * `sample`), the ground worker's for an image (lib/ground.worker.ts).
 *
 *     "PTL2" | u32 w | u32 h | f64 ox | f64 oy | f64 rx | f64 ry | u8 flags
 *     | i16 terrain[w·h] (dm, −32768 none) | [u8 classes] (flags&1)
 *     | [u16 population·10] (flags&2) | [u16 clutter height·10] (flags&4) */

/** The four bytes a planner-web reply starts with. */
export function magic(dv: DataView): string {
  return String.fromCharCode(dv.getUint8(0), dv.getUint8(1), dv.getUint8(2), dv.getUint8(3))
}

export interface DecodedTile {
  w: number; h: number
  /** The top-left cell's centre, and the cell size; rows run south. */
  ox: number; oy: number; rx: number; ry: number
  /** Metres above sea level, NaN where there is none. */
  terrain: Float32Array
  /** Residents per cell, when the tile carries them. */
  population: Float32Array | null
  /** Clutter height in metres, when the tile carries it. */
  clutter: Float32Array | null
}

export function decodeTile(buf: ArrayBuffer): DecodedTile {
  const dv = new DataView(buf)
  if (magic(dv) !== 'PTL2') throw new Error('not a tile')
  const w = dv.getUint32(4, true), h = dv.getUint32(8, true)
  const ox = dv.getFloat64(12, true), oy = dv.getFloat64(20, true)
  const rx = dv.getFloat64(28, true), ry = dv.getFloat64(36, true)
  const flags = dv.getUint8(44)
  const n = w * h
  let off = 45
  const raw = new Int16Array(buf.slice(off, off + n * 2))
  const terrain = new Float32Array(n)
  for (let i = 0; i < n; i++) { const v = raw[i]!; terrain[i] = v === -32768 ? NaN : v * 0.1 }
  off += n * 2
  if (flags & 1) off += n
  let population: Float32Array | null = null
  if (flags & 2) {
    const u = new Uint16Array(buf.slice(off, off + n * 2))
    population = new Float32Array(n)
    for (let i = 0; i < n; i++) population[i] = u[i]! * 0.1
    off += n * 2
  }
  let clutter: Float32Array | null = null
  if (flags & 4) {
    const u = new Uint16Array(buf.slice(off, off + n * 2))
    clutter = new Float32Array(n)
    for (let i = 0; i < n; i++) clutter[i] = u[i]! * 0.1
  }
  return { w, h, ox, oy, rx, ry, terrain, population, clutter }
}
