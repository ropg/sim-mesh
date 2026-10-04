<template>
  <q-dialog :model-value="modelValue" @update:model-value="v => emit('update:modelValue', v)">
    <q-card class="nd">
      <q-card-section class="nd-body">
        <div class="nd-head">
          <div class="nd-heading">Installed nodesets</div>
          <q-space />
          <q-btn flat dense round size="sm" icon="close" v-close-popup />
        </div>
        <div class="nd-text">
          Every nodeset here, whichever geodata it stands on; the Layers panel shows
          those with a node on {{ nodes.geodata ?? 'the chosen geodata' }}.
        </div>
        <SelectBar :sel="sel">
          <template #default="{ keys }">
            <q-btn flat dense no-caps size="sm" :icon="matDeleteOutline" label="Delete"
                   :disable="!keys.length" @click="askDelete(keys)" />
          </template>
        </SelectBar>
        <table class="nd-table">
          <thead><tr><th class="nd-check"></th><th>Nodeset</th><th class="num">Nodes</th>
            <th class="num">Size</th><th></th></tr></thead>
          <tbody>
            <tr v-for="n in catalog.nodesets" :key="n.name" :class="{ 'nd-picked': sel.has(n.name) }">
              <td class="nd-check">
                <q-checkbox dense size="xs" :model-value="sel.has(n.name)" @update:model-value="sel.toggle(n.name)" />
              </td>
              <td>
                <span class="nd-name">{{ n.name }}</span>
                <span v-if="n.name === nodes.active" class="nd-sub"> active</span>
                <div v-if="n.from_index" class="nd-sub">
                  from {{ n.from_index.index }}{{ n.from_index.changed ? ', changed here' : '' }}
                </div>
                <div v-if="n.error" class="nd-bad">{{ n.error }}</div>
              </td>
              <td class="num mono">{{ n.nodes ?? '' }}</td>
              <td class="num mono">{{ sizeText(n.bytes) }}</td>
              <td class="nd-act">
                <q-btn flat dense round size="sm" :icon="matDeleteOutline" @click="askDelete([n.name])">
                  <q-tooltip>Delete, with its own setup script</q-tooltip>
                </q-btn>
              </td>
            </tr>
            <tr v-if="!catalog.nodesets.length"><td colspan="5" class="nd-sub">No nodesets yet.</td></tr>
          </tbody>
        </table>

        <IndexOffers kind="nodesets" />
      </q-card-section>
    </q-card>
  </q-dialog>
</template>

<script setup lang="ts">
/* Every nodeset, not only the layers on this geodata: checkboxes, sizes,
 * where one from an index came from, and deleting several at once; below,
 * what the listed indexes offer (IndexOffers). A nodeset from an index
 * brings the geodata it is made for when that is not here. */
import { watch } from 'vue'
import { useQuasar } from 'quasar'
import { matDeleteOutline } from '@quasar/extras/material-icons'
import IndexOffers from './IndexOffers.vue'
import SelectBar from './SelectBar.vue'
import { useCatalog } from '../stores/catalog'
import { useNodes } from '../stores/nodes'
import { useSelection } from '../lib/selection'
import { sizeText } from '../lib/size'
import { whenSaved } from '../lib/unsaved'

const props = defineProps<{ modelValue: boolean }>()
const emit = defineEmits<{ 'update:modelValue': [value: boolean] }>()
const catalog = useCatalog()
const nodes = useNodes()
const quasar = useQuasar()
const sel = useSelection(() => catalog.nodesets.map(n => n.name))

watch(() => props.modelValue, (open) => {
  if (open) { void catalog.refreshNodesets(); void catalog.refreshIndexes() }
})

/* Deleting the active layer leaves the nodeset being edited, so the page
 * asks first about unsaved edits. */
function askDelete(names: string[]) {
  const go = () => quasar.dialog({
    title: names.length === 1 ? `Delete ${names[0]}` : `Delete ${names.length} nodesets`,
    message: `Delete ${names.join(', ')}, each with its own setup script? This cannot be undone.`,
    ok: { label: 'Delete', color: 'negative', flat: true, noCaps: true },
    cancel: { flat: true, noCaps: true }, persistent: true,
  }).onOk(async () => {
    const refused: string[] = []
    for (const n of names) {
      const error = await nodes.deleteNodeset(n)
      if (error) refused.push(error)
    }
    sel.none()
    await catalog.refreshNodesets()
    void catalog.refreshIndexes()
    if (refused.length) quasar.notify({ type: 'negative', message: refused.join('; '), timeout: 8000 })
  })
  if (nodes.active && names.includes(nodes.active)) whenSaved(quasar, go)
  else go()
}
</script>

<style scoped>
.nd { min-width: 720px; max-width: 960px; width: 80vw; max-height: 86vh; }
.nd-body { overflow-y: auto; max-height: 86vh; }
.nd-head { display: flex; align-items: center; gap: 8px; margin-bottom: 6px; }
.nd-heading { font-size: 14px; font-weight: 500; color: #d1d5db; }
.nd-text { font-size: 12px; color: #9ca3af; line-height: 1.5; margin-bottom: 8px; }
.nd-table { width: 100%; border-collapse: collapse; font-size: 13px; }
.nd-table th { text-align: left; font-weight: 500; font-size: 11px; color: #6b7280;
  padding: 4px 10px; border-bottom: 1px solid #262c35; }
.nd-table td { padding: 6px 10px; border-bottom: 1px solid #1f242c; vertical-align: top; }
.nd-table .num { text-align: right; }
.nd-check { width: 28px; padding-left: 2px !important; padding-right: 0 !important; }
.nd-picked td { background: #172030; }
.nd-name { font-weight: 500; }
.nd-act { text-align: right; width: 1%; white-space: nowrap; }
.nd-sub { font-size: 11px; color: #6b7280; }
.nd-bad { font-size: 11px; color: #fca5a5; }
.mono { font-family: ui-monospace, monospace; font-size: 12px; }
</style>
