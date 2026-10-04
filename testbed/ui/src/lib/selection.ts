/* The checkboxes at the start of a list's rows: which rows are chosen, by
 * key, for the actions above the list. A key whose row has gone is dropped,
 * so an action never reaches something no longer listed. */
import { computed, ref, watch, type Ref } from 'vue'

export interface Selection {
  chosen: Ref<Set<string>>
  has: (key: string) => boolean
  toggle: (key: string) => void
  all: () => void
  none: () => void
  invert: () => void
  /** The chosen keys, in the list's order. */
  keys: Ref<string[]>
}

export function useSelection(listed: () => string[]): Selection {
  const chosen = ref(new Set<string>())
  watch(listed, (now) => {
    const here = new Set(now)
    if ([...chosen.value].some(k => !here.has(k))) {
      chosen.value = new Set([...chosen.value].filter(k => here.has(k)))
    }
  })
  return {
    chosen,
    has: (key) => chosen.value.has(key),
    toggle: (key) => {
      const next = new Set(chosen.value)
      if (!next.delete(key)) next.add(key)
      chosen.value = next
    },
    all: () => { chosen.value = new Set(listed()) },
    none: () => { chosen.value = new Set() },
    invert: () => { chosen.value = new Set(listed().filter(k => !chosen.value.has(k))) },
    keys: computed(() => listed().filter(k => chosen.value.has(k))),
  }
}
