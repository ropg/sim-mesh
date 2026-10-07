<template>
  <q-page class="sp">
    <div class="sp-list">
      <div class="sp-head">
        <span class="sp-heading">Scripts</span>
        <q-space />
        <q-btn flat dense no-caps size="sm" label="New…" @click="askNew" />
      </div>
      <q-list dense>
        <q-item v-for="s in runnable" :key="s.name" clickable :active="s.name === current"
                active-class="sp-active" @click="openScript(s.name)">
          <q-item-section>
            <q-item-label>{{ s.name }}</q-item-label>
            <q-item-label caption class="sp-doc">{{ s.error ?? s.doc }}</q-item-label>
          </q-item-section>
        </q-item>
      </q-list>
      <!-- Scripts others include: parts of those, opened here to edit, never run alone. -->
      <template v-if="included.length">
        <div class="sp-head sp-subhead"><span class="sp-heading">Included by scripts</span></div>
        <q-list dense>
          <q-item v-for="s in included" :key="s.name" clickable :active="s.name === current"
                  active-class="sp-active" @click="openScript(s.name)">
            <q-item-section>
              <q-item-label>{{ s.name }}</q-item-label>
              <q-item-label caption class="sp-doc">{{ s.error ?? `part of ${s.included_by!.join(', ')}` }}</q-item-label>
            </q-item-section>
          </q-item>
        </q-list>
      </template>
    </div>

    <div class="sp-main-pane">
      <template v-if="current || setup">
        <q-toolbar class="sp-bar">
          <span class="sp-title">{{ current ?? `${setup} setup` }}<span v-if="anyDirty" class="sp-dirty"> •</span></span>
          <q-btn flat dense no-caps label="Save" :disable="!tab || tab.readonly || (!tabDirty(tab) && !tab.fresh)"
                 @click="save" />
          <q-btn v-if="!setup" flat dense no-caps label="Save as…" :disable="!tab || tab.readonly || !!tab.nodeset"
                 @click="askSaveAs" />
          <q-space />
          <q-btn v-if="!setup" unelevated dense no-caps color="primary" label="Run…"
                 :disable="!info || !!info.error || !!includedBy.length" @click="running = true">
            <q-tooltip>{{ includedBy.length
              ? `Part of ${includedBy.join(', ')}, which include it: run one of those`
              : 'Run it from its top: a new simulation of its own, or on a running one' }}</q-tooltip>
          </q-btn>
        </q-toolbar>
        <!-- What the script asks for before it runs (its script_input(…)s), chosen here. -->
        <div v-if="!setup && scriptInputs.length" class="sp-inputs">
          <div v-for="input in scriptInputs" :key="input.name" class="sp-input">
            <q-toggle v-if="input.type === 'bool'" :model-value="inputValues[input.name] === 'true'"
                      :label="input.label"
                      @update:model-value="(v: boolean) => { inputValues[input.name] = v ? 'true' : 'false' }" />
            <q-select v-else-if="input.type === 'run'" v-model="inputValues[input.name]"
                      :options="runChoices" dense outlined emit-value map-options
                      :label="input.label" class="sp-input-field" />
            <q-select v-else-if="input.type === 'firmware'" v-model="inputValues[input.name]"
                      :options="firmwareChoices(input.category)" dense outlined emit-value map-options
                      :label="input.label" class="sp-input-field"
                      :hint="input.category ? `${input.category} firmware` : undefined">
              <template #no-option>
                <q-item><q-item-section class="text-grey-6">
                  No {{ input.category ?? '' }} firmware installed: add some on the Firmware tab
                </q-item-section></q-item>
              </template>
            </q-select>
            <q-input v-else v-model="inputValues[input.name]" dense outlined :label="input.label"
                     :type="input.type === 'int' || input.type === 'float' ? 'number' : 'text'"
                     :step="input.type === 'int' ? 1 : 'any'" class="sp-input-field" />
          </div>
        </div>
        <!-- The script, every file it imports (another script to edit, or the
             library's own module to read), scripts/globals.py, and the setup
             file of each nodeset it would run on. Or one nodeset's setup file alone. -->
        <q-tabs v-model="tabKey" dense no-caps inline-label align="left" class="sp-tabs"
                active-color="white" indicator-color="primary">
          <q-tab v-for="t in tabs" :key="t.key" :name="t.key">
            <span>{{ t.label }}<span v-if="tabDirty(t)" class="sp-dirty"> •</span></span>
            <span v-if="t.readonly" class="sp-ro">library</span>
          </q-tab>
        </q-tabs>
        <div class="sp-split">
          <textarea v-if="tab" v-model="tab.text" class="sp-editor" spellcheck="false" :readonly="tab.readonly"
                    :class="{ 'sp-readonly': tab.readonly }" @keydown.tab.prevent="indent" />
          <div v-if="shownRun && runOutput" class="sp-output">
            <div class="sp-output-head">
              <span>{{ runOutput.name }} on {{ runOutput.sim }}</span>
              <q-space />
              <q-btn v-if="runOutput.state === 'running'" flat dense no-caps size="sm" color="negative"
                     label="Stop" @click="stopRun(runOutput.run)" />
              <q-btn flat dense round size="sm" @click="shownRun = null">×</q-btn>
            </div>
            <pre ref="outputEl">{{ runOutput.lines.join('\n') }}</pre>
          </div>
        </div>
      </template>
      <div v-else class="sp-intro">
        <div class="sp-heading">Scripts are Python against the sim_mesh library</div>
        <p>
          A script runs from its top to its end, each call doing what it says:
          <code>from sim_mesh import *</code>, then its declarations,
          <code>sim_speed("real")</code> (or <code>"max"</code>, or a pace such as
          <code>10</code>), <code>nodes().firmware(name)</code> (an installed
          firmware, or <code>"&lt;base&gt;_latest"</code>; most scripts ask for
          theirs with <code>script_input("firmware", type=Firmware)</code>, chosen above
          the script) and <code>.on_first_boot(rules)</code>, then what it does to a
          selection, <code>nodes(tag=…)</code> or <code>node(name)</code>, combined with
          <code>&amp; | - ~</code>: <code>.up()</code>, <code>.exec(lines)</code>,
          <code>.radio(…)</code>, <code>.reticulum.lxmf.announce()</code>,
          <code>.reticulum.lxmf.send(to, text)</code>, <code>.reset()</code>; and to
          the simulation: <code>sim_wait(s)</code>, <code>sim_snapshot(name)</code>,
          <code>sim_pause()</code>.
        </p>
        <p>
          A script's declarations start with <code>script_include("scripts/startup.py")</code>,
          which tells every node its <code>Node.radio(…)</code> and, for a node tagged with
          one, its <code>Node.reticulum.role(…)</code> in <code>.on_first_boot</code>, then
          runs each nodeset's own <code>nodesets/&lt;name&gt;.py</code>, then starts the
          radios with <code>Node.radio_up()</code>. The radio itself, the
          one the map draws, is in <code>scripts/globals.py</code>; a node tagged
          <code>no-radio</code> has none.
        </p>
        <p>
          Run starts its simulation on the Nodes tab's geodata and nodesets and
          goes over to it; it runs on when the script ends, unless the script
          paused it. <code>def report(run_dir)</code> returns the run's report as
          Markdown.
        </p>
      </div>
    </div>

    <q-dialog v-model="running">
      <q-card style="min-width: 420px">
        <q-card-section class="text-subtitle2">Run {{ current }}</q-card-section>
        <q-card-section class="column q-gutter-sm">
          <q-btn-toggle v-model="runOn" dense no-caps unelevated toggle-color="primary"
                        :options="[{ label: 'a new simulation', value: 'new' },
                                   { label: 'on a running one', value: 'running' },
                                   { label: 'on a paused or done one', value: 'paused' }]" />
          <q-select v-if="runOn === 'running'" v-model="runSim" :options="runningNames" dense outlined
                    label="simulation" />
          <template v-else-if="runOn === 'paused'">
            <q-select v-model="runPaused" :options="pausedNames" dense outlined label="simulation" />
            <div class="sp-world-note">It goes on as it ended, in the script's own time (real unless its <code>time(…)</code> says otherwise), and its firmware and first-boot lines apply to it.</div>
          </template>
          <div v-else class="sp-world">
            <div><span>geodata</span><b>{{ nodes.geodata ?? 'none: choose it on the Geodata tab' }}</b></div>
            <div><span>{{ runLayers.length > 1 ? 'nodesets, merged' : 'nodeset' }}</span>
              <b>{{ runLayers.length ? runLayers.join(', ') : 'none: show a layer on the Nodes tab' }}</b></div>
            <div class="sp-world-note">What the Nodes tab shows; change it there. Real or virtual time is the script's own <code>time(…)</code> line.</div>
          </div>
        </q-card-section>
        <q-card-actions align="right">
          <q-btn flat no-caps label="Cancel" v-close-popup />
          <q-btn flat no-caps color="primary" label="Run" :loading="starting" :disable="!runReady" @click="run" />
        </q-card-actions>
      </q-card>
    </q-dialog>
  </q-page>
