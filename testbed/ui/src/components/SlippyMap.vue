<template>
  <div ref="wrap" class="slippy">
    <canvas ref="canvas" :class="{ 'slippy-banding': banding }"
            @pointerdown="onDown" @pointermove="onMove" @pointerup="onUp" @pointercancel="onUp"
            @wheel.prevent="onWheel" />
    <slot />
    <div class="slippy-credit">
      © <a href="https://www.openstreetmap.org/copyright" target="_blank" rel="noopener">OpenStreetMap</a> contributors
    </div>
  </div>
</template>

<script setup lang="ts">
/* A map of the world in degrees: OpenStreetMap's standard tiles in Web
 * Mercator, fetched through the front's /osm/, which keeps them (the tile
 * usage policy asks for that, and one origin keeps it one cache), drawn on a
 * canvas with `shapes` over them, each a GeoJSON polygon or multipolygon in
 * degrees with its fill, outline and label. A drag pans, the wheel zooms
 * about the cursor; with `bandable`, Ctrl or Cmd and a drag draws a
 * rectangle, said as `band` [west, south, east, north]; a click that is no
 * drag says the point under it as `pick` [lon, lat]. The view is kept per
 * browser under `viewKey`. */
import { onMounted, onUnmounted, ref, watch, nextTick } from 'vue'
import { boxGeometry, type Geometry, type MapShape } from '../lib/mapshape'

const props = withDefaults(defineProps<{
  shapes: MapShape[]
  viewKey: string
  bandable?: boolean
  /** A point marked on the map [lon, lat]. */
  pin?: [number, number] | null
}>(), { bandable: false, pin: null })
const emit = defineEmits<{
  band: [rect: [number, number, number, number]]
  /** A click, not a drag: the point under it [lon, lat]. */
  pick: [point: [number, number]]
}>()

const TILE = 256
const MIN_ZOOM = 1
const MAX_ZOOM = 19
const TILES_KEPT = 400

const wrap = ref<HTMLDivElement>()
const canvas = ref<HTMLCanvasElement>()
const view = ref({ lon: 10, lat: 51, zoom: 5 })
let size = { w: 1, h: 1, dpr: 1 }
let ctx: CanvasRenderingContext2D | null = null
const tiles = new Map<string, HTMLImageElement>()

function worldPx(lon: number, lat: number, zoom: number): [number, number] {
  const w = TILE * 2 ** zoom
  const s = Math.sin(Math.max(-85.05, Math.min(85.05, lat)) * Math.PI / 180)
  return [(lon + 180) / 360 * w, (0.5 - Math.log((1 + s) / (1 - s)) / (4 * Math.PI)) * w]
}

function lonLat(px: number, py: number, zoom: number): [number, number] {
  const w = TILE * 2 ** zoom
  const lon = px / w * 360 - 180
  const n = Math.PI - 2 * Math.PI * py / w
  return [lon, Math.atan(Math.sinh(n)) * 180 / Math.PI]
}

function toScreen(lon: number, lat: number): [number, number] {
  const [cx, cy] = worldPx(view.value.lon, view.value.lat, view.value.zoom)
  const [x, y] = worldPx(lon, lat, view.value.zoom)
  return [x - cx + size.w / 2, y - cy + size.h / 2]
}

function fromScreen(sx: number, sy: number): [number, number] {
  const [cx, cy] = worldPx(view.value.lon, view.value.lat, view.value.zoom)
  return lonLat(cx + sx - size.w / 2, cy + sy - size.h / 2, view.value.zoom)
}

function tile(z: number, x: number, y: number): HTMLImageElement {
  const key = `${z}/${x}/${y}`
  let img = tiles.get(key)
  if (img) { tiles.delete(key); tiles.set(key, img); return img }
  img = new Image()
  img.onload = () => schedule()
  img.src = `/osm/${key}.png`
  tiles.set(key, img)
  if (tiles.size > TILES_KEPT) tiles.delete(tiles.keys().next().value!)
  return img
}

let frame = 0
function schedule() {
  if (!frame) frame = requestAnimationFrame(() => { frame = 0; draw() })
}

