/* The planner's sidecar, as the page uses it.
 *
 * The front runs one planner-web per pack in use and passes `/planner/<geodata>/…`
 * through to it with the prefix stripped, so every path below is planner-web's
 * own route under that prefix. Coordinates are the pack's CRS metres
 * throughout. The binary formats are planner-web's, read here as its own page
 * reads them:
 *
 *     tile.bin       "PTL2" | u32 w | u32 h | f64 ox | f64 oy | f64 rx | f64 ry | u8 flags
 *                    | i16 terrain[w·h] (dm, −32768 none) | [u8 classes] (flags&1)
 *                    | [u16 population·10] (flags&2) | [u16 clutter height·10] (flags&4)
 *     basemap.bin    "PBM1" | u32 w | u32 h | … | rgba[w·h] from byte 44
 *     buildings.bin  "PBO3" | u32 rings | u8 truncated | f64 ox | f64 oy
 *                    | per ring: u32 id, u32 n, f32 top (m above sea level), f32 loss,
 *                      f32 census, n × (f32 dx, f32 dy) from (ox, oy)
 *     roads.bin      per way: f32 class, f32 n, n × (f32 x, f32 y), no header
 *
 * The base map is tile.bin's terrain with the server's bake of a layer on it
 * (planner-wasm bakes it where the server's does not come), made in the
 * ground worker off the main thread (lib/ground.worker.ts); the page draws
 * the result with drawImage, so panning and zooming cost nothing until the
 * view leaves it. Footprints and roads are the page's own to draw. */
import type { GroundAsk, GroundDone, GroundJob } from './ground.worker'
import { decodeTile, magic, type DecodedTile } from './tile'

/** The ground under everything. Population is not one: it is a heatmap of
 *  its own over whichever ground (`populationImage`). */
export type BaseLayer = 'terrain' | 'clutter'
/** planner-wasm's layer numbers. */
const LAYER: Record<BaseLayer, number> = { terrain: 0, clutter: 2 }

export interface Box { minx: number; miny: number; maxx: number; maxy: number }

export interface PackInfo {
  name: string
  crs_epsg: number
  extent: Box
  centre: { lat: number; lon: number }
  res_m: number
  layers: string[]
  licenses: { source: string; notice: string }[]
}

export function plannerBase(geodata: string): string {
  return `/planner/${encodeURIComponent(geodata)}`
}

let worker: Worker | null = null
let jobs = 0
const working = new Map<number, { resolve: (done: GroundDone) => void; reject: (e: Error) => void }>()

/** The ground worker, started once for the page. */
function groundWorker(): Worker {
  if (worker) return worker
  worker = new Worker(new URL('./ground.worker.ts', import.meta.url), { type: 'module' })
  worker.onmessage = (event: MessageEvent<GroundDone>) => {
    const done = event.data
    const asker = working.get(done.id)
    working.delete(done.id)
    // Asked for no longer: the bitmap is let go at once rather than at a collection.
    if (!asker) { if (done.kind === 'base' || done.kind === 'population') done.bitmap?.close(); return }
    if (done.kind === 'error') asker.reject(new Error(done.error))
    else asker.resolve(done)
  }
  worker.onerror = (event) => {
    for (const asker of working.values()) asker.reject(new Error(`the ground worker: ${event.message}`))
    working.clear()
  }
  return worker
}

/** A job for the ground worker, its buffers handed over. Aborted, it
 *  rejects at once, and what the worker sends back is let go. */
function inWorker(job: GroundAsk, moved: ArrayBuffer[], signal?: AbortSignal): Promise<GroundDone> {
  const id = ++jobs
  return new Promise((resolve, reject) => {
    const abort = () => { working.delete(id); reject(new DOMException('aborted', 'AbortError')) }
    if (signal?.aborted) { abort(); return }
    signal?.addEventListener('abort', abort, { once: true })
    const done = () => signal?.removeEventListener('abort', abort)
    working.set(id, { resolve: (d) => { done(); resolve(d) }, reject: (e) => { done(); reject(e) } })
    groundWorker().postMessage({ ...job, id } satisfies GroundJob, moved)
  })
}

