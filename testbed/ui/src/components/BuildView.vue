<template>
  <div class="bv">
    <q-toolbar class="bv-bar">
      <q-btn flat dense no-caps label="‹ Back" @click="emit('back')" />
      <div class="bv-title">Build from sources</div>
      <q-space />
      <PlaceSearch nominatim @found="goToPlace" />
    </q-toolbar>
    <div class="bv-main">
      <SlippyMap ref="map" class="bv-map" :shapes="shapes" view-key="sim-mesh.buildview" bandable
                 @band="chose">
        <div class="bv-hint">drag to pan, wheel to zoom, Ctrl/Cmd-drag draws the rectangle</div>
      </SlippyMap>

      <div class="bv-side">
        <div v-if="!rect" class="bv-text">
          Draw the rectangle to build with Ctrl or Cmd and a drag on the map. Outlined
          rectangles are the packs there are; the tinted outlines are where the sources
          that do not cover the world apply, as the source files give them. The build
          takes, in each layer, the source of the highest priority for each part of the
          rectangle.
        </div>
        <template v-else>
          <div class="bv-mono">{{ rect[1].toFixed(4) }}…{{ rect[3].toFixed(4) }} N,
            {{ rect[0].toFixed(4) }}…{{ rect[2].toFixed(4) }} E</div>
          <q-input v-model="name" dense outlined label="name"
                   hint="lower-case letters, digits and hyphens" />
          <div class="bv-row">
            <span class="bv-label">resolution</span>
            <q-btn-toggle v-model="res" dense no-caps unelevated toggle-color="primary"
                          :options="[{ label: '30 m', value: 30 }, { label: '10 m', value: 10 }]" />
          </div>
          <div v-if="plan" class="bv-text">
            {{ plan.grid.cells[0] }} × {{ plan.grid.cells[1] }} cells
            ({{ plan.grid.size_km[0] }} × {{ plan.grid.size_km[1] }} km),
            UTM zone {{ plan.grid.zone }} (EPSG:{{ plan.grid.epsg }});
            OpenStreetMap from Geofabrik's {{ plan.extract.name ?? plan.extract.id }} extract.
          </div>
          <div v-if="refused" class="bv-bad">{{ refused }}</div>

          <table v-if="plan" class="bv-table">
            <thead><tr><th>source</th><th>to fetch</th><th>licence</th></tr></thead>
            <tbody>
              <tr v-for="s in plan.sources" :key="s.source">
                <td>{{ s.title }}<div class="bv-dim">{{ s.used_for }}</div>
                  <div class="bv-dim">{{ s.files }} file{{ s.files === 1 ? '' : 's' }}</div></td>
                <td class="bv-mono">{{ fetchText(s) }}</td>
                <td class="bv-dim">{{ s.licence }}</td>
              </tr>
            </tbody>
          </table>
          <div v-else-if="asking" class="bv-dim">Finding which sources cover this rectangle and which of their files it needs…</div>
          <div v-if="sizing" class="bv-dim">
            You can build now. The download sizes take a while: each file's host is asked how big it is.
          </div>

          <q-btn unelevated no-caps color="primary" label="Build" :loading="starting"
                 :disable="!plan || !!refused || !name.trim() || !!catalog.build && catalog.build.state !== 'failed'"
                 @click="build" />
          <div v-if="catalog.build && catalog.build.state !== 'failed'" class="bv-dim">
            {{ catalog.build.name }} is being built: one build at a time.
          </div>
        </template>
      </div>
    </div>
  </div>
</template>

<script setup lang="ts">
/* Building a pack from its sources: a map to draw the rectangle on, and the
 * side panel saying what it takes.
 *
 * The map is SlippyMap, OpenStreetMap's tiles: a drag pans, the wheel zooms
 * about the cursor, Ctrl/Cmd and a drag draws the rectangle. Over the tiles:
 * the areas of the sources that do not cover the world, tinted, and the
 * packs there are, outlined with their names.
 *
 * The side panel asks the front's /api/geodata/sources what the rectangle
 * takes at its resolution, whenever either changes: the grid, the sources
 * the front chose for it and what each is used for, and each source's
 * download still to fetch (what is in the cache costs nothing). It asks
 * first without the sizes, which takes a moment and is all Build needs, then
 * with them, which asks every file's host and for a city of tiles takes
 * seconds to minutes; Build does not wait for that. Build starts
 * it on the front, a dialog says which sources go into the pack and for
 * what, and the view goes back to the list, where the build's row shows how
 * it goes. */
