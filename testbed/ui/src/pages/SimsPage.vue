<template>
  <q-page class="sims-page">
    <div class="sims-body">
      <div class="sims-head">
        <div class="sims-heading">Simulation runs</div>
      </div>

      <div v-if="!sim.sims.length" class="sims-none">
        No simulation runs yet. A script's Run on the Scripts tab starts one, on
        the Nodes tab's geodata and nodeset, running what the script's
        <code>.firmware(…)</code> says; so does <code>sim run &lt;script&gt;
        --geodata &lt;g&gt; --nodeset &lt;n&gt;</code> from a shell.
      </div>

      <template v-else>
      <SelectBar :sel="sel">
        <template #default="{ keys }">
          <q-btn flat dense no-caps size="sm" :icon="matStop" label="Stop" color="negative"
                 :disable="!rowsOf(keys).some(stoppable)" @click="askBulk('stop', keys)" />
          <q-btn flat dense no-caps size="sm" :icon="matPause" label="Pause"
                 :disable="!rowsOf(keys).some(s => s.state === 'running')" @click="askBulk('pause', keys)" />
          <q-btn flat dense no-caps size="sm" :icon="matDeleteOutline" label="Delete"
                 :disable="!keys.length" @click="askBulk('delete', keys)" />
        </template>
      </SelectBar>
      <table class="sims-table tab-flow">
        <thead>
          <tr>
            <th class="sims-check"></th>
            <th>Simulation</th>
            <th>Nodeset</th>
            <th>Time</th>
            <th>Plan</th>
            <th>Finish</th>
            <th class="num">Stations</th>
            <th class="num">Size</th>
            <th class="sims-icon"></th>
            <th class="sims-icon"></th>
            <th class="sims-icon"></th>
            <th class="sims-icon"></th>
            <th class="sims-icon"></th>
            <th class="sims-icon"></th>
          </tr>
        </thead>
        <tbody>
          <template v-for="s in sim.sims" :key="s.run">
            <tr :class="{ 'sims-selected': s.name === sim.selected, 'sims-exited': s.state === 'exited',
                          'sims-open': s.state === 'running', 'sims-chosen': sel.has(s.run) }"
                @click="s.state === 'running' && sim.attach(s.name)">
              <td class="sims-check" @click.stop>
                <q-checkbox dense size="xs" :model-value="sel.has(s.run)" @update:model-value="sel.toggle(s.run)" />
              </td>
              <td>
                <span :class="s.state === 'running' ? 'sims-name' : 'sims-name-off'">{{ s.name }}</span>
                <div class="sims-state" :class="`sims-state-${stateOf(s)}`">
                  <q-icon v-if="stateOf(s) === 'done'" :name="matCheck" size="12px" />
                  <q-icon v-else-if="stateOf(s) === 'paused'" :name="matPause" size="12px" />
                  {{ stateText(s) }}
                </div>
              </td>
              <td>
                {{ s.nodeset ?? '—' }}<span v-if="s.dirty" class="sims-dirty"> •</span>
                <div class="sims-sub">
                  on {{ s.geodata ?? '—' }}<template v-if="s.script">, set up by {{ s.script }}</template><template
                    v-if="s.snapshot">, from snapshot {{ s.snapshot }}</template>
                </div>
                <div class="sims-sub" :title="`control 127.0.0.1:${s.port}, ether ${s.ether}, run ${s.run}`">
                  {{ s.net }}
                </div>
                <div v-if="sim.progress[s.name]?.running" class="sims-sub sims-losses">
                  loss table {{ sim.progress[s.name]!.band }}: {{ sim.progress[s.name]!.done }}/{{ sim.progress[s.name]!.total }}
                </div>
              </td>
              <td class="mono">
                <div v-if="simText(s)">{{ simText(s) }} <span class="sims-sub">simulated</span></div>
                <div v-if="realText(s)">{{ realText(s) }} <span class="sims-sub">real</span></div>
                <div v-if="speedText(s)" class="sims-sub">{{ speedText(s) }}</div>
                <span v-if="!simText(s) && !realText(s)" class="sims-sub">—</span>
              </td>
              <td>
                <template v-if="phaseText(s)">
                  <div>{{ phaseText(s) }}</div>
                  <div class="sims-phases">
                    <span v-for="(p, i) in s.plan ?? []" :key="i" class="sims-phase"
                          :class="{ 'sims-phase-now': s.phase?.index === i,
                                    'sims-phase-past': s.phase ? i < s.phase.index : s.done }"
                          :style="{ flexGrow: phaseWeight(s, i) }"
                          :title="p.name">
                      <span v-if="s.phase?.index === i" class="sims-phase-fill"
                            :style="{ width: `${phaseProgress(s) * 100}%` }" />
                    </span>
                  </div>
                </template>
                <span v-else class="sims-sub">{{ s.state === 'ended' || s.state === 'paused' ? '—' : 'no plan' }}</span>
              </td>
              <td>
                <template v-if="etaText(s)">
                  {{ etaText(s) }}
                  <div v-if="s.phase?.eta" class="sims-sub">
                    {{ s.phase.name }} ends {{ clockTime(s.phase.eta) }}
                  </div>
                </template>
                <span v-else-if="s.state === 'paused'" class="sims-sub">
                  {{ stateOf(s) }} {{ s.paused_at ? clockTime(Date.parse(s.paused_at) / 1000) : '' }}
                </span>
                <span v-else-if="s.state === 'ended'" class="sims-sub">
                  started {{ s.started_at ? clockTime(Date.parse(s.started_at) / 1000) : '—' }}
                </span>
                <span v-else class="sims-sub">{{ s.plan && !s.done ? 'pace not known yet' : '—' }}</span>
              </td>
              <td class="num mono">
                <template v-if="s.state === 'paused' || s.state === 'ended'">{{ s.stations ?? '—' }}</template>
                <template v-else>
                  <span :class="{ 'sims-all-up': s.stations && up(s) === s.stations }">
                    {{ up(s) }}/{{ s.stations }}
                  </span>
                  <div class="sims-sub">{{ others(s) }}</div>
                </template>
              </td>
              <td class="num mono">{{ sizeText(s.bytes) }}</td>
              <td class="sims-icon" @click.stop>
                <q-btn v-if="s.state === 'paused'" flat dense round size="sm" color="primary"
                       :icon="matPlayArrow" aria-label="Resume" @click="sim.resumeSim(s.name)">
                  <q-tooltip>Resume: start it again as it ended, in real time, in a new run directory; a script runs on it from the Scripts tab (on a paused one)</q-tooltip>
                </q-btn>
              </td>
              <td class="sims-icon" @click.stop>
                <q-btn v-if="s.state === 'running'" flat dense round size="sm" color="primary"
                       :icon="matPause" aria-label="Pause" @click="sim.pauseSim(s.name)">
                  <q-tooltip>Pause: stop it with its state kept; it stays listed, to be resumed as it ended</q-tooltip>
                </q-btn>
              </td>
              <td class="sims-icon" @click.stop>
                <q-btn v-if="s.state === 'running' || s.state === 'starting' || s.state === 'stopping' || s.state === 'paused'"
                       flat dense round size="sm" color="negative" :icon="matStop" aria-label="Stop"
                       :disable="s.state === 'stopping'" @click="askStop(s)">
                  <q-tooltip>{{ s.state === 'paused'
                    ? 'Stop for good: the state it was kept with goes, so it can no longer be resumed'
                    : 'Stop for good: its run stays on disk, its state is not kept for resuming' }}</q-tooltip>
                </q-btn>
              </td>
              <td class="sims-icon" @click.stop>
                <q-btn v-if="s.state === 'running'" flat dense round size="sm" aria-label="More">
                  <span class="sims-more">⋯</span>
                  <q-menu auto-close anchor="bottom right" self="top right">
                    <q-list dense style="min-width: 210px">
                      <q-item clickable @click="to(s.name, 'reset_all')">
                        <q-item-section>Reset all</q-item-section>
                        <q-item-section side><span class="sims-sub">restart</span></q-item-section>
                      </q-item>
                      <q-item clickable @click="askFactory(s.name)">
                        <q-item-section>Factory reset all</q-item-section>
                        <q-item-section side><span class="sims-sub">wipe state</span></q-item-section>
                      </q-item>
                      <q-separator />
                      <q-item clickable @click="askSnapshot(s.name)"><q-item-section>Save snapshot as…</q-item-section></q-item>
                      <q-item clickable :disable="!catalog.snapshots.length" @click="restoring = s.name">
                        <q-item-section>Load snapshot into it…</q-item-section>
                      </q-item>
                    </q-list>
                  </q-menu>
                </q-btn>
              </td>
              <td class="sims-icon" @click.stop>
                <q-btn v-if="s.report" flat dense round size="sm" :icon="matDescription" aria-label="Report"
                       @click="reportOf = s.run">
                  <q-tooltip>The run's report</q-tooltip>
                </q-btn>
              </td>
              <td class="sims-icon" @click.stop>
                <q-btn flat dense round size="sm" :icon="matDeleteOutline" color="grey-6" aria-label="Delete"
                       :disable="s.state === 'stopping'" @click="askBulk('delete', [s.run])">
                  <q-tooltip>Delete the run: its directory, logs, report{{ s.state === 'paused'
                    ? ' and kept state' : '' }}{{ live(s) ? '; it is stopped first' : '' }}</q-tooltip>
                </q-btn>
              </td>
            </tr>
            <tr v-if="s.state === 'exited' && s.tail.length" class="sims-tail-row">
              <td /><td colspan="13"><pre class="sims-tail">{{ s.tail.join('\n') }}</pre></td>
            </tr>
            <tr v-else-if="s.errors.length" class="sims-tail-row">
              <td /><td colspan="13" class="sims-errors">
                <div v-for="([at, text], i) in s.errors" :key="i">
                  <span class="mono">{{ clockTime(at) }}</span> {{ text }}
                </div>
              </td>
            </tr>
          </template>
        </tbody>
      </table>
      </template>

      <div class="sims-sub q-mt-lg">
        Each simulation is its own simd with its own ether, stations, network and
        run directory under <code>testbed/runs/</code>. A station's web UI is at
        <code>http://&lt;station&gt;.&lt;simulation&gt;.sim.localhost:{{ socket.port }}/</code>.
        A new simulation's loss tables come from the cache, or are computed
        through the planner before it starts, with progress in its row.
      </div>
    </div>

    <!-- Load a snapshot into a running simulation. -->
    <q-dialog v-model="restoringOpen">
      <q-card style="min-width: 360px">
        <q-card-section class="text-subtitle2">Load a snapshot into {{ restoring }}</q-card-section>
        <q-card-section class="text-caption text-grey-6 q-pt-none">
          The network as it was: its nodeset, script and loss table, and every
          station's identity, keys, paths and message history. What the
          simulation is running now goes.
        </q-card-section>
        <q-list dense bordered separator style="max-height: 50vh; overflow-y: auto">
          <q-item v-for="s in catalog.snapshots" :key="s.name" clickable v-close-popup
                  @click="to(restoring!, 'snapshot_load', { name: s.name })">
            <q-item-section>{{ s.name }}</q-item-section>
            <q-item-section side>
              <span class="text-caption text-grey-6">{{ s.nodeset ?? '' }} · {{ sizeText(s.bytes as number | undefined) }}</span>
            </q-item-section>
          </q-item>
        </q-list>
        <q-card-actions align="right"><q-btn flat no-caps label="Cancel" v-close-popup /></q-card-actions>
      </q-card>
    </q-dialog>

    <ReportDialog :run="reportOf" @close="reportOf = null" />
  </q-page>
