import { defineStore } from 'pinia'
import { samples } from '../lib/planner'
import { useGeodata } from './geodata'

/* The ground under each node, in metres above sea level, for the antennas'
 * directions: 0 on synthetic ground; on a pack the terrain there, asked of
 * the sidecar once per point and kept for the page's life. The points
 * wanted together (a nodeset's, as it is drawn) are asked in one request,
 * AT_ONCE at most. `version` moves as answers land, so whatever reads them
 * redraws. */

const AT_ONCE = 1024

export const useGrounds = defineStore('grounds', {
  state: () => ({
    values: {} as Record<string, number>,
    version: 0,
  }),

  actions: {
    /** The ground at a node's position, or null while it is being asked. */
    at(lat: number, lon: number): number | null {
      const g = useGeodata()
      if (!g.isPack || !g.sidecar) return 0
      const key = `${g.current!.name}|${lat},${lon}`
      if (key in this.values) return this.values[key]!
      queue(key, g.sidecar, ...g.frame.toXY(lat, lon))
      return null
    },
  },
})

const wanted: { key: string; base: string; x: number; y: number }[] = []
const asked = new Set<string>()
let flushing = false

function queue(key: string, base: string, x: number, y: number) {
  if (asked.has(key)) return
  asked.add(key)
  wanted.push({ key, base, x, y })
  if (flushing) return
  // Every point asked for in this turn of the page goes in the one request.
  flushing = true
  setTimeout(() => { flushing = false; void ask() }, 0)
}

async function ask() {
  while (wanted.length) {
    const base = wanted[0]!.base
    const jobs = wanted.filter(j => j.base === base).slice(0, AT_ONCE)
    for (const j of jobs) wanted.splice(wanted.indexOf(j), 1)
    try {
      const got = await samples(base, jobs.map(j => [j.x, j.y] as [number, number]))
      const store = useGrounds()
      jobs.forEach((j, i) => { store.values[j.key] = Number.isFinite(got[i]!.ground) ? got[i]!.ground : 0 })
      store.version++
    } catch {
      for (const j of jobs) asked.delete(j.key)
    }
  }
}
