/* The map's ground, made off the page's main thread: what lib/planner.ts
 * fetches is decoded and composed here, and comes back as an ImageBitmap to
 * draw and, for the base map, the terrain under it; a footprint reply comes
 * back as its rings' points, absolute, in one array.
 *
 * A base image is tile.bin's terrain with the server's bake of the layer
 * (basemap.bin) on it. planner-wasm's compose of a Tile given that bake,
 * with nothing drawn over it, is the bake itself (Tile::compose_base clones
 * it), so the bake is handed on as it came; planner-wasm bakes the terrain
 * itself only where the server's image did not come or does not fit the
 * tile. A population heatmap is the tile's residents per cell, coloured.
 * Either is put into an OffscreenCanvas as the page put it into a canvas,
 * and handed over whole: the page draws the same pixels it drew before. */
import init, { Tile } from 'planner-wasm'
import wasmUrl from 'planner-wasm/planner_wasm_bg.wasm?url'
import type { Box, Grid } from './planner'
import { decodeTile, magic } from './tile'

/** What the page asks: a base image, a population heatmap, or footprints read. */
export type GroundAsk =
  | { kind: 'base'; tile: ArrayBuffer; basemap: ArrayBuffer | null; layer: number }
  | { kind: 'population'; tile: ArrayBuffer }
  | { kind: 'footprints'; reply: ArrayBuffer }

/** A buildings.bin reply read: per ring its building's id, its point count,
 *  its building's roof and its box (minx, miny, maxx, maxy), and every
 *  ring's points one after another, x and y in absolute metres. */
export interface Rings {
  truncated: boolean
  ids: Uint32Array; lens: Uint32Array; tops: Float32Array; boxes: Float64Array; points: Float64Array
}

export type GroundJob = GroundAsk & { id: number }

export type GroundDone =
  | { id: number; kind: 'base'; bitmap: ImageBitmap; bounds: Box; grid: Grid }
  | { id: number; kind: 'population'; bitmap: ImageBitmap | null; bounds: Box }
  | { id: number; kind: 'footprints'; rings: Rings }
  | { id: number; kind: 'error'; error: string }

const scope = self as unknown as Worker

let ready: Promise<unknown> | null = null
/** planner-wasm, instantiated the first time a bake is wanted. */
function wasm(): Promise<unknown> {
  if (!ready) ready = init({ module_or_path: wasmUrl })
  return ready
}

/** Pixels into a bitmap, through a canvas as the page's own were. */
function bitmapOf(rgba: Uint8ClampedArray<ArrayBuffer>, w: number, h: number): ImageBitmap {
  const canvas = new OffscreenCanvas(w, h)
  canvas.getContext('2d')!.putImageData(new ImageData(rgba, w, h), 0, 0)
  return canvas.transferToImageBitmap()
}

async function base(job: Extract<GroundJob, { kind: 'base' }>): Promise<GroundDone> {
  const d = decodeTile(job.tile)
  let rgba: Uint8ClampedArray<ArrayBuffer> | null = null
  if (job.basemap) {
    const dv = new DataView(job.basemap)
    if (magic(dv) === 'PBM1' && dv.getUint32(4, true) === d.w && dv.getUint32(8, true) === d.h
        && job.basemap.byteLength >= 44 + d.w * d.h * 4) {
      rgba = new Uint8ClampedArray(job.basemap, 44, d.w * d.h * 4)
    }
  }
  // The cells out to their edges, in Tile::outer_bounds's arithmetic, step for step.
  const x1 = d.ox + d.rx * (d.w - 1), y1 = d.oy + -d.ry * (d.h - 1)
  const hx = Math.abs(d.rx) * 0.5, hy = Math.abs(-d.ry) * 0.5
  let outer = { minx: Math.min(d.ox, x1) - hx, maxx: Math.max(d.ox, x1) + hx,
                miny: Math.min(y1, d.oy) - hy, maxy: Math.max(d.oy, y1) + hy }
  if (!rgba) {
    await wasm()
    const tile = new Tile(d.ox, d.oy, d.rx, d.ry, d.w, d.h, d.terrain)
    try {
      const out = tile.compose_base(job.layer, false, false)
      const [minx, miny, maxx, maxy] = tile.outer_bounds() as unknown as number[]
      outer = { minx: minx!, miny: miny!, maxx: maxx!, maxy: maxy! }
      rgba = new Uint8ClampedArray(out.buffer as ArrayBuffer, out.byteOffset, out.length)
    } finally {
      tile.free()
    }
  }
  return {
    id: job.id, kind: 'base', bitmap: bitmapOf(rgba, d.w, d.h), bounds: outer,
    grid: { w: d.w, h: d.h, ox: d.ox, oy: d.oy, rx: d.rx, ry: d.ry, terrain: d.terrain },
  }
}

