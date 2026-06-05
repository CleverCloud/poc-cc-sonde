<script setup>
import { ref, watch } from 'vue'

// Edits a string→value map (e.g. warpscript_file, thresholds, headers) as a
// list of key/value rows. Rebuilds and emits the whole object on every change.
const props = defineProps({
  modelValue: { type: Object, default: () => ({}) },
  valueType: { type: String, default: 'string' }, // 'string' | 'number'
  keyPlaceholder: { type: String, default: 'clé' },
  valuePlaceholder: { type: String, default: 'valeur' },
})
const emit = defineEmits(['update:modelValue'])

const pairs = ref(toPairs(props.modelValue))

function toPairs(obj) {
  return Object.entries(obj || {}).map(([k, v]) => ({ k, v: String(v) }))
}

// Keep local rows in sync when the parent replaces the map wholesale
// (e.g. after a reload), but not while the user is typing here.
watch(
  () => props.modelValue,
  (val) => {
    const current = rebuild()
    if (JSON.stringify(current) !== JSON.stringify(val || {})) {
      pairs.value = toPairs(val)
    }
  }
)

function rebuild() {
  const out = {}
  for (const { k, v } of pairs.value) {
    if (!k) continue
    out[k] = props.valueType === 'number' ? Number(v) : v
  }
  return out
}

function commit() {
  emit('update:modelValue', rebuild())
}

function add() {
  pairs.value.push({ k: '', v: '' })
}
function remove(i) {
  pairs.value.splice(i, 1)
  commit()
}
</script>

<template>
  <div class="map-editor">
    <div v-for="(pair, i) in pairs" :key="i" class="kv-row">
      <input
        type="text"
        v-model="pair.k"
        :placeholder="keyPlaceholder"
        @input="commit"
      />
      <input
        :type="valueType === 'number' ? 'number' : 'text'"
        step="any"
        v-model="pair.v"
        :placeholder="valuePlaceholder"
        @input="commit"
      />
      <button class="small danger" type="button" @click="remove(i)">✕</button>
    </div>
    <button class="small ghost" type="button" @click="add">+ ajouter</button>
  </div>
</template>