</template>

<script setup lang="ts">
import { computed, onMounted, ref, watch } from 'vue'
import { useQuasar } from 'quasar'
import { useSim, type SimSummary } from '../stores/sim'
import { useCatalog } from '../stores/catalog'
import { useSocket } from '../stores/socket'
import { clockTime, etaText, phaseText, realText, simText, speedText } from '../components/runtime'
import ReportDialog from '../components/ReportDialog.vue'
import SelectBar from '../components/SelectBar.vue'
import { useSelection } from '../lib/selection'
import { sizeText } from '../lib/size'
import {
  matCheck, matDeleteOutline, matDescription, matPause, matPlayArrow, matStop,
} from '@quasar/extras/material-icons'

const sim = useSim()
const catalog = useCatalog()
const socket = useSocket()
const quasar = useQuasar()
/** The rows chosen by their checkboxes, by run directory. */
const sel = useSelection(() => sim.sims.map(s => s.run))

/** A row's state as the list says it: a pause its script asked for is the
 *  script done, drawn apart from a pause a person asked for. */
function stateOf(s: SimSummary): string {
  return s.state === 'paused' && s.paused_by === 'script' ? 'done' : s.state
}

function stateText(s: SimSummary) {
  return s.state === 'exited' ? `exited (${s.code})` : stateOf(s)
}

