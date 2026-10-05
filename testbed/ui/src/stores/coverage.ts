import { defineStore } from 'pinia'
import { request } from '../lib/front'
import { useSocket } from './socket'
import { useCatalog, type GeodataInfo } from './catalog'
import { threshold } from '../lib/links'
import { direction, gain, specOf, type Antenna, type AntennaSpec } from '../lib/antennas'
import { coverageBands, NO_BAND, plannerBase, type BandsNode } from '../lib/planner'
import { txDbm } from '../lib/boards'
import type { Frame } from '../lib/proj'
import { NO_RADIO, type NodeView } from './nodes'

/* Coverage: where each node's frames can be decoded, and the network's as
 * the best margin at each point over the nodes on show.
 *
 * On a pack each node's path loss to the ground around it is a raster the
 * front has the planner sweep (coverage.py), cached by the node's position
 * and height. The page holds none of them: for the view on show the planner
 * combines the rasters of the nodes asked about into the band of each
 * square of it (/coverage/bands.bin), with the arithmetic below. On
 * synthetic ground the loss is the log-distance formula, worked out here,
 * on flat ground at 0 m. Either way the level at a point is the node's
 * maximum power at its connector (lib/boards.ts) plus its antenna's gain
 * toward the point less that loss, and the margin is that less the
 * decoding threshold (the SF and bandwidth scripts/globals.py gives every
 * radio, over the noise floor), as the ether rules. A node tagged no-radio
 * covers nothing, and without the globals nothing is covered. The gain is taken in three dimensions: the azimuth to the
 * point and the elevation from the antenna's tip (the ground under the node
 * plus its height) to a receiver RX_HEIGHT_M over the ground there, the
 * earth's curvature taken off. So a high collinear's narrow beam passes over
 * the ground close under it, and an aimed yagi covers what it faces. */

/** A view a coverage image is of: its centre in the geodata's metres, and metres per CSS pixel. */
export interface CoverageView { cx: number; cy: number; mpp: number }

/** What the map asks of a view `w`×`h` CSS pixels: the band of each
 *  `px`-pixel square of it, rows from the top, as an index into
 *  COVERAGE_BANDS or NO_BAND where nothing decodes. It throws an AbortError
 *  once `signal` says the view is no longer wanted (a newer aim, a moved
 *  view). */
export interface CoverageSource {
  version: string
  bands(at: CoverageView, w: number, h: number, px: number, signal: AbortSignal): Promise<Uint8Array>
}

/** How long one slice of coverage work may hold the page before it yields,
 *  so a dial being dragged, a map being panned, stay live. */
export const SLICE_MS = 12

/** The page's turn: a frame drawn and input handled before the next slice. */
export function yieldToPage(): Promise<void> {
  return new Promise(resolve => setTimeout(resolve, 0))
}

const RX_GAIN_DBI = 0
/** The receiver the rasters are swept to (coverage.py RX_HEIGHT_M). */
const RX_HEIGHT_M = 2

/** What a margin over the decoding threshold is good for, from the most:
 *  indoors too (enough over the edge to lose a building's walls), outdoors
 *  only, and the edge, where fading decides; below 0 there is no chance and
 *  nothing is drawn. `from` is the least margin of each, in dB. */
export const EDGE_DB = 6
export const INDOOR_LOSS_DB = 15
export const COVERAGE_BANDS = [
  { name: 'indoors too', from: EDGE_DB + INDOOR_LOSS_DB, rgba: [34, 197, 94, 150] },
  { name: 'outdoors only', from: EDGE_DB, rgba: [250, 204, 21, 140] },
  { name: 'edge, maybe', from: 0, rgba: [239, 68, 68, 130] },
] as const

/** The band a margin falls in: the first it reaches, NO_BAND below 0 or with none. */
function bandOf(m: number | null): number {
  if (m === null || m < 0) return NO_BAND
  const i = COVERAGE_BANDS.findIndex(b => m >= b.from)
  return i < 0 ? COVERAGE_BANDS.length - 1 : i
}

