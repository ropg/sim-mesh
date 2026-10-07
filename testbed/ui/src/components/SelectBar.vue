<template>
  <div class="sb">
    <q-btn flat dense no-caps size="sm" label="All" @click="sel.all()" />
    <q-btn flat dense no-caps size="sm" label="None" @click="sel.none()" />
    <q-btn flat dense no-caps size="sm" label="Invert" @click="sel.invert()" />
    <span class="sb-count">{{ sel.keys.value.length ? `${sel.keys.value.length} chosen` : '' }}</span>
    <slot v-if="sel.keys.value.length" :keys="sel.keys.value" />
  </div>
</template>

<script setup lang="ts">
/* Above a list with checkboxes: All, None and Invert, how many are chosen,
 * and, while any are, the list's own actions on them, in the slot. */
import type { Selection } from '../lib/selection'

defineProps<{ sel: Selection }>()
</script>

<style scoped>
.sb { display: flex; align-items: center; flex-wrap: wrap; gap: 4px 14px; margin: 0 0 4px; min-height: 28px; }
.sb-count { font-size: 11px; color: #6b7280; min-width: 60px; }
</style>