</template>

<script setup lang="ts">
/* Scripts: each one's text and the files it imports, in tabs, edited here
 * and saved through the front, which checks a script parses; Run starts it
 * as a process of the front's, and its output comes back here as it is
 * written. With `?setup=<nodeset>` the page is that nodeset's setup file
 * alone, which the Nodes tab's Edit setup opens. */
import { computed, nextTick, reactive, ref, watch } from 'vue'
import { useQuasar } from 'quasar'
import { useRoute, useRouter } from 'vue-router'
import { useCatalog, type ScriptInput, type ScriptRow } from '../stores/catalog'
import { useSim } from '../stores/sim'
import { saveIfEditing, useNodes } from '../stores/nodes'
import { request } from '../lib/front'

/** One tab: a script of the store (editable), a nodeset's setup file
 *  (editable, saved with nodeset_setup_save), or a library module
 *  (read-only). `fresh`: a setup file not written yet, its template, which
 *  Save writes. */
interface Tab {
  key: string; label: string; script: string | null; nodeset: string | null
  readonly: boolean; fresh: boolean; text: string; saved: string
}

const GLOBALS_PATH = 'scripts/globals.py'

const catalog = useCatalog()
const sim = useSim()
const nodes = useNodes()
const quasar = useQuasar()
const route = useRoute()
const router = useRouter()
const current = ref<string | null>(null)
/** The nodeset whose setup file the page shows alone, or null. */
const setup = ref<string | null>(null)
const info = ref<ScriptRow | null>(null)
const tabs = ref<Tab[]>([])
const tabKey = ref<string | null>(null)
const running = ref(false)
const starting = ref(false)
const runOn = ref<'running' | 'paused' | 'new'>('new')
const runSim = ref<string | null>(null)
const shownRun = ref<string | null>(null)
const outputEl = ref<HTMLPreElement>()

