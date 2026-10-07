<template>
  <div class="io">
    <div class="tab-head">
      <div class="tab-heading">Download pre-built {{ kind === 'geodata' ? 'geodata packs' : 'nodesets' }}</div>
      <q-space />
      <q-btn flat dense no-caps label="Refresh" :loading="catalog.indexesLoading"
             @click="catalog.refreshIndexes()" />
      <q-btn flat dense no-caps label="Add index" @click="adding = true" />
    </div>
    <div v-if="catalog.indexes === null" class="io-sub">Asking the indexes…</div>
    <div v-for="ix in catalog.indexes ?? []" :key="ix.name" class="io-index">
      <div class="io-index-head">
        <span class="io-index-name">{{ ix.name }}</span>
        <span class="io-sub">{{ ix.title }}</span>
        <q-space />
        <span class="io-sub io-address" :title="ix.address">{{ ix.address }}</span>
        <q-btn v-if="!ix.own" flat dense round size="sm" :icon="matDeleteOutline" color="grey-6"
               aria-label="Forget this index" @click="askForget(ix.name)">
          <q-tooltip>Forget this index; what came from it stays installed</q-tooltip>
        </q-btn>
        <span v-else class="io-icon-gap" />
      </div>
      <div v-if="ix.description" class="io-about">
        <p v-for="(para, i) in paragraphs(ix.description)" :key="i">{{ para }}</p>
      </div>
      <div v-if="ix.error" class="io-bad">{{ ix.error }}</div>
      <table v-else-if="ix[kind].length" class="io-table tab-flow">
        <tbody>
          <tr v-for="e in ix[kind]" :key="e.name" :class="{ 'io-row': !e.taken && !fetchingOf(ix.name, e.name) }"
              :title="rowTitle(ix.name, e)" @click="clicked(ix.name, e)">
            <td class="io-name-cell">
              <span class="io-name">{{ e.name }}</span>
              <div v-if="e.title" class="io-sub">{{ keepUnits(e.title) }}</div>
            </td>
            <td>
              <div v-if="e.description" class="io-text">{{ keepUnits(e.description) }}</div>
              <div class="io-sub">{{ facts(e) }}</div>
            </td>
            <td class="col-size mono">{{ sizeText(e.bytes) }}</td>
            <td class="col-act" @click.stop>
              <template v-if="fetchingOf(ix.name, e.name)">
                <div class="io-sub">{{ progressText(fetchingOf(ix.name, e.name)!) }}</div>
                <q-btn flat dense no-caps size="sm" label="Cancel" @click="cancel(ix.name, e.name)" />
              </template>
              <span v-else-if="e.installed" class="io-ok">installed{{ e.changed ? ', changed here' : '' }}</span>
              <span v-else-if="e.taken" class="io-warn">name taken here</span>
            </td>
          </tr>
        </tbody>
      </table>
      <div v-else class="io-sub io-none">Offers no {{ kind === 'geodata' ? 'geodata' : 'nodesets' }}.</div>
    </div>

    <q-dialog v-model="adding">
      <q-card style="min-width: 460px">
        <q-card-section class="text-subtitle2">Add an index</q-card-section>
        <q-card-section class="column q-gutter-sm">
          <q-input v-model="address" dense outlined label="address"
                   placeholder="https://example.org/mesh/index.yaml" autofocus />
          <div class="text-caption text-grey-6">
            The address of an index.yaml: a web address, or a path on this machine (a
            directory means the index.yaml in it). It lists geodata and nodesets, each
            by its sha256; it is read now and listed under the name it gives, on the
            Geodata tab and among the nodesets alike.
          </div>
        </q-card-section>
        <q-card-actions align="right">
          <q-btn flat no-caps label="Cancel" v-close-popup />
          <q-btn flat no-caps color="primary" label="Add" :loading="busy" :disable="!address.trim()" @click="addIndex" />
        </q-card-actions>
      </q-card>
    </q-dialog>
  </div>
</template>

<script setup lang="ts">
/* What the listed indexes offer of one kind, geodata or nodesets: each
 * index with its entries, each entry installed here (a click opens it), its
 * name taken by something else, being fetched (with Cancel), or not here (a
 * click installs it).
 * sim-mesh's own index is always listed; one a person added has a trash can
 * that forgets it. The indexes are one list for both kinds. */
import { onMounted, ref } from 'vue'
import { useQuasar } from 'quasar'
import { matDeleteOutline } from '@quasar/extras/material-icons'
import { fetchKey, useCatalog, type Fetching, type IndexEntry, type IndexKind } from '../stores/catalog'
import { request } from '../lib/front'
import { keepUnits, sizeText } from '../lib/size'

const props = defineProps<{ kind: IndexKind }>()
/** A click on an installed entry: its name, for the page to open. */
const emit = defineEmits<{ open: [name: string] }>()
const catalog = useCatalog()
const quasar = useQuasar()
const adding = ref(false)
const address = ref('')
const busy = ref(false)

onMounted(() => { if (catalog.indexes === null) void catalog.refreshIndexes() })

function tell(error: string | null | undefined, done?: string) {
  if (error) quasar.notify({ type: 'negative', message: error, timeout: 8000 })
  else if (done) quasar.notify({ type: 'positive', message: done, timeout: 4000 })
}