/** A request, asked again after a growing pause while the sidecar sheds load with 429. */
async function withBackoff(ask: () => Promise<Response>, signal?: AbortSignal): Promise<Response> {
  for (let attempt = 0; ; attempt++) {
    const r = await ask()
    if (r.status !== 429 || attempt >= 6) return r
    await new Promise(res => setTimeout(res, 150 * (attempt + 1)))
    if (signal?.aborted) throw new DOMException('aborted', 'AbortError')
  }
}

/** GET, backing off while the sidecar sheds load with 429. */
export function getWithBackoff(url: string, signal?: AbortSignal): Promise<Response> {
  return withBackoff(() => fetch(url, { signal }), signal)
}

function boxQuery(b: Box, w: number, h: number, extra: Record<string, string | number> = {}) {
  const q = new URLSearchParams()
  for (const [k, v] of Object.entries({ ...b, w, h, ...extra })) q.set(k, String(v))
  return q.toString()
}

export async function packInfo(base: string, signal?: AbortSignal): Promise<PackInfo> {
  const r = await fetch(`${base}/api/pack`, { signal })
  if (!r.ok) throw new Error(`the planner sidecar answered ${r.status}: ${(await r.text()).slice(0, 200)}`)
  return await r.json() as PackInfo
}

/** One heatmap image and the ground rectangle it covers. */
export interface Heatmap { bitmap: ImageBitmap; bounds: Box }

/** The population in `box` at `w`×`h` cells, as a heatmap (the ground
 *  worker colours it); null when the pack has no population layer. */
export async function populationImage(base: string, box: Box, w: number, h: number,
                                      signal?: AbortSignal): Promise<Heatmap | null> {
  const r = await getWithBackoff(`${base}/tile.bin?${boxQuery(box, w, h)}`, signal)
  if (!r.ok) throw new Error(`tile ${r.status}`)
  const tile = await r.arrayBuffer()
  const done = await inWorker({ kind: 'population', tile }, [tile], signal)
  if (done.kind !== 'population') throw new Error('not a heatmap')
  return done.bitmap ? { bitmap: done.bitmap, bounds: done.bounds } : null
}

/** A copy of an image in grey, for what lies under a heatmap. */
export function greyed(src: HTMLCanvasElement | ImageBitmap): HTMLCanvasElement {
  const out = document.createElement('canvas')
  out.width = src.width
  out.height = src.height
  if (!src.width || !src.height) return out
  const c = out.getContext('2d')!
  c.drawImage(src, 0, 0)
  const img = c.getImageData(0, 0, out.width, out.height)
  const p = img.data
  for (let i = 0; i < p.length; i += 4) {
    const l = Math.round(0.299 * p[i]! + 0.587 * p[i + 1]! + 0.114 * p[i + 2]!)
    p[i] = l; p[i + 1] = l; p[i + 2] = l
  }
  c.putImageData(img, 0, 0)
  return out
}

/** Terrain heights on a grid: the origin is the top-left cell's centre, rows run south. */
export interface Grid { w: number; h: number; ox: number; oy: number; rx: number; ry: number; terrain: Float32Array }

/** One composed base image, the ground rectangle it covers, and the terrain under it. */
export interface BaseImage {
  bitmap: ImageBitmap
  bounds: Box
  grid: Grid
}

/**
 * The base map for `box` at `w`×`h` cells: tile.bin for the grid, the
 * server's bake of `layer` for the pixels, made into an image in the ground
 * worker (which has planner-wasm bake them itself, from the terrain, when
 * the server's image does not come). Roads are not burnt in: baked at the
 * pack's cell size they are tens of metres wide close up, so the map draws
 * them from roads.bin as lines.
 */
export async function baseImage(base: string, box: Box, w: number, h: number,
                                layer: BaseLayer, signal?: AbortSignal): Promise<BaseImage> {
  const q = boxQuery(box, w, h, { terrain_only: 1 })
  const [tr, br] = await Promise.all([
    getWithBackoff(`${base}/tile.bin?${q}`, signal),
    getWithBackoff(`${base}/basemap.bin?${q}&layer=${LAYER[layer]}&roads=0`, signal)
      .catch((e: unknown) => { if ((e as Error).name === 'AbortError') throw e; return null }),
  ])
  if (!tr.ok) throw new Error(`tile ${tr.status}`)
  const [tile, basemap] = await Promise.all([tr.arrayBuffer(), br && br.ok ? br.arrayBuffer() : null])
  const done = await inWorker({ kind: 'base', tile, basemap, layer: LAYER[layer] },
                              basemap ? [tile, basemap] : [tile], signal)
  if (done.kind !== 'base') throw new Error('not a base image')
  return { bitmap: done.bitmap, bounds: done.bounds, grid: done.grid }
}