/** A simulation still on its run: deleting the run stops it first. */
function live(s: SimSummary) {
  return s.state === 'running' || s.state === 'starting' || s.state === 'stopping' || s.state === 'exited'
}

/** What Stop applies to: a running one, a paused or done one (its kept state
 *  goes), and an exited one, which leaves the registry. */
function stoppable(s: SimSummary) {
  return s.state === 'running' || s.state === 'starting' || s.state === 'paused' || s.state === 'exited'
}

function rowsOf(keys: string[]): SimSummary[] {
  const chosen = new Set(keys)
  return sim.sims.filter(s => chosen.has(s.run))
}

function count(n: number, one: string, many = `${one}s`) { return `${n} ${n === 1 ? one : many}` }

/* One confirmation for what the chosen rows get: each action skips the rows
 * it does not apply to, and says so. */
function askBulk(action: 'stop' | 'pause' | 'delete', keys: string[]) {
  const rows = rowsOf(keys)
  const fits = rows.filter(action === 'stop' ? stoppable
    : action === 'pause' ? (s: SimSummary) => s.state === 'running' : () => true)
  if (!fits.length) return
  const skipped = rows.length - fits.length
  const names = fits.map(s => s.state === 'ended' ? s.run.split('/').pop()! : s.name).join(', ')
  let message: string
  if (action === 'delete') {
    const running = fits.filter(live).length
    const kept = fits.filter(s => s.state === 'paused').length
    message = `${names}: ${fits.length === 1 ? 'its run directory goes' : 'their run directories go'}, `
      + 'with logs, record and report.'
      + (running ? ` ${count(running, 'is', 'are')} still running and stopped first.` : '')
      + (kept ? ` ${count(kept, 'kept state')} goes, and can no longer be resumed.` : '')
  } else if (action === 'stop') {
    const kept = fits.filter(s => s.state === 'paused').length
    message = `${names}: stopped for good. Each run directory stays; nothing is saved to a `
      + 'nodeset or as a snapshot.'
      + (kept ? ` The state ${count(kept, 'paused or done one')} was kept with goes, so `
        + `${kept === 1 ? 'it' : 'they'} can no longer be resumed.` : '')
  } else {
    message = `${names}: stopped with ${fits.length === 1 ? 'its' : 'their'} state kept, to be resumed as `
      + `${fits.length === 1 ? 'it' : 'they'} ended.`
  }
  if (skipped) message += ` ${count(skipped, 'other')} chosen ${skipped === 1 ? 'is' : 'are'} left as ${skipped === 1 ? 'it is' : 'they are'}.`
  const title = action === 'delete' ? `Delete ${count(fits.length, 'run')}`
    : `${action === 'stop' ? 'Stop' : 'Pause'} ${count(fits.length, 'simulation')}`
  quasar.dialog({
    title, message,
    ok: { label: title.split(' ')[0], color: action === 'pause' ? 'primary' : 'negative', flat: true, noCaps: true },
    cancel: { flat: true, noCaps: true }, persistent: true,
  }).onOk(() => {
    for (const s of fits) {
      if (action === 'delete') sim.deleteRun(s.run.split('/').pop()!, true)
      else if (action === 'stop') sim.stopSim(s.name)
      else sim.pauseSim(s.name)
    }
    if (action === 'delete') sel.none()
  })
}
/** The run whose report is on show. */
const reportOf = ref<string | null>(null)
/** The simulation a snapshot is being chosen for, while the dialog is open. */
const restoring = ref<string | null>(null)
const restoringOpen = computed<boolean>({
  get: () => restoring.value !== null,
  set: (v) => { if (!v) restoring.value = null },
})