function drawTiles(c: CanvasRenderingContext2D) {
  const z = Math.max(0, Math.min(MAX_ZOOM, Math.round(view.value.zoom)))
  const scale = 2 ** (view.value.zoom - z)
  const [cx, cy] = worldPx(view.value.lon, view.value.lat, z)
  const left = cx - size.w / 2 / scale, top = cy - size.h / 2 / scale
  const n = 2 ** z
  const x0 = Math.floor(left / TILE), y0 = Math.max(0, Math.floor(top / TILE))
  const x1 = Math.floor((left + size.w / scale) / TILE), y1 = Math.min(n - 1, Math.floor((top + size.h / scale) / TILE))
  for (let ty = y0; ty <= y1; ty++) {
    for (let tx = x0; tx <= x1; tx++) {
      const img = tile(z, ((tx % n) + n) % n, ty)
      const sx = (tx * TILE - left) * scale, sy = (ty * TILE - top) * scale
      if (img.complete && img.naturalWidth) c.drawImage(img, sx, sy, TILE * scale + 0.5, TILE * scale + 0.5)
    }
  }
}

function polygons(g: Geometry): number[][][][] {
  if (!g) return []
  return (g.type === 'Polygon' ? [g.coordinates] : g.type === 'MultiPolygon' ? g.coordinates : []) as number[][][][]
}

/** The first point of a shape's first ring's top-left, on screen, for its label. */
function labelAt(g: Geometry): [number, number] | null {
  const ring = polygons(g)[0]?.[0]
  if (!ring?.length) return null
  let west = Infinity, north = -Infinity
  for (const [lon, lat] of ring) { west = Math.min(west, lon!); north = Math.max(north, lat!) }
  return toScreen(west, north)
}

function drawShape(c: CanvasRenderingContext2D, s: MapShape) {
  const polys = polygons(s.geometry)
  if (!polys.length) return
  c.beginPath()
  for (const poly of polys) {
    for (const ring of poly) {
      ring.forEach(([lon, lat], i) => {
        const [x, y] = toScreen(lon!, lat!)
        if (i) c.lineTo(x, y); else c.moveTo(x, y)
      })
      c.closePath()
    }
  }
  if (s.fill) { c.fillStyle = s.fill; c.fill('evenodd') }
  if (s.stroke) {
    c.strokeStyle = s.stroke
    c.lineWidth = s.width ?? 1.5
    c.setLineDash(s.dash ?? [])
    c.stroke()
    c.setLineDash([])
  }
  if (s.label) {
    const at = labelAt(s.geometry)
    if (at) {
      c.font = '12px system-ui, sans-serif'
      c.fillStyle = s.stroke ?? '#1f2937'
      c.fillText(s.label, at[0] + 4, at[1] + 14)
    }
  }
}

function draw() {
  const c = ctx
  if (!c) return
  c.fillStyle = '#d4d0c8'
  c.fillRect(0, 0, size.w, size.h)
  drawTiles(c)
  for (const s of props.shapes) drawShape(c, s)
  if (props.pin) {
    const [x, y] = toScreen(props.pin[0], props.pin[1])
    c.beginPath()
    c.arc(x, y, 5, 0, 2 * Math.PI)
    c.fillStyle = '#dc2626'
    c.fill()
    c.strokeStyle = '#ffffff'
    c.lineWidth = 2
    c.stroke()
  }
  const r = band ? bandRect() : null
  if (r) drawShape(c, { geometry: boxGeometry(r), fill: 'rgba(234, 88, 12, 0.15)', stroke: '#ea580c', width: 2 })
}

/* ── the pointer ── */
const CLICK_PX = 4                      // a press that moves less than this is a click
let drag: { x: number; y: number; lon: number; lat: number; moved: boolean } | null = null
let band: { x0: number; y0: number; x1: number; y1: number } | null = null
const banding = ref(false)

function pointer(e: PointerEvent): [number, number] {
  const r = canvas.value!.getBoundingClientRect()
  return [e.clientX - r.left, e.clientY - r.top]
}

function bandRect(): [number, number, number, number] | null {
  if (!band) return null
  const [lonA, latA] = fromScreen(band.x0, band.y0)
  const [lonB, latB] = fromScreen(band.x1, band.y1)
  return [Math.min(lonA, lonB), Math.min(latA, latB), Math.max(lonA, lonB), Math.max(latA, latB)]
}

function onDown(e: PointerEvent) {
  const [x, y] = pointer(e)
  canvas.value!.setPointerCapture(e.pointerId)
  if (props.bandable && (e.ctrlKey || e.metaKey)) {
    band = { x0: x, y0: y, x1: x, y1: y }
    banding.value = true
  } else {
    drag = { x, y, lon: view.value.lon, lat: view.value.lat, moved: false }
  }
}

