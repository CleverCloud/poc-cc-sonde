<script setup>
import { ref, reactive, onMounted } from 'vue'
import HealthcheckProbe from './components/HealthcheckProbe.vue'
import WarpscriptProbe from './components/WarpscriptProbe.vue'
import AgentProbe from './components/AgentProbe.vue'

const config = reactive({ healthcheck_probes: [], warpscript_probes: [], agent_probes: [] })
const status = reactive({ text: 'Chargement…', kind: 'info' })
const loading = ref(true)
const saving = ref(false)
const dirty = ref(false)
const mode = ref('form') // 'form' | 'raw'
const rawText = ref('')

function setStatus(text, kind = 'info') {
  status.text = text
  status.kind = kind
}

async function load() {
  loading.value = true
  setStatus('Chargement…', 'info')
  try {
    const res = await fetch('/api/config', { credentials: 'same-origin' })
    if (res.status === 404) {
      // Aucune configuration stockée : on part d'une base vide.
      config.healthcheck_probes = []
      config.warpscript_probes = []
      config.agent_probes = []
      setStatus('Aucune configuration stockée — partez de zéro.', 'info')
    } else if (!res.ok) {
      setStatus(`Erreur de chargement : ${res.status}`, 'err')
    } else {
      const data = await res.json()
      config.healthcheck_probes = data.healthcheck_probes || []
      config.warpscript_probes = data.warpscript_probes || []
      config.agent_probes = data.agent_probes || []
      setStatus('Configuration chargée.', 'ok')
    }
  } catch (e) {
    setStatus(`Erreur réseau : ${e}`, 'err')
  } finally {
    loading.value = false
    dirty.value = false
  }
}

// Supprime récursivement les scalaires vides (null, "", NaN) pour laisser le
// backend appliquer ses valeurs par défaut. Conserve objets et tableaux.
function clean(v) {
  if (Array.isArray(v)) return v.map(clean)
  if (v && typeof v === 'object') {
    const out = {}
    for (const [k, val] of Object.entries(v)) {
      const c = clean(val)
      if (c === undefined) continue
      out[k] = c
    }
    return out
  }
  if (v === null || v === '' || (typeof v === 'number' && Number.isNaN(v))) return undefined
  return v
}

function buildPayload() {
  const cleaned = clean(JSON.parse(JSON.stringify(config)))
  // Un en-tête vide ne doit pas compter comme une vérification configurée.
  for (const p of cleaned.healthcheck_probes || []) {
    if (p.checks && p.checks.expected_header && Object.keys(p.checks.expected_header).length === 0) {
      delete p.checks.expected_header
    }
  }
  return cleaned
}

async function save() {
  saving.value = true
  let payload
  if (mode.value === 'raw') {
    try {
      payload = JSON.parse(rawText.value)
    } catch (e) {
      setStatus(`JSON invalide : ${e}`, 'err')
      saving.value = false
      return
    }
  } else {
    payload = buildPayload()
  }

  setStatus('Enregistrement…', 'info')
  try {
    const res = await fetch('/api/config', {
      method: 'PUT',
      credentials: 'same-origin',
      headers: { 'Content-Type': 'application/json' },
      body: JSON.stringify(payload),
    })
    const body = await res.text()
    if (res.ok) {
      setStatus('Configuration enregistrée et sondes rechargées ✓', 'ok')
      dirty.value = false
      await load()
    } else {
      setStatus(body || `Erreur ${res.status}`, 'err')
    }
  } catch (e) {
    setStatus(`Erreur réseau : ${e}`, 'err')
  } finally {
    saving.value = false
  }
}

function addHealthcheck() {
  config.healthcheck_probes.push({
    name: 'Nouvelle sonde',
    interval_seconds: 60,
    url: '',
    apps: [],
    checks: {
      expected_status: 200,
      expected_body_contains: '',
      expected_body_regex: '',
      expected_header: null,
    },
    on_failure_command: '',
    command_timeout_seconds: 30,
    delay_after_success_seconds: null,
    delay_after_failure_seconds: null,
    delay_after_command_success_seconds: null,
    delay_after_command_failure_seconds: null,
    failure_retries_before_command: null,
    request_timeout_seconds: null,
    suppress_command_output: false,
  })
  dirty.value = true
}

function addWarpscript() {
  config.warpscript_probes.push({
    name: 'Nouvelle sonde WarpScript',
    interval_seconds: 60,
    warpscript_file: { cpu: 'warpscript/cpu_usage.mc2' },
    command_timeout_seconds: 30,
    request_timeout_seconds: null,
    on_failure_command: '',
    failure_retries_before_command: null,
    delay_after_command_success_seconds: null,
    delay_after_command_failure_seconds: null,
    suppress_command_output: false,
    apps: [],
    scaling: {
      instances: { min: 1, max: 3 },
      flavors: ['S', 'M'],
      scale_up_threshold: { cpu: 70 },
      scale_down_threshold: { cpu: 30 },
      upscale_command: '',
      downscale_command: '',
      delay_after_scale_seconds: null,
      delay_after_upscale: null,
      delay_after_downscale: null,
    },
  })
  dirty.value = true
}