/** A road or railway: its class (0 motorway, 1 trunk, 2 primary, 3 secondary,
 *  4 rail) and its points in absolute metres. */
export interface Way { cls: number; pts: Float32Array; box: Box }

/** The ways in `box`. The sidecar keeps whole ways in the pack's order up to
 *  a byte budget, so a view of a whole city gets its first share of them. */
export async function roads(base: string, box: Box, signal?: AbortSignal): Promise<Way[]> {
  const r = await getWithBackoff(`${base}/roads.bin?${boxQuery(box, 1, 1)}`, signal)
  if (!r.ok) throw new Error(`roads ${r.status}`)
  const all = new Float32Array(await r.arrayBuffer())
  const ways: Way[] = []
  for (let at = 0; at + 2 <= all.length;) {
    const cls = all[at]!, n = all[at + 1]!
    const pts = all.subarray(at + 2, at + 2 + n * 2)
    at += 2 + n * 2
    const b = { minx: Infinity, miny: Infinity, maxx: -Infinity, maxy: -Infinity }
    for (let k = 0; k < n; k++) {
      const x = pts[k * 2]!, y = pts[k * 2 + 1]!
      if (x < b.minx) b.minx = x
      if (x > b.maxx) b.maxx = x
      if (y < b.miny) b.miny = y
      if (y > b.maxy) b.maxy = y
    }
    ways.push({ cls, pts, box: b })
  }
  return ways
}

/** What stands at a point: the ground and the clutter height there. */
export interface Sample { ground: number; clutter: number | null }

/** The box a point's tile is asked for: two metres round it, one cell asked. */
const SAMPLE_R = 2

/** A point's values off the tile asked for it: its nearest cell's. */
function sampleOf(d: DecodedTile, x: number, y: number): Sample {
  /* The origin is the top-left cell's centre, rows running south. */
  const col = Math.min(d.w - 1, Math.max(0, Math.round((x - d.ox) / Math.abs(d.rx))))
  const row = Math.min(d.h - 1, Math.max(0, Math.round((d.oy - y) / Math.abs(d.ry))))
  const k = row * d.w + col
  return { ground: d.terrain[k]!, clutter: d.clutter ? d.clutter[k]! : null }
}

/** Ground and clutter height at one point, from the pack's rasters. */
export async function sample(base: string, x: number, y: number, signal?: AbortSignal): Promise<Sample> {
  const r = SAMPLE_R
  const q = boxQuery({ minx: x - r, miny: y - r, maxx: x + r, maxy: y + r }, 1, 1)
  const res = await getWithBackoff(`${base}/tile.bin?${q}`, signal)
  if (!res.ok) throw new Error(`tile ${res.status}`)
  return sampleOf(decodeTile(await res.arrayBuffer()), x, y)
}

/** `sample` at many points, their tiles in one /tiles.bin request, each as
 *  tile.bin answers it; the box's numbers go as strings, read exactly. */
export async function samples(base: string, points: [number, number][],
                              signal?: AbortSignal): Promise<Sample[]> {
  const r = SAMPLE_R, s = String
  const tiles = points.map(([x, y]) => ({ minx: s(x - r), miny: s(y - r), maxx: s(x + r), maxy: s(y + r), w: 1, h: 1 }))
  const res = await withBackoff(() => fetch(`${base}/tiles.bin`, {
    method: 'POST', body: JSON.stringify({ tiles }), headers: { 'Content-Type': 'application/json' }, signal,
  }), signal)
  if (!res.ok) throw new Error(`tiles ${res.status}`)
  const buf = await res.arrayBuffer()
  const dv = new DataView(buf)
  if (magic(dv) !== 'PTLS' || dv.getUint32(4, true) !== points.length) throw new Error('not a tile batch')
  const out: Sample[] = []
  for (let i = 0, at = 8; i < points.length; i++) {
    const n = dv.getUint32(at, true)
    out.push(sampleOf(decodeTile(buf.slice(at + 4, at + 4 + n)), points[i]![0], points[i]![1]))
    at += 4 + n
  }
  return out
}

