import { defineStore } from 'pinia'
import { request } from '../lib/front'
import type { AntennaSpec } from '../lib/antennas'
import { useSocket } from './socket'

/* What the store holds, for every picker and list on the page: firmware,
 * antennas, geodata, nodesets, scripts, snapshots, and the script runs the front keeps
 * with their output. The registry the front sends every second names what
 * there is; the *_list verbs describe it, and a name alone keeps what a
 * description already said. */

/** An installed firmware, as the Firmware tab lists it, named
 *  `<base>_<arch>_<version>`. `users` are the paused runs and snapshots that
 *  hold it; `paused` the paused simulations among them, by name, which a
 *  delete stops. */
export interface FirmwareRow {
  name: string
  base: string
  arch: string
  version: string
  category?: string
  radio?: string | null
  title?: string
  hardware?: string | null
  users?: string[]
  paused?: string[]
  /** Its directory's size on disk. */
  bytes?: number | null
  error?: string
}

export interface GeodataInfo {
  name: string
  kind: 'pack' | 'synthetic'
  /** Latitude and longitude: a pack's centre, synthetic ground's 0°, 0°. */
  origin: [number, number]
  /** [lon0, lat0, lon1, lat1]. */
  bbox: [number, number, number, number]
  pack?: string
  crs_epsg?: number
  layers?: string[]
  /** Each source's notice, which the map shows when it draws this ground. */
  licences?: { source: string; notice: string }[]
  pack_manifest_hash?: string
  exponent?: number
  terrain?: string
  extent_m?: number
  /** A pack's percentage of locations, when the geodata states one. */
  loc_pct?: number
  /** How many nodesets have a node inside its bbox. */
  nodesets?: number
  /** Its directory's size on disk. */
  bytes?: number
  /** The index it came from, when it did. */
  from_index?: Origin | null
  error?: string
}

/** Where something installed from an index came from. */
export interface Origin {
  index: string; address: string; url: string; sha256: string; added: string
  /** A nodeset edited since it came. */
  changed?: boolean
}

export interface NodesetRow {
  name: string
  nodes?: number
  bbox?: [number, number, number, number] | null
  tags?: Record<string, number>
  /** Its file's size on disk, with its own setup script. */
  bytes?: number
  from_index?: Origin | null
  error?: string
}

export type IndexKind = 'geodata' | 'nodesets'

/** One thing an index offers, and whether it is here. */
export interface IndexEntry {
  name: string; url: string; sha256: string; bytes?: number
  title?: string; description?: string; licences?: string; tags?: string[]
  bbox?: [number, number, number, number]
  /** A nodeset's: the geodata it is made for, and how many nodes it has. */
  geodata?: string; nodes?: number
  installed: boolean
  /** The name is something else's here. */
  taken: boolean
  /** A nodeset installed from it and edited since. */
  changed?: boolean
}

export interface IndexRow {
  name: string; address: string
  /** sim-mesh's own, always listed. */
  own: boolean
  title: string
  /** The index's text about its collection, line breaks its own. */
  description: string
  error?: string
  geodata: IndexEntry[]
  nodesets: IndexEntry[]
}

/** An entry being installed, as the front tells it. */
export interface Fetching {
  index: string; kind: IndexKind; name: string
  state: 'fetching' | 'done' | 'failed' | 'cancelled'
  /** What is being fetched now: a nodeset's geodata comes first. */
  now: { kind: IndexKind; name: string } | null
  fetched: number; of: number | null; error: string | null
}

/** One source of the build's cache. */
export interface SourceRow {
  source: string; what: string; licence: string; bytes: number
  /** What its files hold, for the row's tooltip. */
  holds: string
  /** Whether it has an area to draw on the map. */
  map: boolean
  /** A source's layers and its priority in each, where it stands in the
   *  source files (`global`, or continent › country), and whether it is
   *  this machine's own (testbed/sources.yaml). */
  layers?: Record<string, number>
  where?: string
  own?: boolean
}

/** A file a script imports that is sim-mesh's own: the library's (read-only
 *  here) or another script's. */
export interface ScriptReference { name: string; path: string; library: boolean }

