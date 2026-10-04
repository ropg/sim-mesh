<template>
  <q-page class="dp">
    <div class="dp-body">
      <div class="dp-head">
        <div class="dp-heading">Installed firmware</div>
        <q-space />
        <q-btn flat dense no-caps label="Add from pre-built…" @click="openPrebuilt" />
        <q-btn unelevated dense no-caps color="primary" label="Add from zip…" @click="adding = true" />
      </div>
      <div class="dp-text">
        What a node can run, for this machine ({{ catalog.arch || '…' }}). A script
        names one by name, or the newest of a base as <code>&lt;base&gt;_latest</code>;
        most scripts ask for theirs above the script. Firmware a paused run or a
        snapshot holds cannot be deleted: its state can only be resumed on it.
      </div>

      <SelectBar :sel="sel">
        <template #default="{ keys }">
          <q-btn flat dense no-caps size="sm" :icon="matDeleteOutline" label="Delete"
                 :disable="!keys.some(free)" @click="removeMany(keys)" />
        </template>
      </SelectBar>
      <table class="dp-table">
        <thead>
          <tr><th class="dp-check"></th><th>Firmware</th><th>Category</th><th>Hardware</th><th>Version</th>
            <th class="num">Size</th><th></th></tr>
        </thead>
        <tbody>
          <tr v-for="f in catalog.firmware" :key="f.name" :class="{ 'dp-off': f.error, 'dp-chosen': sel.has(f.name) }">
            <td class="dp-check">
              <q-checkbox dense size="xs" :model-value="sel.has(f.name)" @update:model-value="sel.toggle(f.name)" />
            </td>
            <td>
              <div class="dp-name mono">{{ f.name }}</div>
              <div v-if="f.error" class="dp-bad">{{ f.error }}</div>
              <div v-else-if="f.title" class="dp-sub">{{ f.title }}</div>
              <div v-if="f.users?.length" class="dp-sub">held by {{ f.users.join(', ') }}</div>
            </td>
            <td class="mono">{{ f.category ?? '—' }}</td>
            <td>
              <div>{{ f.hardware ? `virtual ${f.hardware}` : '—' }}</div>
              <div v-if="f.radio" class="dp-sub">virtual {{ f.radio.toUpperCase() }}</div>
            </td>
            <td class="mono">{{ when(f.version) }}</td>
            <td class="num mono">{{ sizeText(f.bytes) }}</td>
            <td class="dp-act">
              <q-btn flat dense round size="sm" :icon="matDeleteOutline"
                     :disable="!!f.users?.length" @click="removeMany([f.name])">
                <q-tooltip>{{ f.users?.length ? 'Held by a paused run or a snapshot' : 'Delete this firmware' }}</q-tooltip>
              </q-btn>
            </td>
          </tr>
          <tr v-if="!catalog.firmware.length">
            <td colspan="7" class="dp-sub">No firmware installed: add a zip, or a pre-built one.</td>
          </tr>
        </tbody>
      </table>
    </div>

    <q-dialog v-model="adding">
      <q-card style="min-width: 420px">
        <q-card-section class="text-subtitle2">Add firmware from a zip</q-card-section>
        <q-card-section class="column q-gutter-sm">
          <q-file v-model="file" dense outlined label="firmware zip" accept=".zip,application/zip" />
          <div class="text-caption text-grey-6">
            A zip with a node.yaml at its top, built for this machine. It is
            installed under the name its node.yaml gives,
            <code>&lt;base&gt;_&lt;arch&gt;_&lt;version&gt;</code>.
          </div>
        </q-card-section>
        <q-card-actions align="right">
          <q-btn flat no-caps label="Cancel" v-close-popup />
          <q-btn flat no-caps color="primary" label="Add" :loading="busy" :disable="!file" @click="doAdd" />
        </q-card-actions>
      </q-card>
    </q-dialog>

    <q-dialog v-model="prebuiltOpen">
      <q-card style="min-width: 640px; max-width: 900px">
        <q-card-section class="text-subtitle2">Add pre-built firmware</q-card-section>
        <q-card-section>
          <div class="dp-text">From {{ index || 'sim-mesh.net' }}, for this machine.</div>
          <div v-if="prebuiltError" class="dp-bad">{{ prebuiltError }}</div>
          <table v-else class="dp-table">
            <tbody>
              <tr v-for="p in prebuilt" :key="p.name">
                <td>
                  <div class="dp-name mono">{{ p.name }}</div>
                  <div v-if="p.title" class="dp-sub">{{ p.title }}</div>
                </td>
                <td class="mono">{{ p.category ?? '—' }}</td>
                <td class="mono">{{ p.radio ?? '' }}</td>
                <td class="dp-act">
                  <q-btn flat dense no-caps size="sm" :label="p.installed ? 'Installed' : 'Add'"
                         :disable="p.installed" :loading="fetching === p.name" @click="addPrebuilt(p)" />
                </td>
              </tr>
              <tr v-if="!prebuilt.length && !loadingPrebuilt">
                <td class="dp-sub">Nothing pre-built for this machine.</td>
              </tr>
            </tbody>
          </table>
          <q-inner-loading :showing="loadingPrebuilt" />
        </q-card-section>
        <q-card-actions align="right">
          <q-btn flat no-caps label="Close" v-close-popup />
        </q-card-actions>
      </q-card>
    </q-dialog>
  </q-page>
</template>