const tab = computed(() => tabs.value.find(t => t.key === tabKey.value) ?? null)
/** The scripts to run, and those other scripts include (startup, globals). */
const runnable = computed(() => catalog.scripts.filter(s => !s.included_by?.length))
const included = computed(() => catalog.scripts.filter(s => !!s.included_by?.length))
const includedBy = computed(() => catalog.scripts.find(s => s.name === current.value)?.included_by ?? [])
function tabDirty(t: Tab) { return !t.readonly && t.text !== t.saved }
const anyDirty = computed(() => tabs.value.some(tabDirty))
/** What a new simulation runs on: the Nodes tab's shown layers, the active
 *  one first, merged when there are several. */
const runLayers = computed<string[]>(() => {
  const active = nodes.nodeset?.name
  const shown = nodes.layers.filter(l => l.shown && l.name !== active).map(l => l.name)
  return [...(active ? [active] : []), ...shown]
})
const runningNames = computed(() => sim.sims.filter(s => s.state === 'running').map(s => s.name))
const pausedNames = computed(() => sim.sims.filter(s => s.state === 'paused').map(s => s.name))
const runPaused = ref<string | null>(null)
/** The open script's inputs, and the values chosen for them, kept per script. */
const scriptInputs = computed<ScriptInput[]>(() => info.value?.inputs ?? [])
const chosenInputs = reactive<Record<string, Record<string, string | null>>>({})
watch([current, scriptInputs], () => {
  const name = current.value ?? ''
  const mine = chosenInputs[name] ?? (chosenInputs[name] = {})
  for (const input of scriptInputs.value) {
    if (!(input.name in mine)) {
      mine[input.name] = input.default != null ? String(input.default)
        : input.type === 'bool' ? 'false' : null
    }
  }
}, { immediate: true })

/** The runs a run input offers: those that are not running, newest first. */
const runChoices = computed(() => sim.sims
  .filter(s => s.state === 'ended' || s.state === 'paused')
  .map(s => (s.run ?? '').split('/').pop() ?? '')
  .filter(r => r)
  .map(r => ({ label: r, value: r })))