/** Something a script asks for before it runs (`script_input(…)`): a number,
 *  text, yes or no, a run's name, or for type `firmware` an installed
 *  firmware, of `category` when it names one. */
export interface ScriptInput {
  name: string; type: 'int' | 'float' | 'str' | 'bool' | 'firmware' | 'run'; label: string
  category?: string; default?: string | number | boolean
}

export interface ScriptRow {
  name: string; doc?: string; report?: boolean; references?: ScriptReference[]; error?: string
  inputs?: ScriptInput[]
  /** The scripts that include or import this one: it is part of them, not run on its own. */
  included_by?: string[]
  /** One sim-mesh ships: never saved over, kept changed with Save as. */
  example?: boolean
}

export interface ScriptRun {
  run: string
  name: string
  sim: string
  state: 'running' | 'stopping' | 'exited'
  code: number | null
  started: number
  /** The simulation's run directory, relative to testbed/. */
  run_dir?: string
  /** Whether that run has a report.md. */
  report?: boolean
  lines: string[]
}

/** A pack being built from its sources, as the front tells it. */
export interface BuildRow {
  name: string
  state: 'fetching' | 'compiling' | 'done' | 'failed' | 'cancelled'
  step: string | null
  done: number
  total: number
  /** Bytes fetched so far, and of how many (null: a host did not say). */
  fetched: number
  of: number | null
  error: string | null
  spec: { bbox: number[]; res_m: number; terrain: string; buildings: string; population: string }
}

export interface Listing { name: string; [key: string]: unknown }

/** The radio scripts/globals.py gives every node not tagged no-radio. */
export interface Globals { FREQ_MHZ: number; SF: number; BW_KHZ: number; CR: number }

const LINES_KEPT = 2000

