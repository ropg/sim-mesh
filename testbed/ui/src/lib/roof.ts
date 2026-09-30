/* "On this roof": an antenna height from a clicked point.
 *
 * With footprints (a pack built with --lod2-geometry), the footprint under
 * the click gives its roof in metres above sea level, and the antenna stands
 * on it: the height above the ground there is the roof less the terrain,
 * which is the footprint's height_m where its ground_z is the terrain's.
 * Without a footprint under the click the pack's raster stands in, its
 * clutter height at the point, and the height is marked `raster` rather than
 * `roof` so nobody mistakes it for a building's. */
import { buildings, getWithBackoff, inside, sample } from './planner'
import type { GroundPoint } from './marks'
import type { HeightFrom } from '../stores/nodes'

/** What stands at a point, in metres above sea level: the terrain, and the
 *  roof of the building whose footprint holds the point, when one does. */
export interface Place {
  ground: number
  roof: number | null
  /** The sidecar has no footprints to say yet (it answers so while it is
   *  still indexing them, as when the pack has none): ask again later. */
  roofUnknown?: boolean
}

const places = new Map<string, Promise<Place | null>>()

/** The terrain and roof at (x, y) on a pack, asked once per point; an
 *  answer that does not know the roof yet is not kept. */
export function placeAt(sidecar: string, x: number, y: number): Promise<Place | null> {
  const key = `${sidecar}|${x.toFixed(1)},${y.toFixed(1)}`
  let got = places.get(key)
  if (!got) {
    got = (async () => {
      const s = await sample(sidecar, x, y)
      if (!Number.isFinite(s.ground)) return null
      const near = await buildings(sidecar, { minx: x - 1, miny: y - 1, maxx: x + 1, maxy: y + 1 })
      if (near === null) {
        places.delete(key)
        return { ground: s.ground, roof: null, roofUnknown: true }
      }
      const under = near.list.find(f => inside(f, x, y))
      return { ground: s.ground, roof: under ? under.top : null }
    })().catch(() => { places.delete(key); return null })
    places.set(key, got)
  }
  return got
}

export async function roofAt(sidecar: string, at: GroundPoint): Promise<{ height_m: number; height_from: HeightFrom } | null> {
  if (at.footprint) {
    const ground = at.ground ?? (await sample(sidecar, at.x, at.y)).ground
    if (!Number.isFinite(ground)) return null
    return { height_m: Math.max(0.5, Math.round((at.footprint.top - ground) * 10) / 10), height_from: 'roof' }
  }
  const s = await sample(sidecar, at.x, at.y)
  if (s.clutter === null || !Number.isFinite(s.clutter) || s.clutter <= 0) return null
  return { height_m: Math.round(s.clutter * 10) / 10, height_from: 'raster' }
}

/** planner's estimate of an antenna's height at (x, y) on a pack, for a node
 *  whose height nobody measured (the sidecar's /height.json): a roof within
 *  reach of an imprecise position with a mast on it, `roof`, else the
 *  clutter or the land class around it, `raster`, each with the band it
 *  believes. null for an estimate on no evidence, no better than a height
 *  assumed, and from a sidecar without the estimator. */
export interface Estimate {
  height_m: number
  height_from: HeightFrom
  low_m: number
  high_m: number
}

const FROM_BASIS: Record<string, HeightFrom> = {
  'lod2-building': 'roof', 'clutter-neighbourhood': 'raster', 'class-typical': 'raster',
}

export async function estimateAt(sidecar: string, x: number, y: number): Promise<Estimate | null> {
  const res = await getWithBackoff(`${sidecar}/height.json?x=${x.toFixed(3)}&y=${y.toFixed(3)}`)
  if (res.status === 404) return null
  if (!res.ok) throw new Error(`height ${res.status}: ${(await res.text()).slice(0, 200)}`)
  const e = await res.json() as { h_agl_m: number; low_m: number; high_m: number; basis: string; buildings_index: string }
  /* Until the index is in, no roof is asked: an answer then is the rasters'
   * alone, which is not what the pack would say. */
  if (e.buildings_index === 'loading') throw new Error('the pack\'s buildings are still being indexed: try again in a moment')
  const from = FROM_BASIS[e.basis]
  if (!from) return null
  return { height_m: Math.round(e.h_agl_m * 10) / 10, height_from: from, low_m: e.low_m, high_m: e.high_m }
}