const inputValues = computed<Record<string, string | null>>(() =>
  chosenInputs[current.value ?? ''] ?? {})
const inputsReady = computed(() => scriptInputs.value.every(i => !!inputValues.value[i.name]))

/** A base that ends in the upstream release it packages, `<family>-1.2.3`. */
const UPSTREAM_RE = /^(.+)-(\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?)$/

/** The installed firmware a firmware input offers: the newest of each base
 *  (`<base>_latest`), and of each family of bases that name their upstream
 *  release, first, then every one by name; of `category` when the input
 *  names one. */
function firmwareChoices(category?: string) {
  const rows = catalog.firmware.filter(f => !f.error && (!category || f.category === category))
  const families = rows.map(f => UPSTREAM_RE.exec(f.base)?.[1]).filter((b): b is string => !!b)
  const bases = [...new Set([...rows.map(f => f.base), ...families])].sort()
  return [
    ...bases.map(b => ({ label: `${b}_latest — the newest ${b}`, value: `${b}_latest` })),
    ...rows.map(f => ({ label: f.title ? `${f.name} — ${f.title}` : f.name, value: f.name })),
  ]
}

const runReady = computed(() => inputsReady.value && (runOn.value === 'running' ? !!runSim.value
  : runOn.value === 'paused' ? !!runPaused.value
    : !!(nodes.geodata && runLayers.value.length)))
const runOutput = computed(() => (shownRun.value ? catalog.runs[shownRun.value] ?? null : null))

watch(running, (open) => {
  if (!open) return
  runSim.value = runSim.value ?? sim.selected ?? runningNames.value[0] ?? null
  if (!runPaused.value || !pausedNames.value.includes(runPaused.value)) {
    runPaused.value = pausedNames.value[0] ?? null
  }
  if (runOn.value === 'running' && !runningNames.value.length) runOn.value = 'new'
  if (runOn.value === 'paused' && !pausedNames.value.length) runOn.value = 'new'
})
watch(() => runOutput.value?.lines.length, async () => {
  await nextTick()
  const el = outputEl.value
  if (el) el.scrollTop = el.scrollHeight
})

function tell(error: string | null | undefined, done?: string) {
  if (error) quasar.notify({ type: 'negative', message: error, timeout: 8000 })
  else if (done) quasar.notify({ type: 'positive', message: done, timeout: 2500 })
}

function guard(go: () => void, kept?: () => void) {
  if (!anyDirty.value) { go(); return }
  const what = current.value ? `${current.value} and what it imports` : `${setup.value}'s setup`
  quasar.dialog({ title: 'Unsaved changes', message: `Discard the changes to ${what}?`,
                  cancel: true, persistent: true }).onOk(go).onCancel(() => kept?.())
}