export const useCatalog = defineStore('catalog', {
  state: () => ({
    firmware: [] as FirmwareRow[],
    arch: '' as string,
    antennas: [] as AntennaSpec[],
    geodata: [] as GeodataInfo[],
    /** The pack being built, or the last one that failed. */
    build: null as BuildRow | null,
    nodesets: [] as NodesetRow[],
    scripts: [] as ScriptRow[],
    snapshots: [] as Listing[],
    runs: {} as Record<string, ScriptRun>,
    /** scripts/globals.py's radio, as the registry and `store` say it; null
     *  with `globalsError` when the file does not give it. */
    globals: null as Globals | null,
    globalsError: null as string | null,
    /** The listed indexes, as last fetched; null before they are asked for. */
    indexes: null as IndexRow[] | null,
    indexesLoading: false,
    /** Entries being installed, by `index/kind/name`. */
    fetching: {} as Record<string, Fetching>,
  }),

  getters: {
    runList: (s): ScriptRun[] => Object.values(s.runs).sort((a, b) => b.started - a.started),
    /** The antennas by type, for the patterns. */
    antennaByType: (s): Record<string, AntennaSpec> =>
      Object.fromEntries(s.antennas.map(a => [a.type, a])),
  },

  actions: {
    receive(msg: Record<string, unknown>) {
      if (msg.sim !== undefined) {
        if (msg.type === 'store') this.names(msg)
        return
      }
      if (msg.type === 'firmware_changed') {
        void this.refreshFirmware()
      } else if (msg.type === 'index_progress') {
        const row = msg as unknown as Fetching
        const key = fetchKey(row.index, row.kind, row.name)
        if (row.state === 'fetching') {
          this.fetching[key] = row
        } else {
          delete this.fetching[key]
          if (row.state === 'done') {
            void this.refreshIndexes()
            void this.refreshGeodata()
            void this.refreshNodesets()
          }
        }
      } else if (msg.type === 'geodata_progress') {
        const row = msg.build as BuildRow | null
        this.build = row && row.state !== 'done' && row.state !== 'cancelled' ? row : null
        if (row?.state === 'done') void this.refreshGeodata()
      } else if (msg.type === 'sims') {
        this.names(msg)
        for (const r of (msg.script_runs as Omit<ScriptRun, 'lines'>[]) ?? []) {
          const had = this.runs[r.run]
          this.runs[r.run] = { ...r, lines: had?.lines ?? [] }
        }
      } else if (msg.type === 'store') {
        this.names(msg)
      } else if (msg.type === 'script_output') {
        const run = this.runs[msg.run as string]
        if (run) {
          run.lines.push(msg.line as string)
          if (run.lines.length > LINES_KEPT) run.lines.splice(0, run.lines.length - LINES_KEPT)
        }
      } else if (msg.type === 'script_exit') {
        const run = this.runs[msg.run as string]
        if (run) { run.state = 'exited'; run.code = msg.code as number }
      }
    },

    /** Names from the registry or a simulation's `store` message. */
    names(msg: Record<string, unknown>) {
      const keep = <T extends { name: string }>(old: T[], v: unknown): T[] => {
        if (!Array.isArray(v)) return old
        const byName = new Map(old.map(o => [o.name, o]))
        return (v as string[]).map(n => byName.get(n) ?? ({ name: n } as T))
      }
      this.geodata = keep(this.geodata, msg.geodata_names)
      this.nodesets = keep(this.nodesets, msg.nodesets)
      this.scripts = keep(this.scripts, msg.scripts)
      this.snapshots = keep(this.snapshots, msg.snapshots)
      if ('globals' in msg) {
        const g = (msg.globals as Globals | null) ?? null
        // Replaced only when it changes, so what is drawn from it is not redone every second.
        if (JSON.stringify(g) !== JSON.stringify(this.globals)) this.globals = g
        this.globalsError = (msg.globals_error as string | null) ?? null
      }
    },

    async refresh() {
      const [g, n, s, p, f, a] = await Promise.all([
        request('geodata_list'), request('nodeset_list'), request('script_list'),
        request('snapshot_list'), request('firmware_list'), request('antenna_list'),
      ])
      if (g.ok) { this.geodata = g.geodata as GeodataInfo[]; this.build = g.build as BuildRow | null }
      if (n.ok) this.nodesets = n.nodesets as NodesetRow[]
      if (s.ok) this.scripts = s.scripts as ScriptRow[]
      if (p.ok) this.snapshots = p.snapshots as Listing[]
      if (f.ok) this.takeFirmware(f)
      if (a.ok) this.antennas = a.antennas as AntennaSpec[]
    },

    takeFirmware(f: Record<string, unknown>) {
      this.firmware = f.firmware as FirmwareRow[]
      this.arch = f.arch as string
    },

    /** The antennas, once: the catalogue is a file and does not change. A
     *  simd on its own sends it in its snapshot instead. */
    async ensureAntennas() {
      if (this.antennas.length || !useSocket().front) return
      const a = await request('antenna_list')
      if (a.ok) this.antennas = a.antennas as AntennaSpec[]
    },

    async refreshGeodata() {
      const g = await request('geodata_list')
      if (g.ok) { this.geodata = g.geodata as GeodataInfo[]; this.build = g.build as BuildRow | null }
    },

    async refreshFirmware() {
      const f = await request('firmware_list')
      if (f.ok) this.takeFirmware(f)
    },

    async refreshNodesets() {
      const n = await request('nodeset_list')
      if (n.ok) this.nodesets = n.nodesets as NodesetRow[]
    },

    /** Every listed index, fetched again by the front, and what is being
     *  installed from them. */
    async refreshIndexes() {
      this.indexesLoading = true
      const r = await request('index_list')
      this.indexesLoading = false
      if (!r.ok) return
      this.indexes = r.indexes as IndexRow[]
      this.fetching = Object.fromEntries((r.fetching as Fetching[])
        .map(f => [fetchKey(f.index, f.kind, f.name), f]))
    },

    /** A script run's output so far, for a page opened after it began. */
    async loadRun(run: string) {
      const r = await request('script_log', { run })
      const held = this.runs[run]
      if (r.ok && held) held.lines = r.lines as string[]
    },
  },
})

export function fetchKey(index: string, kind: string, name: string) { return `${index}/${kind}/${name}` }

/** Whether [lon0, lat0, lon1, lat1] boxes overlap. */
export function overlaps(a: number[] | null | undefined, b: number[] | null | undefined): boolean {
  if (!a || !b) return false
  return a[0]! <= b[2]! && b[0]! <= a[2]! && a[1]! <= b[3]! && b[1]! <= a[3]!
}