onMounted(() => { void catalog.refresh() })

/** One of a simulation's own verbs, sent to it through the front. */
function to(simName: string, type: string, fields: Record<string, unknown> = {}) {
  socket.send({ sim: simName, type, ...fields })
}

function askFactory(simName: string) {
  quasar.dialog({
    title: `Factory reset ${simName}`,
    message: 'Wipe every station\'s state and set it up again: identities, keys, paths and '
           + 'message history go.',
    cancel: true, persistent: true,
  }).onOk(() => to(simName, 'factory_reset_all'))
}

function askSnapshot(simName: string) {
  quasar.dialog({
    title: `Save a snapshot of ${simName}`, message: 'Lower-case letters, digits and hyphens.',
    prompt: { model: '', type: 'text' }, cancel: true,
  }).onOk((n: string) => { if (n.trim()) to(simName, 'snapshot_save_as', { name: n.trim() }) })
}

function up(s: SimSummary) { return s.counts.up ?? 0 }

/** The stations that are not up, by what they are doing instead. */
function others(s: SimSummary) {
  return Object.entries(s.counts)
    .filter(([status, n]) => status !== 'up' && n)
    .map(([status, n]) => `${n} ${status}`).join(', ')
}

/** A phase's share of the plan's bar: its length in T. */
function phaseWeight(s: SimSummary, i: number) {
  const plan = s.plan ?? []
  const from = i === 0 ? (s.plan_from ?? plan[0]!.until) : plan[i - 1]!.until
  return Math.max(1, plan[i]!.until - from)
}