<script setup lang="ts">
/* The firmware a script can run: installed from a zip, or from what
 * sim-mesh.net builds, and deleted unless something holds it. */
import { onMounted, ref } from 'vue'
import { useQuasar } from 'quasar'
import { matDeleteOutline } from '@quasar/extras/material-icons'
import { useCatalog } from '../stores/catalog'
import { request, upload } from '../lib/front'
import { useSelection } from '../lib/selection'
import { sizeText } from '../lib/size'
import SelectBar from '../components/SelectBar.vue'

interface PrebuiltRow {
  name: string; url: string; title?: string; category?: string; radio?: string
  installed: boolean
}

const catalog = useCatalog()
const quasar = useQuasar()
const sel = useSelection(() => catalog.firmware.map(f => f.name))
const adding = ref(false)
const file = ref<File | null>(null)
const busy = ref(false)
const prebuiltOpen = ref(false)
const prebuilt = ref<PrebuiltRow[]>([])
const prebuiltError = ref<string | null>(null)
const loadingPrebuilt = ref(false)
const fetching = ref<string | null>(null)
const index = ref('')

onMounted(() => { void catalog.refreshFirmware() })

/** A version as it reads: a build stamp as its date and time, a semver as it is. */
function when(version?: string) {
  if (!version || !/^\d{14}$/.test(version)) return version ?? '—'
  return `${version.slice(0, 4)}-${version.slice(4, 6)}-${version.slice(6, 8)} ${version.slice(8, 10)}:${version.slice(10, 12)}`
}

function fail(error?: string) {
  quasar.notify({ type: 'negative', message: error ?? 'refused', timeout: 8000 })
}

/** A firmware nothing holds, which can be deleted. */
function free(name: string) {
  return !catalog.firmware.find(f => f.name === name)?.users?.length
}

/* Those of the names nothing holds, deleted after one confirmation that
 * says which are kept. */
function removeMany(names: string[]) {
  const go = names.filter(free)
  if (!go.length) return
  const kept = names.length - go.length
  quasar.dialog({
    title: go.length === 1 ? 'Delete firmware' : `Delete ${go.length} firmware`,
    message: `Delete ${go.join(', ')}?${kept ? ` ${kept} held by a paused run or a snapshot ${kept === 1 ? 'is' : 'are'} kept.` : ''}`,
    ok: { label: 'Delete', color: 'negative', flat: true, noCaps: true },
    cancel: { flat: true, noCaps: true }, persistent: true,
  }).onOk(() => {
    void (async () => {
      const r = await request('firmware_delete', { names: go })
      if (!r.ok) fail(r.error)
      sel.none()
      await catalog.refreshFirmware()
    })()
  })
}

async function doAdd() {
  if (!file.value) return
  busy.value = true
  const r = await upload('/api/firmware/add', file.value.name, file.value)
  busy.value = false
  if (!r.ok) { fail(r.error); return }
  quasar.notify({ type: 'positive', message: `added ${r.name as string}`, timeout: 3000 })
  adding.value = false
  file.value = null
  await catalog.refreshFirmware()
}

async function openPrebuilt() {
  prebuiltOpen.value = true
  loadingPrebuilt.value = true
  prebuiltError.value = null
  const r = await request('firmware_prebuilt')
  loadingPrebuilt.value = false
  if (!r.ok) { prebuiltError.value = r.error ?? 'refused'; prebuilt.value = []; return }
  prebuilt.value = r.firmware as PrebuiltRow[]
  index.value = r.index as string
}

async function addPrebuilt(p: PrebuiltRow) {
  fetching.value = p.name
  const r = await request('firmware_add', { url: p.url })
  fetching.value = null
  if (!r.ok) { fail(r.error); return }
  p.installed = true
  quasar.notify({ type: 'positive', message: `added ${p.name}`, timeout: 3000 })
  await catalog.refreshFirmware()
}
</script>

<style scoped>
/* Its own height, the page container's, so a long list scrolls within it. */
.dp { overflow-y: auto; height: 100%; }
.dp-body { padding: 16px 20px 32px; max-width: 1100px; }
.dp-head { display: flex; align-items: center; gap: 8px; margin-bottom: 6px; }
.dp-heading { font-size: 14px; font-weight: 500; color: #d1d5db; }
.dp-text { font-size: 12px; color: #9ca3af; line-height: 1.5; margin-bottom: 12px; max-width: 760px; }
.dp-text code { color: #cbd5e1; }
.dp-table { width: 100%; border-collapse: collapse; font-size: 13px; }
.dp-table th {
  text-align: left; font-weight: 500; font-size: 11px; color: #6b7280;
  padding: 4px 10px; border-bottom: 1px solid #262c35; white-space: nowrap;
}
.dp-table td { padding: 8px 10px; border-bottom: 1px solid #1f242c; vertical-align: top; }
.dp-act { text-align: right; width: 1%; white-space: nowrap; }
.dp-table .num { text-align: right; }
.dp-check { width: 28px; padding-left: 2px !important; padding-right: 0 !important; }
.dp-chosen td { background: #172030; }
.dp-name { font-weight: 500; color: #e5e7eb; }
.dp-sub { font-size: 11px; color: #6b7280; }
.dp-bad { font-size: 11px; color: #fca5a5; }
.dp-off td { color: #6b7280; }
.mono { font-family: ui-monospace, monospace; font-size: 12px; }
</style>