import { computed, onMounted, onUnmounted, ref, watch } from 'vue'
import { useQuasar } from 'quasar'
import PlaceSearch, { type FoundPlace } from './PlaceSearch.vue'
import SlippyMap from './SlippyMap.vue'
import { useCatalog } from '../stores/catalog'
import { slug } from '../lib/zip'
import { boxGeometry, type Geometry, type MapShape } from '../lib/mapshape'

interface SourceRow {
  source: string; title: string; licence: string; used_for: string
  files: number; cached: number; to_fetch: number | null
}
interface Plan {
  grid: { zone: number; epsg: number; cells: [number, number]; size_km: [number, number] }
  extract: { id: string; name?: string }
  sources: SourceRow[]
}
const emit = defineEmits<{ back: []; started: [name: string] }>()
const catalog = useCatalog()
const quasar = useQuasar()
const map = ref<InstanceType<typeof SlippyMap>>()

const ASK_AFTER_MS = 400

/* ── the side panel ── */
const rect = ref<[number, number, number, number] | null>(null)
const name = ref('')
const res = ref(30)
const plan = ref<Plan | null>(null)
const refused = ref<string | null>(null)
const asking = ref(false)
const sizing = ref(false)
const sizesFailed = ref(false)
const starting = ref(false)
let askTimer: ReturnType<typeof setTimeout> | null = null
let askCtrl: AbortController | null = null

function mb(bytes: number) {
  return bytes >= 1e9 ? `${(bytes / 1e9).toFixed(1)} GB` : `${Math.max(0.1, bytes / 1e6).toFixed(1)} MB`
}

function fetchText(s: SourceRow) {
  if (s.cached === s.files) return 'in the cache'
  if (s.to_fetch === null && sizing.value) return 'asking the host for the size…'
  if (s.to_fetch === null) return sizesFailed.value ? 'size unknown: the host did not answer' : 'size unknown'
  return `${mb(s.to_fetch)}${s.cached ? ` (${s.cached} cached)` : ''}`
}

function query(): URLSearchParams {
  return new URLSearchParams({
    bbox: rect.value!.map(v => v.toFixed(6)).join(','), res_m: String(res.value),
  })
}

function askSoon() {
  if (askTimer) clearTimeout(askTimer)
  if (!rect.value) return
  askTimer = setTimeout(() => { void ask() }, ASK_AFTER_MS)
}

/* Twice: first without the download sizes, which is quick and all Build
 * needs, then with them, which means asking every file's host. */
async function ask() {
  askCtrl?.abort()
  const ctrl = new AbortController()
  askCtrl = ctrl
  asking.value = true
  sizesFailed.value = false
  try {
    for (const sizes of [false, true]) {
      const q = query()
      if (!sizes) q.set('sizes', '0')
      const r = await fetch(`/api/geodata/sources?${q.toString()}`, { signal: ctrl.signal })
      const body = await r.json() as Plan & { ok: boolean; error?: string }
      if (ctrl.signal.aborted) return
      if (!body.ok && sizes) { sizesFailed.value = true; return }
      if (!body.ok) { plan.value = null; refused.value = body.error ?? 'refused'; return }
      refused.value = null
      plan.value = body
      if (!sizes) { asking.value = false; sizing.value = true }
    }
  } catch (e) {
    if (!ctrl.signal.aborted) {
      if (sizing.value) sizesFailed.value = true       // the plan stands; only the sizes are missing
      else { plan.value = null; refused.value = (e as Error).message }
    }
  } finally {
    if (askCtrl === ctrl) { asking.value = false; sizing.value = false }
  }
}

watch(res, askSoon)