function phaseProgress(s: SimSummary) {
  if (!s.phase || s.t === null) return 0
  const { from, until } = s.phase
  return Math.min(1, Math.max(0, (s.t - from) / Math.max(1, until - from)))
}

function askStop(s: SimSummary) {
  quasar.dialog({
    title: `Stop ${s.name}`,
    message: s.state === 'paused'
      ? 'The state it was kept with is deleted, so it can no longer be resumed; '
        + `the run directory stays, ended where it ${stateOf(s) === 'done' ? 'was done' : 'paused'}.`
      : 'Its stations are flushed and stopped and its simd ends. The run '
        + 'directory stays; nothing is saved to the nodeset or as a snapshot, and it cannot '
        + 'be resumed (pause keeps it resumable).',
    cancel: true,
    persistent: true,
  }).onOk(() => sim.stopSim(s.name))
}
</script>

<style scoped>
.sims-page { overflow-y: auto; height: 100%; }
.sims-body { padding: 16px 20px 32px; max-width: 1200px; }
.sims-head { display: flex; align-items: center; gap: 10px; margin-bottom: 10px; }
.sims-heading { font-size: 18px; font-weight: 500; color: #e5e7eb; margin-bottom: 12px; }
.sims-head .sims-heading { margin-bottom: 0; }
.sims-none { color: #6b7280; font-size: 13px; line-height: 1.5; max-width: 760px; }
.sims-table { width: 100%; border-collapse: collapse; font-size: 13px; }
.sims-table th {
  text-align: left; font-weight: 500; font-size: 11px; color: #6b7280;
  padding: 4px 10px; border-bottom: 1px solid #262c35; white-space: nowrap;
}
.sims-table td { padding: 8px 10px; border-bottom: 1px solid #1f242c; vertical-align: top; }
.sims-table .num { text-align: right; }
.mono { font-family: ui-monospace, monospace; font-size: 12px; }
.sims-selected td { background: #1a2029; }
.sims-exited td { color: #9ca3af; }
.sims-name { color: var(--q-primary); font-weight: 500; }
.sims-open { cursor: pointer; }
.sims-open:hover td { background: #1b2028; }
.sims-state-paused { color: #a78bfa; }
.sims-state-done { color: #2dd4bf; }
.sims-state-ended { color: #6b7280; }
.sims-check { width: 28px; padding-left: 2px !important; padding-right: 0 !important; }
.sims-icon { width: 30px; padding-left: 0 !important; padding-right: 0 !important; text-align: center; }
.sims-chosen td { background: #172030; }
.sims-name-off { font-weight: 500; }
.sims-state { font-size: 11px; color: #6b7280; }
.sims-state-running { color: #22c55e; }
.sims-state-starting, .sims-state-stopping { color: #f59e0b; }
.sims-state-exited { color: #ef4444; }
.sims-sub { font-size: 11px; color: #6b7280; }
.sims-dirty { color: #f59e0b; }
.sims-all-up { color: #22c55e; }
.sims-phases { display: flex; gap: 2px; margin-top: 4px; width: 180px; height: 5px; }
.sims-phase { position: relative; flex-basis: 0; background: #2b313b; border-radius: 2px; overflow: hidden; }
.sims-phase-past { background: #3b82f6; }
.sims-phase-now { background: #1e3a5f; }
.sims-phase-fill { position: absolute; left: 0; top: 0; bottom: 0; background: #3b82f6; }
.sims-tail-row td { padding-top: 0; }
.sims-tail {
  margin: 0; padding: 6px 8px; max-height: 180px; overflow: auto;
  font: 11px ui-monospace, monospace; color: #c7ccd4; background: #0e1116; border-radius: 3px;
  white-space: pre-wrap; word-break: break-word;
}
.sims-errors { font-size: 12px; color: #fca5a5; }
.sims-more { font-size: 16px; line-height: 1; color: #9ca3af; }
.sims-losses { color: #fbbf24; }
</style>