/** A file of the store as a tab: a script, when it is one of scripts/, else read-only. */
function moduleTab(path: string, library: boolean, text: string): Tab {
  const script = library ? null : path.replace(/^scripts\//, '').replace(/\.py$/, '')
  return { key: `ref:${path}`, label: path.replace(/^scripts\//, ''), script, nodeset: null,
           readonly: library, fresh: false, text, saved: text }
}

function setupTab(name: string, text: string, exists: boolean): Tab {
  return { key: `setup:${name}`, label: `nodesets/${name}.py`, script: null, nodeset: name,
           readonly: false, fresh: !exists, text, saved: text }
}

/** A script in its tab, a tab for each file it imports and for
 *  scripts/globals.py, and one for each nodeset of the world it would run on
 *  that has a setup file. */
async function show(name: string, row: ScriptRow, text: string) {
  current.value = name
  setup.value = null
  if (route.query.setup !== undefined) void router.replace({ query: {} })
  info.value = row
  const opened: Tab[] = [{ key: `script:${name}`, label: `${name}.py`, script: name, nodeset: null,
                           readonly: false, fresh: false, text, saved: text }]
  const refs = row.references ?? []
  const paths = [...refs.map(r => ({ path: r.path, library: r.library })),
                 ...(refs.some(r => r.path === GLOBALS_PATH) ? [] : [{ path: GLOBALS_PATH, library: false }])]
  for (const ref of paths) {
    const r = await request('module_open', { path: ref.path })
    if (r.ok) opened.push(moduleTab(ref.path, ref.library, r.text as string))
  }
  for (const layer of runLayers.value) {
    const r = await request('nodeset_setup_open', { name: layer })
    if (r.ok && r.exists) opened.push(setupTab(layer, r.text as string, true))
  }
  tabs.value = opened
  tabKey.value = opened[0]!.key
}

/** One nodeset's setup file alone, its template when it has none yet. */
async function showSetup(name: string) {
  const r = await request('nodeset_setup_open', { name })
  if (!r.ok) { tell(r.error); return }
  current.value = null
  info.value = null
  setup.value = name
  tabs.value = [setupTab(name, r.text as string, !!r.exists)]
  tabKey.value = tabs.value[0]!.key
}

function openScript(name: string) {
  guard(async () => {
    const r = await request('script_open', { name })
    if (!r.ok) { tell(r.error); return }
    await show(name, r.script as ScriptRow, r.text as string)
  })
}

// `?script=<name>&geodata=<g>&nodeset=<n>…&set.<input>=<value>…`, as `sim run`
// opens the page when a script lacks an input: the script open on its world,
// the inputs given filled in, and Run… asked, for the rest to be chosen.
watch(() => route.query.script, (name) => {
  if (typeof name !== 'string' || !name) return
  const q = { ...route.query }
  void router.replace({ query: {} })
  sim.show('scripts')
  guard(() => {
    void (async () => {
      const layers = ([] as unknown[]).concat(q.nodeset ?? []).filter((l): l is string =>
        typeof l === 'string' && !!l)
      if (typeof q.geodata === 'string' && q.geodata) await nodes.chooseGeodata(q.geodata)
      if (layers.length) {
        const error = await nodes.activate(layers[0]!)
        if (error) tell(error)
        for (const layer of layers.slice(1)) {
          if (!nodes.layers.find(l => l.name === layer)?.shown) await nodes.toggleLayer(layer)
        }
      }
      const r = await request('script_open', { name })
      if (!r.ok) { tell(r.error); return }
      await show(name, r.script as ScriptRow, r.text as string)
      const mine = chosenInputs[name] ?? (chosenInputs[name] = {})
      for (const [key, value] of Object.entries(q)) {
        if (key.startsWith('set.') && typeof value === 'string') mine[key.slice(4)] = value
      }
      running.value = true
    })()
  })
}, { immediate: true })

watch(() => route.query.setup, (name) => {
  if (typeof name !== 'string' || !name || name === setup.value) return
  sim.show('scripts')
  // Kept: the query goes, so asking for the same setup again comes back here.
  guard(() => { void showSetup(name) }, () => { void router.replace({ query: {} }) })
}, { immediate: true })

async function saveTab(t: Tab): Promise<boolean> {
  if (t.readonly || (!tabDirty(t) && !t.fresh)) return true
  if (t.nodeset) {
    const r = await request('nodeset_setup_save', { name: t.nodeset, text: t.text })
    if (!r.ok) { tell(r.error); return false }
    t.saved = t.text
    t.fresh = false
    return true
  }
  if (!t.script) return true
  const r = await request('script_save', { name: t.script, text: t.text })
  if (!r.ok) { tell(r.error); return false }
  t.saved = t.text
  if (t.script === current.value) info.value = r.script as ScriptRow
  return true
}

async function save() {
  if (tab.value && await saveTab(tab.value)) {
    tell(null, 'saved')
    void catalog.refresh()
  }
}

function askName(title: string, then: (name: string) => void) {
  quasar.dialog({ title, message: 'Lower-case letters, digits and hyphens.',
                  prompt: { model: '', type: 'text' }, cancel: true })
    .onOk((name: string) => { if (name.trim()) then(name.trim()) })
}

function askNew() {
  guard(() => askName('New script', async (name) => {
    const r = await request('script_new', { name })
    if (!r.ok) { tell(r.error); return }
    await show(name, r.script as ScriptRow, r.text as string)
    void catalog.refresh()
  }))
}

function askSaveAs() {
  const t = tab.value
  if (!t || t.readonly) return
  askName('Save script as', async (name) => {
    const r = await request('script_save_as', { name, text: t.text })
    if (!r.ok) { tell(r.error); return }
    await show(name, r.script as ScriptRow, t.text)
    void catalog.refresh()
  })
}

async function run() {
  for (const t of tabs.value) if (!await saveTab(t)) return
  if (runOn.value === 'new') {
    const error = await saveIfEditing(nodes.nodeset?.name ?? null)
    if (error) { tell(error); return }
  }
  starting.value = true
  const inputs = { ...inputValues.value }
  const r = await request('script_run', runOn.value === 'running'
    ? { name: current.value, sim: runSim.value, inputs }
    : runOn.value === 'paused'
      ? { name: current.value, resume: runPaused.value, inputs }
      : { name: current.value, geodata: nodes.geodata, nodesets: runLayers.value, inputs })
  starting.value = false
  if (!r.ok) { tell(r.error); return }
  running.value = false
  if (r.run) shownRun.value = r.run as string
  // Over to its live map, framed on its nodes once they are there: the
  // script starts its simulation itself, a moment from now.
  if (r.simulation) {
    sim.fitWanted = true
    sim.attach(r.simulation as string, true)
  }
}

async function stopRun(id: string) {
  const r = await request('script_stop', { run: id })
  if (!r.ok) tell(r.error)
}

/** Tab in the editor puts four spaces in, as Python wants. */
function indent(event: KeyboardEvent) {
  const t = tab.value
  if (!t || t.readonly) return
  const el = event.target as HTMLTextAreaElement
  const start = el.selectionStart, end = el.selectionEnd
  t.text = `${t.text.slice(0, start)}    ${t.text.slice(end)}`
  void nextTick(() => { el.selectionStart = el.selectionEnd = start + 4 })
}
</script>

<style scoped>
.sp { display: flex; height: 100%; }
.sp-world { font-size: 12px; }
.sp-world > div { display: flex; gap: 10px; padding: 2px 0; }
.sp-world span { color: #6b7280; width: 110px; flex: none; }
.sp-world b { font-weight: 500; color: #e5e7eb; }
.sp-world .sp-world-note { color: #6b7280; font-size: 11px; }
.sp-inputs {
  display: flex; flex-wrap: wrap; gap: 10px; padding: 8px 12px 10px;
  background: #14181d; border-bottom: 1px solid #262c35; flex: none;
}
.sp-input-field { min-width: 380px; }
.sp-subhead { padding-top: 12px; }
.sp-list { width: 280px; flex: none; border-right: 1px solid #262c35; overflow-y: auto; padding: 10px 0; }
.sp-head { display: flex; align-items: center; padding: 0 12px 4px; }
.sp-heading { font-size: 14px; font-weight: 500; color: #d1d5db; }
.sp-doc { font-size: 11px; color: #6b7280 !important; }
.sp-active { background: #1a2029; color: #fff; }
.sp-main-pane { flex: 1 1 auto; min-width: 0; display: flex; flex-direction: column; }
.sp-bar { min-height: 38px; gap: 6px; background: #171b21; border-bottom: 1px solid #262c35; flex: none; }
.sp-tabs { background: #13171d; border-bottom: 1px solid #262c35; flex: none; font-size: 12px; }
.sp-ro { margin-left: 6px; font-size: 10px; color: #6b7280; border: 1px solid #374151; border-radius: 3px; padding: 0 4px; }
.sp-title { font-size: 14px; font-weight: 500; padding: 0 8px; }
.sp-dirty { color: #f59e0b; }
.sp-split { flex: 1 1 auto; min-height: 0; display: flex; flex-direction: column; }
.sp-editor {
  flex: 1 1 60%; min-height: 0; resize: none; border: none; outline: none; padding: 10px 14px;
  background: #0e1116; color: #d1d5db; font: 13px/1.5 ui-monospace, monospace; tab-size: 4;
}
.sp-readonly { color: #9ca3af; background: #0b0d10; }
.sp-output { flex: 1 1 40%; min-height: 0; display: flex; flex-direction: column; border-top: 1px solid #262c35; }
.sp-output-head { display: flex; align-items: center; padding: 2px 8px 2px 12px; font-size: 12px; color: #9ca3af; }
.sp-output pre {
  flex: 1 1 auto; margin: 0; overflow: auto; padding: 6px 12px; background: #0b0d10;
  font: 12px/1.4 ui-monospace, monospace; color: #c7ccd4; white-space: pre-wrap; word-break: break-word;
}
.sp-intro { padding: 16px 20px; max-width: 720px; font-size: 13px; color: #9ca3af; line-height: 1.6; }
</style>