function onMove(e: PointerEvent) {
  const [x, y] = pointer(e)
  if (band) { band.x1 = x; band.y1 = y; schedule(); return }
  if (!drag) return
  if (!drag.moved && Math.hypot(x - drag.x, y - drag.y) < CLICK_PX) return
  drag.moved = true
  const [cx, cy] = worldPx(drag.lon, drag.lat, view.value.zoom)
  const [lon, lat] = lonLat(cx - (x - drag.x), cy - (y - drag.y), view.value.zoom)
  view.value = { ...view.value, lon, lat }
  schedule()
}

function onUp() {
  if (band) {
    const r = bandRect()
    if (r && Math.abs(band.x1 - band.x0) > 4 && Math.abs(band.y1 - band.y0) > 4) emit('band', r)
    band = null
    banding.value = false
    schedule()
  }
  if (drag?.moved) saveView()
  else if (drag) emit('pick', fromScreen(drag.x, drag.y))
  drag = null
}

function onWheel(e: WheelEvent) {
  const [x, y] = [e.offsetX, e.offsetY]
  const [lon, lat] = fromScreen(x, y)
  const zoom = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, view.value.zoom - e.deltaY * 0.002))
  // Keep the point under the cursor where it is.
  const [px, py] = worldPx(lon, lat, zoom)
  const [clon, clat] = lonLat(px - (x - size.w / 2), py - (y - size.h / 2), zoom)
  view.value = { lon: clon, lat: clat, zoom }
  schedule()
  saveView()
}

/** The view on a rectangle [west, south, east, north], with a margin. */
function fitBox(b: number[]) {
  const [x0, y0] = worldPx(b[0]!, Math.min(85, b[3]!), 0)
  const [x1, y1] = worldPx(b[2]!, Math.max(-85, b[1]!), 0)
  const zoom = Math.log2(Math.min(size.w / Math.max(1e-9, x1 - x0), size.h / Math.max(1e-9, y1 - y0)) * 0.8)
  const [lon, lat] = lonLat((x0 + x1) / 2, (y0 + y1) / 2, 0)
  view.value = { lon, lat, zoom: Math.max(MIN_ZOOM, Math.min(16, zoom)) }
  schedule()
  saveView()
}

/** A geometry's extent [west, south, east, north], or null for none. */
function extentOf(geometries: Geometry[]): number[] | null {
  let w = Infinity, s = Infinity, e = -Infinity, n = -Infinity
  for (const g of geometries) {
    for (const poly of polygons(g)) {
      for (const [lon, lat] of poly[0] ?? []) {
        w = Math.min(w, lon!); e = Math.max(e, lon!); s = Math.min(s, lat!); n = Math.max(n, lat!)
      }
    }
  }
  return Number.isFinite(w) ? [w, s, e, n] : null
}

function saveView() {
  try { localStorage.setItem(props.viewKey, JSON.stringify(view.value)) } catch { /* private window */ }
}

function restoreView() {
  try {
    const got = JSON.parse(localStorage.getItem(props.viewKey) ?? 'null') as typeof view.value | null
    if (got && Number.isFinite(got.zoom)) view.value = got
  } catch { /* nothing kept */ }
}

function resize() {
  const element = wrap.value, surface = canvas.value
  if (!element || !surface) return
  const dpr = window.devicePixelRatio || 1
  size = { w: element.clientWidth, h: element.clientHeight, dpr }
  surface.width = Math.round(size.w * dpr)
  surface.height = Math.round(size.h * dpr)
  surface.style.width = `${size.w}px`
  surface.style.height = `${size.h}px`
  ctx = surface.getContext('2d')
  ctx?.setTransform(dpr, 0, 0, dpr, 0, 0)
  schedule()
}

let observer: ResizeObserver | null = null
onMounted(async () => {
  await nextTick()
  restoreView()
  resize()
  observer = new ResizeObserver(resize)
  if (wrap.value) observer.observe(wrap.value)
})
onUnmounted(() => {
  observer?.disconnect()
  if (frame) cancelAnimationFrame(frame)
})
watch(() => [props.shapes, props.pin], schedule, { deep: true })

defineExpose({ fitBox, extentOf, schedule })
</script>

<style scoped>
/* Not `.sm`: Quasar's visibility classes (xs, sm, md, …) would hide it. */
.slippy { position: relative; overflow: hidden; }
.slippy canvas { display: block; cursor: grab; touch-action: none; }
.slippy canvas:active { cursor: grabbing; }
.slippy canvas.slippy-banding { cursor: crosshair; }
.slippy-credit {
  position: absolute; right: 6px; bottom: 4px; font-size: 10px; color: #374151;
  background: rgba(255, 255, 255, 0.75); padding: 1px 5px; border-radius: 3px;
}
.slippy-credit a { color: inherit; }
</style>