/* Residents per cell of the pack's population raster, log-scaled as the
 * planner's own map scales them (density spans orders of magnitude): a
 * trace is a faint violet, a dense block a bright orange-yellow. */
function populationColour(v: number): [number, number, number, number] | null {
  if (!(v > 0.02)) return null
  const f = Math.min(1, Math.log(v + 1) / Math.log(6))
  const r = Math.round(120 + f * 135), g = Math.round(40 + f * 170), b = Math.round(170 - f * 130)
  return [r, g, b, Math.round(70 + f * 150)]
}

function population(job: Extract<GroundJob, { kind: 'population' }>): GroundDone {
  const d = decodeTile(job.tile)
  // The origin is the top-left cell's centre: the image covers half a cell more.
  const bounds = { minx: d.ox - d.rx / 2, maxx: d.ox + (d.w - 0.5) * d.rx,
                   maxy: d.oy + d.ry / 2, miny: d.oy - (d.h - 0.5) * d.ry }
  if (!d.population) return { id: job.id, kind: 'population', bitmap: null, bounds }
  const rgba = new Uint8ClampedArray(d.w * d.h * 4)
  for (let i = 0; i < d.w * d.h; i++) {
    const colour = populationColour(d.population[i]!)
    if (colour) rgba.set(colour, i * 4)
  }
  return { id: job.id, kind: 'population', bitmap: bitmapOf(rgba, d.w, d.h), bounds }
}

/* buildings.bin: "PBO3" | u32 rings | u8 truncated | f64 ox | f64 oy | per
 * ring: u32 id, u32 n, f32 top, f32 loss, f32 census, n × (f32 dx, f32 dy)
 * from (ox, oy). A ring is seldom 4-byte aligned in it, so it is copied to
 * be read as floats, as the page read it. */
function footprints(job: Extract<GroundJob, { kind: 'footprints' }>): GroundDone {
  const buf = job.reply, dv = new DataView(buf)
  if (magic(dv) !== 'PBO3') throw new Error('not a footprint reply')
  const count = dv.getUint32(4, true)
  const ox = dv.getFloat64(9, true), oy = dv.getFloat64(17, true)
  const ids = new Uint32Array(count), lens = new Uint32Array(count), tops = new Float32Array(count)
  const boxes = new Float64Array(count * 4)
  let total = 0
  for (let i = 0, off = 25; i < count; i++) {
    const n = dv.getUint32(off + 4, true)
    total += n
    off += 20 + n * 8
  }
  const points = new Float64Array(total * 2)
  for (let i = 0, off = 25, at = 0; i < count; i++) {
    ids[i] = dv.getUint32(off, true)
    const n = lens[i] = dv.getUint32(off + 4, true)
    tops[i] = dv.getFloat32(off + 8, true)
    off += 20
    const rel = new Float32Array(buf.slice(off, off + n * 8))
    off += n * 8
    let minx = Infinity, miny = Infinity, maxx = -Infinity, maxy = -Infinity
    for (let k = 0; k < n; k++, at += 2) {
      const px = ox + rel[k * 2]!, py = oy + rel[k * 2 + 1]!
      points[at] = px
      points[at + 1] = py
      if (px < minx) minx = px
      if (px > maxx) maxx = px
      if (py < miny) miny = py
      if (py > maxy) maxy = py
    }
    boxes.set([minx, miny, maxx, maxy], i * 4)
  }
  return { id: job.id, kind: 'footprints', rings: { truncated: !!dv.getUint8(8), ids, lens, tops, boxes, points } }
}

scope.onmessage = async (event: MessageEvent<GroundJob>) => {
  const job = event.data
  try {
    const done = job.kind === 'base' ? await base(job) : job.kind === 'population' ? population(job) : footprints(job)
    const moved: Transferable[] = done.kind === 'base' || done.kind === 'population'
      ? (done.bitmap ? [done.bitmap] : []) : []
    if (done.kind === 'base') moved.push(done.grid.terrain.buffer as ArrayBuffer)
    if (done.kind === 'footprints') {
      const r = done.rings
      moved.push(...[r.ids, r.lens, r.tops, r.boxes, r.points].map(a => a.buffer as ArrayBuffer))
    }
    scope.postMessage(done, moved)
  } catch (e) {
    scope.postMessage({ id: job.id, kind: 'error', error: (e as Error).message ?? String(e) } satisfies GroundDone)
  }
}