function escapeHtml(s: string) {
  return s.replace(/[&<>"']/g, c => `&#${c.charCodeAt(0)};`)
}

/* What the build takes, and for which part of the rectangle. */
function tellChosen(pack: string, rows: SourceRow[]) {
  const items = rows.map(s => `<li><b>${escapeHtml(s.title)}</b>: ${escapeHtml(s.used_for)}</li>`).join('')
  quasar.dialog({
    title: `Building ${escapeHtml(pack)}`,
    message: `The best source is taken for each part of the rectangle. The pack is built from:<ul>${items}</ul>`,
    html: true,
  })
}

async function build() {
  if (!rect.value) return
  starting.value = true
  try {
    const pack = name.value.trim()
    const rows = plan.value?.sources ?? []
    const r = await fetch('/api/geodata/build', {
      method: 'POST', headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify({ name: pack, bbox: rect.value, res_m: res.value }),
    })
    const body = await r.json() as { ok: boolean; error?: string; build?: unknown }
    if (!body.ok) { quasar.notify({ type: 'negative', message: body.error ?? 'refused', timeout: 8000 }); return }
    tellChosen(pack, rows)
    emit('started', pack)
  } finally {
    starting.value = false
  }
}

/* ── the map ── */
interface Area { source: string; title: string; outline: Geometry }
const areas = ref<Area[]>([])
const AREA_COLOURS = ['59, 130, 246', '217, 70, 239', '16, 185, 129', '245, 158, 11', '239, 68, 68']

/* Every regional source's outline tinted, each outline once (sources that
 * share one, Berlin's three, are one area), the packs outlined with their
 * names, and the rectangle. */
const areaShapes = computed<MapShape[]>(() => {
  const seen = new Set<string>()
  const out: MapShape[] = []
  for (const a of areas.value) {
    const key = JSON.stringify(a.outline)
    if (seen.has(key)) continue
    seen.add(key)
    const c = AREA_COLOURS[out.length % AREA_COLOURS.length]!
    out.push({ geometry: a.outline, fill: `rgba(${c}, 0.08)`, stroke: `rgba(${c}, 0.8)` })
  }
  return out
})
const shapes = computed<MapShape[]>(() => [
  ...areaShapes.value,
  ...catalog.geodata.filter(g => g.kind === 'pack' && g.bbox).map(g => ({
    geometry: boxGeometry(g.bbox), stroke: '#1f2937', dash: [6, 4], label: g.name,
  })),
  ...(rect.value ? [{ geometry: boxGeometry(rect.value), fill: 'rgba(234, 88, 12, 0.15)',
                      stroke: '#ea580c', width: 2 }] : []),
])

function chose(r: [number, number, number, number]) {
  rect.value = r
  plan.value = null
  askSoon()
}

function goToPlace(p: FoundPlace) {
  map.value?.fitBox(p.bbox)
  if (!name.value) name.value = slug(p.name.split(',')[0] ?? '')
}

onMounted(async () => {
  try {
    const r = await fetch('/api/geodata/areas')
    const body = await r.json() as { ok: boolean; areas?: Area[] }
    if (body.ok) areas.value = body.areas ?? []
  } catch { /* the outlines are a help, not a need */ }
})
onUnmounted(() => {
  if (askTimer) clearTimeout(askTimer)
  askCtrl?.abort()
})
</script>

<style scoped>
.bv { display: flex; flex-direction: column; height: 100%; }
.bv-bar { min-height: 38px; gap: 6px; padding-left: 4px; background: #171b21; border-bottom: 1px solid #262c35; flex: none; }
.bv-title { font-size: 14px; font-weight: 500; padding: 0 10px; }
.bv-main { flex: 1 1 auto; min-height: 0; display: flex; }
.bv-map { flex: 1 1 auto; min-width: 0; }
.bv-hint {
  position: absolute; left: 10px; bottom: 8px; font-size: 11px; color: #e5e7eb;
  background: rgba(18, 20, 23, 0.75); padding: 2px 6px; border-radius: 3px; pointer-events: none;
}
.bv-side {
  width: 340px; flex: none; overflow-y: auto; padding: 12px 14px 24px; display: flex;
  flex-direction: column; gap: 8px; border-left: 1px solid #262c35; background: #15181d;
}
.bv-text { font-size: 12px; color: #9ca3af; line-height: 1.5; }
.bv-label { font-size: 11px; color: #6b7280; margin-top: 4px; }
.bv-row { display: flex; align-items: center; gap: 10px; }
.bv-bad { font-size: 12px; color: #fca5a5; }
.bv-dim { font-size: 11px; color: #6b7280; }
.bv-mono { font-family: ui-monospace, monospace; font-size: 12px; }
.bv-table { width: 100%; border-collapse: collapse; font-size: 12px; }
.bv-table th { text-align: left; font-weight: 500; font-size: 11px; color: #6b7280; padding: 3px 4px; border-bottom: 1px solid #262c35; }
.bv-table td { padding: 4px; border-bottom: 1px solid #1f242c; vertical-align: top; }
</style>
