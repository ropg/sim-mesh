<template>
  <div class="sb">
    <q-btn flat dense no-caps size="sm" label="All" @click="sel.all()" />
    <q-btn flat dense no-caps size="sm" label="None" @click="sel.none()" />
    <q-btn flat dense no-caps size="sm" label="Invert" @click="sel.invert()" />
    <span class="sb-count">{{ sel.keys.value.length ? `${sel.keys.value.length} chosen` : '' }}</span>
    <span class="sb-gap" />
    <slot :keys="sel.keys.value" />
  </div>
</template>

<script setup lang="ts">
/* Above a list with checkboxes: All, None and Invert, how many are chosen,
 * and the list's own actions on them, in the slot. */
import type { Selection } from '../lib/selection'

defineProps<{ sel: Selection }>()
</script>

<style scoped>
.sb { display: flex; align-items: center; gap: 2px; margin: 0 0 4px; min-height: 28px; }
.sb-count { font-size: 11px; color: #6b7280; padding: 0 8px; min-width: 70px; }
.sb-gap { width: 12px; }
</style>