/** An index's description as paragraphs, each reflowed to the page's width:
 *  a blank line parts two, a single line break is a space. */
function paragraphs(text: string) {
  return text.split(/\n\s*\n/).map(p => keepUnits(p.replace(/\s*\n\s*/g, ' ').trim())).filter(Boolean)
}

function facts(e: IndexEntry) {
  const out: string[] = []
  if (e.geodata) out.push(`for ${e.geodata}`)
  if (e.nodes !== undefined) out.push(`${e.nodes} nodes`)
  if (e.bbox) {
    const [lon0, lat0, lon1, lat1] = e.bbox
    out.push(`${lat0.toFixed(3)}…${lat1.toFixed(3)} N, ${lon0.toFixed(3)}…${lon1.toFixed(3)} E`)
  }
  if (e.tags?.length) out.push(e.tags.join(', '))
  if (e.licences) out.push(e.licences)
  return out.join(' · ')
}

function fetchingOf(index: string, name: string): Fetching | undefined {
  return catalog.fetching[fetchKey(index, props.kind, name)]
}

function progressText(f: Fetching) {
  const first = f.now && (f.now.kind !== f.kind || f.now.name !== f.name) ? `${f.now.name} first: ` : ''
  if (!f.fetched) return `${first}fetching…`
  return `${first}${sizeText(f.fetched)}${f.of ? ` of ${sizeText(f.of)}` : ''}`
}

function rowTitle(index: string, e: IndexEntry) {
  if (fetchingOf(index, e.name)) return ''
  if (e.installed) return 'Already installed, click to open'
  if (e.taken) return 'Something else here has this name: rename or delete it to install this one'
  return 'Click to install'
}

/** A click on a row: what is installed opened, what is not installed. */
function clicked(index: string, e: IndexEntry) {
  if (fetchingOf(index, e.name) || e.taken) return
  if (e.installed) emit('open', e.name)
  else void install(index, e)
}

async function install(index: string, e: IndexEntry) {
  const r = await request('index_install', { index, kind: props.kind, name: e.name })
  if (!r.ok && !String(r.error).startsWith('cancelled')) tell(r.error)
  else if (r.ok) {
    const all = (r.installed as { kind: string; name: string }[]).map(i => i.name).join(' and ')
    tell(null, `added ${all}`)
  }
}

async function cancel(index: string, name: string) {
  const r = await request('index_cancel', { index, kind: props.kind, name })
  if (!r.ok) tell(r.error)
}

async function addIndex() {
  busy.value = true
  const r = await request('index_new', { address: address.value.trim() })
  busy.value = false
  if (!r.ok) { tell(r.error); return }
  adding.value = false
  address.value = ''
  tell(null, `listed ${(r.index as { name: string }).name}`)
  await catalog.refreshIndexes()
}

function askForget(name: string) {
  quasar.dialog({
    title: `Forget ${name}`,
    message: 'It is no longer listed. Geodata and nodesets that came from it stay installed.',
    ok: { label: 'Forget', color: 'negative', flat: true, noCaps: true },
    cancel: { flat: true, noCaps: true }, persistent: true,
  }).onOk(async () => {
    const r = await request('index_delete', { name })
    if (!r.ok) { tell(r.error); return }
    await catalog.refreshIndexes()
  })
}
</script>

<style scoped>
.io { margin-top: 36px; }
.io-index { margin: 8px 0 14px; }
.io-index-head { display: flex; align-items: center; gap: 10px; padding: 4px 10px;
  border-bottom: 1px solid #262c35; }
.io-index-name { font-weight: 500; color: #e5e7eb; font-size: 13px; }
.io-address { max-width: 360px; overflow: hidden; text-overflow: ellipsis; white-space: nowrap;
  font-family: ui-monospace, monospace; }
.io-icon-gap { width: 30px; }
.io-table { width: 100%; border-collapse: collapse; font-size: 13px; }
.io-table td { padding: 6px 10px 6px 24px; border-bottom: 1px solid #1f242c; vertical-align: top; }
.io-table td + td { padding-left: 10px; }
.io-name-cell { width: 28%; }
.io-name { font-weight: 500; }
.io-row { cursor: pointer; }
.col-act .io-sub { white-space: normal; }
.io-row:hover td { background: #1b2028; }
.io-row .io-name { color: var(--q-primary); }
.io-text { font-size: 12px; color: #9ca3af; }
/* The width of the page's own paragraphs, from the same left edge. */
.io-about { font-size: 12px; color: #9ca3af; line-height: 1.5; padding: 8px 0 4px; max-width: 760px; }
.io-about p { margin: 0 0 6px; }
@media (max-width: 640px) {
  .io-index-head { flex-wrap: wrap; }
  .io-address { max-width: 100%; }
}
.io-sub { font-size: 11px; color: #6b7280; }
.io-none { padding: 6px 24px; }
.io-bad { font-size: 11px; color: #fca5a5; padding: 6px 24px; }
.io-ok { font-size: 11px; color: #22c55e; }
.io-warn { font-size: 11px; color: #f59e0b; }
.num { text-align: right; }
.mono { font-family: ui-monospace, monospace; font-size: 12px; }
</style>