/** One node of a coverage view: its raster's key, where it stands, what it
 *  has to spend (its power and the receiver's gain less the decoding
 *  threshold) and its antenna's pattern and aim, null with none. */
export interface BandsNode {
  key: string; x: number; y: number; height_m: number; budget_db: number
  antenna: {
    directional: boolean; peak_dbi: number; vbw_deg: number; tilt_deg: number; hbw_deg: number | null
    floor_db: number; azimuth_deg: number; elevation_deg: number
  } | null
}

/** No band reached: below the last one's margin, or no raster there. */
export const NO_BAND = 255

/**
 * The coverage band of every `px`-pixel square of a view `w`×`h` CSS pixels
 * at `at`, over `nodes`, which the sidecar works out from their rasters in
 * the front's cache: an index into `bands` (each one's least margin, the
 * best first), NO_BAND where none is reached, rows from the top. Every
 * number goes as JavaScript writes it, in a string, which the sidecar reads
 * exactly.
 */
export async function coverageBands(base: string, ask: {
  geodata: string; at: { cx: number; cy: number; mpp: number }; w: number; h: number; px: number
  bands: number[]; rxH: number; nodes: BandsNode[]
}, signal?: AbortSignal): Promise<Uint8Array> {
  const s = String
  const body = JSON.stringify({
    geodata: ask.geodata, cx: s(ask.at.cx), cy: s(ask.at.cy), mpp: s(ask.at.mpp),
    w: s(ask.w), h: s(ask.h), px: s(ask.px), bands: ask.bands.map(s), rx_h: s(ask.rxH),
    nodes: ask.nodes.map(n => ({
      key: n.key, x: s(n.x), y: s(n.y), height_m: s(n.height_m), budget_db: s(n.budget_db),
      antenna: n.antenna && {
        directional: n.antenna.directional, peak_dbi: s(n.antenna.peak_dbi), vbw_deg: s(n.antenna.vbw_deg),
        tilt_deg: s(n.antenna.tilt_deg), hbw_deg: n.antenna.hbw_deg === null ? null : s(n.antenna.hbw_deg),
        floor_db: s(n.antenna.floor_db), azimuth_deg: s(n.antenna.azimuth_deg),
        elevation_deg: s(n.antenna.elevation_deg),
      },
    })),
  })
  const r = await withBackoff(() => fetch(`${base}/coverage/bands.bin`, {
    method: 'POST', body, headers: { 'Content-Type': 'application/json' }, signal,
  }), signal)
  if (!r.ok) throw new Error((await r.text()).trim() || `coverage ${r.status}`)
  const buf = await r.arrayBuffer()
  const dv = new DataView(buf)
  if (magic(dv) !== 'PCB1') throw new Error('not a coverage reply')
  return new Uint8Array(buf, 12, dv.getUint32(4, true) * dv.getUint32(8, true))
}

/** One building's footprint: its rings in absolute metres, its roof above sea level. */
export interface Footprint {
  id: number
  rings: Float64Array[]
  top: number
  box: Box
}

export interface Footprints {
  truncated: boolean
  list: Footprint[]
}

/** The footprints in `box`, or null when the pack has no building geometry
 *  (a pack built without `--lod2-geometry` answers 204). The reply is read
 *  in the ground worker; a building is its rings in a row, each a view of
 *  the points the worker hands over. */
