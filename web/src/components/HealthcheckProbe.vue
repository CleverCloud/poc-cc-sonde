<script setup>
import { computed } from 'vue'
import MapEditor from './MapEditor.vue'

// The probe object is shared by reference with the parent; nested fields are
// mutated in place so changes propagate to the parent's `config` model.
const props = defineProps({
  probe: { type: Object, required: true },
})
defineEmits(['remove'])

const useApps = computed({
  get: () => Array.isArray(props.probe.apps) && props.probe.apps.length > 0,
  set: (val) => {
    if (val) {
      props.probe.url = null
      if (!props.probe.apps || props.probe.apps.length === 0) {
        props.probe.apps = [{ id: '', url: '' }]
      }
    } else {
      props.probe.apps = []
      if (props.probe.url == null) props.probe.url = ''
    }
  },
})

function addApp() {
  ;(props.probe.apps ||= []).push({ id: '', url: '' })
}
function removeApp(i) {
  props.probe.apps.splice(i, 1)
}

function ensureHeaders() {
  if (!props.probe.checks.expected_header) props.probe.checks.expected_header = {}
}
</script>

<template>
  <div class="card">
    <div class="card-head">
      <span class="title">{{ probe.name || 'Sonde sans nom' }}</span>
      <button class="small danger" type="button" @click="$emit('remove')">Supprimer</button>
    </div>

    <div class="row">
      <div class="field">
        <label>Nom</label>
        <input type="text" v-model="probe.name" placeholder="API Health Check" />
      </div>
      <div class="field" style="max-width: 180px">
        <label>Intervalle (s)</label>
        <input type="number" min="1" v-model.number="probe.interval_seconds" />
      </div>
    </div>

    <div class="field">
      <label class="checkbox">
        <input type="checkbox" v-model="useApps" />
        Plusieurs applications (sinon URL unique)
      </label>
    </div>

    <div v-if="!useApps" class="field">
      <label>URL</label>
      <input type="text" v-model="probe.url" placeholder="https://exemple.com/health" />
    </div>

    <div v-else class="subpanel">
      <h4>Applications</h4>
      <div v-for="(app, i) in probe.apps" :key="i" class="kv-row">
        <input type="text" v-model="app.id" placeholder="app_id" />
        <input type="text" v-model="app.url" placeholder="https://…/health" />
        <button class="small danger" type="button" @click="removeApp(i)">✕</button>
      </div>
      <button class="small ghost" type="button" @click="addApp">+ application</button>
    </div>

    <div class="subpanel">
      <h4>Vérifications</h4>
      <div class="row">
        <div class="field" style="max-width: 200px">
          <label>Statut HTTP attendu</label>
          <input type="number" v-model.number="probe.checks.expected_status" placeholder="200" />
        </div>
      </div>
      <div class="field">
        <label>Le corps contient</label>
        <input type="text" v-model="probe.checks.expected_body_contains" placeholder="ok" />
      </div>
      <div class="field">
        <label>Regex sur le corps</label>
        <input type="text" v-model="probe.checks.expected_body_regex" placeholder="^\{.*status.*\}$" />
      </div>
      <div class="field">
        <label>En-têtes attendus</label>
        <MapEditor
          v-if="probe.checks.expected_header"
          v-model="probe.checks.expected_header"
          key-placeholder="Header"
          value-placeholder="valeur"
        />
        <button v-else class="small ghost" type="button" @click="ensureHeaders">+ en-têtes</button>
      </div>
    </div>

    <div class="field">
      <label>Commande en cas d'échec (on_failure_command)</label>
      <input type="text" v-model="probe.on_failure_command" placeholder="clever restart --app ${APP_ID}" />
    </div>

    <details class="advanced">
      <summary>Options avancées</summary>
      <div class="row">
        <div class="field">
          <label>Timeout commande (s)</label>
          <input type="number" v-model.number="probe.command_timeout_seconds" />
        </div>
        <div class="field">
          <label>Échecs avant commande</label>
          <input type="number" v-model.number="probe.failure_retries_before_command" />
        </div>
        <div class="field">
          <label>Timeout requête HTTP (s)</label>
          <input type="number" v-model.number="probe.request_timeout_seconds" />
        </div>
      </div>
      <div class="row">
        <div class="field">
          <label>Délai après succès (s)</label>
          <input type="number" v-model.number="probe.delay_after_success_seconds" />
        </div>
        <div class="field">
          <label>Délai après échec (s)</label>
          <input type="number" v-model.number="probe.delay_after_failure_seconds" />
        </div>
      </div>
      <div class="row">
        <div class="field">
          <label>Délai après commande OK (s)</label>
          <input type="number" v-model.number="probe.delay_after_command_success_seconds" />
        </div>
        <div class="field">
          <label>Délai après commande KO (s)</label>
          <input type="number" v-model.number="probe.delay_after_command_failure_seconds" />
        </div>
      </div>
      <label class="checkbox">
        <input type="checkbox" v-model="probe.suppress_command_output" />
        Masquer la sortie des commandes dans les logs
      </label>
    </details>
  </div>
</template>