function addAgent() {
  config.agent_probes.push({
    name: 'Nouvelle sonde agent',
    interval_seconds: 30,
    window_seconds: 120,
    command_timeout_seconds: 30,
    on_failure_command: '',
    failure_retries_before_command: null,
    delay_after_command_success_seconds: null,
    delay_after_command_failure_seconds: null,
    suppress_command_output: false,
    apps: [{ id: '' }],
    scaling: {
      instances: { min: 1, max: 3 },
      flavors: ['S', 'M'],
      scale_up_threshold: { cpu: 70, memory: 80 },
      scale_down_threshold: { cpu: 30, memory: 30 },
      upscale_command: '',
      downscale_command: '',
      delay_after_scale_seconds: null,
      delay_after_upscale: null,
      delay_after_downscale: null,
    },
  })
  dirty.value = true
}

function removeHealthcheck(i) {
  config.healthcheck_probes.splice(i, 1)
  dirty.value = true
}
function removeWarpscript(i) {
  config.warpscript_probes.splice(i, 1)
  dirty.value = true
}
function removeAgent(i) {
  config.agent_probes.splice(i, 1)
  dirty.value = true
}

function switchMode(next) {
  if (next === 'raw') {
    rawText.value = JSON.stringify(buildPayload(), null, 2)
  } else {
    // En quittant le mode brut, on tente de réinjecter le JSON dans le formulaire.
    try {
      const parsed = JSON.parse(rawText.value)
      config.healthcheck_probes = parsed.healthcheck_probes || []
      config.warpscript_probes = parsed.warpscript_probes || []
      config.agent_probes = parsed.agent_probes || []
    } catch (e) {
      setStatus(`JSON invalide, retour au formulaire impossible : ${e}`, 'err')
      return
    }
  }
  mode.value = next
}

onMounted(load)
</script>

<template>
  <div class="app">
    <header class="topbar">
      <h1>POC Sonde · Configuration</h1>
      <div class="actions">
        <span class="status" :class="status.kind">{{ status.text }}</span>
        <button class="ghost small" type="button" @click="load" :disabled="saving">Recharger</button>
        <button class="primary" type="button" @click="save" :disabled="saving || loading">
          {{ saving ? 'Enregistrement…' : 'Enregistrer & appliquer' }}
        </button>
      </div>
    </header>

    <div class="toggle-bar">
      <button class="small" :class="{ active: mode === 'form' }" @click="switchMode('form')">Formulaire</button>
      <button class="small" :class="{ active: mode === 'raw' }" @click="switchMode('raw')">JSON brut</button>
    </div>

    <p class="banner" v-if="dirty">Modifications non enregistrées. Cliquez sur « Enregistrer & appliquer » pour recharger les sondes.</p>

    <template v-if="mode === 'form'">
      <section class="group">
        <h2>
          Sondes HTTP <span class="tag">{{ config.healthcheck_probes.length }}</span>
          <button class="small ghost" type="button" @click="addHealthcheck">+ sonde HTTP</button>
        </h2>
        <p class="empty" v-if="config.healthcheck_probes.length === 0">Aucune sonde HTTP.</p>
        <HealthcheckProbe
          v-for="(probe, i) in config.healthcheck_probes"
          :key="i"
          :probe="probe"
          @remove="removeHealthcheck(i)"
        />
      </section>

      <section class="group">
        <h2>
          Sondes WarpScript <span class="tag">{{ config.warpscript_probes.length }}</span>
          <button class="small ghost" type="button" @click="addWarpscript">+ sonde WarpScript</button>
        </h2>
        <p class="empty" v-if="config.warpscript_probes.length === 0">Aucune sonde WarpScript.</p>
        <WarpscriptProbe
          v-for="(probe, i) in config.warpscript_probes"
          :key="i"
          :probe="probe"
          @remove="removeWarpscript(i)"
        />
      </section>

      <section class="group">
        <h2>
          Sondes Agent <span class="tag">{{ config.agent_probes.length }}</span>
          <button class="small ghost" type="button" @click="addAgent">+ sonde Agent</button>
        </h2>
        <p class="empty" v-if="config.agent_probes.length === 0">Aucune sonde agent.</p>
        <AgentProbe
          v-for="(probe, i) in config.agent_probes"
          :key="i"
          :probe="probe"
          @remove="removeAgent(i)"
        />
      </section>
    </template>

    <section class="group raw-editor" v-else>
      <h2>JSON brut</h2>
      <textarea v-model="rawText" spellcheck="false"></textarea>
    </section>
  </div>
</template>