export async function buildings(base: string, box: Box, signal?: AbortSignal): Promise<Footprints | null> {
  const r = await getWithBackoff(`${base}/buildings.bin?${boxQuery(box, 1, 1)}`, signal)
  if (r.status === 204) return null
  if (!r.ok) throw new Error(`buildings ${r.status}`)
  const reply = await r.arrayBuffer()
  const done = await inWorker({ kind: 'footprints', reply }, [reply], signal)
  if (done.kind !== 'footprints') throw new Error('not a footprint reply')
  const { truncated, ids, lens, tops, boxes, points } = done.rings
  const list: Footprint[] = []
  let cur: Footprint | null = null
  for (let i = 0, at = 0; i < ids.length; i++) {
    const id = ids[i]!, n = lens[i]!
    if (!cur || cur.id !== id) {
      cur = { id, rings: [], top: tops[i]!, box: { minx: Infinity, miny: Infinity, maxx: -Infinity, maxy: -Infinity } }
      list.push(cur)
    }
    cur.rings.push(points.subarray(at, at + n * 2))
    at += n * 2
    const b = cur.box
    if (boxes[i * 4]! < b.minx) b.minx = boxes[i * 4]!
    if (boxes[i * 4 + 1]! < b.miny) b.miny = boxes[i * 4 + 1]!
    if (boxes[i * 4 + 2]! > b.maxx) b.maxx = boxes[i * 4 + 2]!
    if (boxes[i * 4 + 3]! > b.maxy) b.maxy = boxes[i * 4 + 3]!
  }
  return { truncated, list }
}

/** Whether (x, y) is inside a footprint: inside its outer ring and not in a hole. */
export function inside(f: Footprint, x: number, y: number): boolean {
  if (x < f.box.minx || x > f.box.maxx || y < f.box.miny || y > f.box.maxy) return false
  let hit = false
  for (const ring of f.rings) {
    const n = ring.length / 2
    for (let i = 0, j = n - 1; i < n; j = i++) {
      const xi = ring[i * 2]!, yi = ring[i * 2 + 1]!, xj = ring[j * 2]!, yj = ring[j * 2 + 1]!
      if ((yi > y) !== (yj > y) && x < (xj - xi) * (y - yi) / (yj - yi) + xi) hit = !hit
    }
  }
  return hit
}

export interface Place {
  kind: string
  name: string
  ctx: string
  x: number
  y: number
  lat: number
  lon: number
}

export async function search(base: string, text: string, signal?: AbortSignal): Promise<Place[]> {
  const r = await fetch(`${base}/search?q=${encodeURIComponent(text)}`, { signal })
  if (!r.ok) return []
  return ((await r.json()) as { hits: Place[] }).hits ?? []
}

/** One point of link.json's profile: distance in km, and heights in metres
 *  above sea level of the terrain (`h`), the surface with clutter and
 *  buildings (`g`), the ray, and the first Fresnel radius there. */
export interface ProfilePoint { d: number; h: number; g: number; e: number; ray: number; f1: number; clr: number }

export interface LinkReply {
  lb_db: number
  distance_km: number
  /** The antennas' heights above the ground, where they are. */
  tx_h: number
  rx_h: number
  /** An end inside a building: what its walls cost, dB; null in the open. */
  tx_indoor_entry_db?: number | null
  rx_indoor_entry_db?: number | null
  fresnel: { verdict: 'clear' | 'grazing' | 'obstructed'; worst_clearance_m: number; worst_d_km: number }
  profile_evidence?: { model?: string; [k: string]: unknown }
  terminal_clutter?: unknown
  profile: ProfilePoint[]
  [key: string]: unknown
}

/** link.json between two points, or the sidecar's refusal as an Error.
 *  `locPct` is the geodata's percentage of locations, when it states one:
 *  the one its loss tables were asked at. */
export async function link(base: string, a: [number, number], b: [number, number],
                           txH: number, rxH: number, txGain?: number, rxGain?: number,
                           signal?: AbortSignal, locPct?: number): Promise<LinkReply> {
  const q = new URLSearchParams({
    ax: String(a[0]), ay: String(a[1]), bx: String(b[0]), by: String(b[1]),
    tx_h: String(txH), rx_h: String(rxH),
  })
  if (txGain !== undefined) q.set('tx_gain_dbi', String(txGain))
  if (rxGain !== undefined) q.set('rx_gain_dbi', String(rxGain))
  if (locPct !== undefined) q.set('loc_pct', String(locPct))
  const r = await getWithBackoff(`${base}/link.json?${q}`, signal)
  if (!r.ok) throw new Error((await r.text()).trim() || `link.json ${r.status}`)
  return await r.json() as LinkReply
}