/** One node's antenna: where it stands, the ground there and its tip, in
 *  metres above sea level, and its pattern. */
interface Beam { x: number; y: number; ground: number; top: number; spec: AntennaSpec | null; antenna: Antenna }

function beamGain(b: Beam, x: number, y: number, rxGround: number): number {
  if (!b.spec) return 0
  const [az, el] = direction(b.x, b.y, b.top, x, y, rxGround + RX_HEIGHT_M)
  return gain(b.spec, b.antenna, az, el)
}

/** Where a node stands, as far as its raster goes. */
function placeKey(n: NodeView) { return `${n.lat},${n.lon},${n.height_m}` }

/** The nodes with a radio: every one not tagged no-radio. */
function withRadio(nodes: NodeView[]) { return nodes.filter(n => !n.tags.includes(NO_RADIO)) }

function fspl1m(freqHz: number) {
  return 20 * Math.log10(4 * Math.PI * Math.max(freqHz, 1) / 299_792_458)
}

export const useCoverage = defineStore('coverage', {
  state: () => ({
    /** Each node's raster key on `geodata`, with the position it was asked
     *  for: a key counts only while the node still stands there, so an
     *  answer that lands late, or one from before a move, is never misread.
     *  Answers add to it, node by node, whatever else was asked since. */
    keys: {} as Record<string, { key: string; at: string }>,
    /** The keys whose raster is in the front's cache, for the planner to combine. */
    ready: {} as Record<string, true>,
    /** The last ask, whose answer alone says what is still being swept. */
    asked: 0,
    geodata: null as string | null,
    /** Nodes whose raster is being swept. */
    pending: [] as string[],
    problem: null as string | null,
    version: 0,
  }),

  actions: {
    /** Have the rasters for these nodes on this geodata: cached ones at once,
     *  the rest as the front's sweeps land. Nothing to ask on synthetic ground. */
    async ensure(gd: GeodataInfo | null, all: NodeView[]) {
      const ask = ++this.asked
      const nodes = withRadio(all)
      if (!gd || gd.kind !== 'pack' || !nodes.length || !useSocket().front) { this.pending = []; return }
      if (this.geodata !== gd.name) { this.keys = {}; this.ready = {}; this.geodata = gd.name }
      const at = Object.fromEntries(nodes.map(n => [n.name, placeKey(n)]))
      const r = await request('coverage', {
        geodata: gd.name,
        nodes: nodes.map(n => ({ name: n.name, lat: n.lat, lon: n.lon, height_m: n.height_m })),
      })
      if (this.geodata !== gd.name) return
      const latest = ask === this.asked
      if (!r.ok) { if (latest) this.problem = r.error ?? 'no coverage'; return }
      const tiles = r.tiles as { node: string; key: string; cached: boolean }[]
      for (const t of tiles) {
        this.keys[t.node] = { key: t.key, at: at[t.node] ?? '' }
        if (t.cached) this.ready[t.key] = true
      }
      if (latest) {
        this.problem = null
        this.pending = tiles.filter(t => !this.ready[t.key]).map(t => t.node)
      }
      this.version++
    },

    receive(msg: Record<string, unknown>) {
      if (msg.type === 'coverage_tile') {
        this.pending = this.pending.filter(n => n !== msg.node)
        if (msg.geodata === this.geodata) this.ready[msg.key as string] = true
        this.version++
      } else if (msg.type === 'coverage_error') {
        this.pending = this.pending.filter(n => n !== msg.node)
        this.problem = msg.error as string
      }
    },

    /** The bands source for these nodes on this geodata, in its frame. */
    source(gd: GeodataInfo, frame: Frame, all: NodeView[], noiseFigureDb = 6): CoverageSource {
      const exponent = gd.exponent ?? 2.7
      const catalog = useCatalog()
      const antennas = catalog.antennaByType
      const radio = catalog.globals
      const nodes = radio ? withRadio(all) : []
      const radioKey = radio ? `${radio.FREQ_MHZ},${radio.SF},${radio.BW_KHZ}` : 'none'
      const each = nodes.map((n) => {
        const freq = radio!.FREQ_MHZ * 1e6
        const budget = txDbm(n.max_dbm, undefined) + RX_GAIN_DBI
          - threshold(radio!.BW_KHZ * 1e3, radio!.SF, noiseFigureDb)
        const [x, y] = frame.toXY(n.lat, n.lon)
        const anchor = fspl1m(freq)
        const held = this.geodata === gd.name ? this.keys[n.name] : undefined
        const key = held && held.at === placeKey(n) ? held.key : ''
        const spec = specOf(antennas, n.antenna)
        // Synthetic ground is flat at 0 m.
        const beam: Beam = { x, y, ground: 0, top: n.height_m, spec, antenna: n.antenna }
        // Past this the formula's loss exceeds the budget at the antenna's peak: nothing to ask.
        const reach = Math.pow(10, (budget + (spec?.peak_dbi ?? 0) - anchor) / (10 * exponent))
        return { node: n, x, y, budget, anchor, reach, key, ready: !!key && !!this.ready[key], spec, beam }
      })
      const antennaKey = (n: NodeView) => `${JSON.stringify(n.antenna)}${n.max_dbm ?? ''}`
      const version = `${gd.name}|${radioKey}|${Object.keys(antennas).length}|${nodes.map((n, i) =>
        `${n.name}:${n.lat},${n.lon},${n.height_m},${antennaKey(n)}`
        + `:${each[i]!.key}:${each[i]!.ready ? 1 : 0}`).join(';')}`
      if (gd.kind === 'pack') {
        // The planner combines the rasters; a node whose raster has not come is left out.
        const asked: BandsNode[] = each.filter(e => e.ready).map(e => ({
          key: e.key, x: e.x, y: e.y, height_m: e.node.height_m, budget_db: e.budget,
          antenna: e.spec && {
            directional: e.spec.kind === 'directional', peak_dbi: e.spec.peak_dbi, vbw_deg: e.spec.vbw_deg,
            tilt_deg: e.spec.tilt_deg, hbw_deg: e.spec.hbw_deg, floor_db: e.spec.floor_db,
            azimuth_deg: e.node.antenna?.azimuth_deg ?? 0, elevation_deg: e.node.antenna?.elevation_deg ?? 0,
          },
        }))
        return {
          version,
          async bands(at, w, h, px, signal) {
            if (!asked.length) return new Uint8Array(Math.ceil(w / px) * Math.ceil(h / px)).fill(NO_BAND)
            return coverageBands(plannerBase(gd.name), {
              geodata: gd.name, at, w, h, px, bands: COVERAGE_BANDS.map(b => b.from), rxH: RX_HEIGHT_M, nodes: asked,
            }, signal)
          },
        }
      }
      const marginAt = (x: number, y: number): number | null => {
        let best: number | null = null
        for (const e of each) {
          const d = Math.hypot(x - e.x, y - e.y)
          if (d > e.reach) continue
          const loss = e.anchor + 10 * exponent * Math.log10(Math.max(d, 1))
          const margin = e.budget + beamGain(e.beam, x, y, 0) - loss
          if (best === null || margin > best) best = margin
        }
        return best
      }
      return {
        version,
        // A slice at a time, yielding to the page between slices.
        async bands(at, w, h, px, signal) {
          const cols = Math.ceil(w / px), rows = Math.ceil(h / px)
          const out = new Uint8Array(cols * rows)
          let since = performance.now()
          for (let row = 0; row < rows; row++) {
            if (performance.now() - since > SLICE_MS) {
              await yieldToPage()
              if (signal.aborted) throw new DOMException('aborted', 'AbortError')
              since = performance.now()
            }
            const y = at.cy - ((row + 0.5) * px - h / 2) * at.mpp
            for (let col = 0; col < cols; col++) {
              out[row * cols + col] = bandOf(marginAt(at.cx + ((col + 0.5) * px - w / 2) * at.mpp, y))
            }
          }
          return out
        },
      }
    },
  },
})
