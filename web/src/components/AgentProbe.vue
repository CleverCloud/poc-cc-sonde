<script setup>
import { computed } from 'vue'
import MapEditor from './MapEditor.vue'

const props = defineProps({
  probe: { type: Object, required: true },
})
defineEmits(['remove'])

const stateful = computed({
  get: () => props.probe.scaling.instances != null,
  set: (val) => {
    if (val) {
      props.probe.scaling.instances = { min: 1, max: 3 }
      if (!props.probe.scaling.flavors || props.probe.scaling.flavors.length === 0) {
        props.probe.scaling.flavors = ['S']
      }
    } else {
      props.probe.scaling.instances = null
      props.probe.scaling.flavors = []
    }
  },
})

const flavorsText = computed({
  get: () => (props.probe.scaling.flavors || []).join(', '),
  set: (val) => {
    props.probe.scaling.flavors = val.split(',').map((s) => s.trim()).filter(Boolean)
  },
})

function addApp() {
  ;(props.probe.apps ||= []).push({ id: '' })
}
function removeApp(i) {
  props.probe.apps.splice(i, 1)
}
</script>

<template>
  <div class="card">
    <div class="card-head">
      <span class="title">{{ probe.name || 'Sonde agent sans nom' }}</span>
      <button class="small danger" type="button" @click="$emit('remove')">Supprimer</button>
    </div>

    <div class="row">
      <div class="field">
        <label>Nom</label>
        <input type="text" v-model="probe.name" placeholder="Agent Scaler" />
      </div>
      <div class="field" style="max-width: 160px">
        <label>Intervalle (s)</label>
        <input type="number" min="1" v-model.number="probe.interval_seconds" />
      </div>
      <div class="field" style="max-width: 200px">
        <label>Fenêtre de moyenne (s)</label>
        <input type="number" min="1" v-model.number="probe.window_seconds" />
      </div>
    </div>

    <div class="subpanel">
      <h4>Applications (l'endpoint d'ingestion est /api/metrics/&lt;id&gt;)</h4>
      <div v-for="(app, i) in probe.apps" :key="i" class="kv-row">
        <input type="text" v-model="app.id" placeholder="app_id" />
        <button class="small danger" type="button" @click="removeApp(i)">✕</button>
      </div>
      <button class="small ghost" type="button" @click="addApp">+ application</button>
    </div>

    <div class="subpanel">
      <h4>Scaling</h4>
      <label class="checkbox" style="margin-bottom: 10px">
        <input type="checkbox" v-model="stateful" />
        Scaling avec niveaux (instances + flavors) — sinon mode stateless (alerte)
      </label>

      <template v-if="stateful">
        <div class="row">
          <div class="field" style="max-width: 140px">
            <label>Instances min</label>
            <input type="number" min="1" v-model.number="probe.scaling.instances.min" />
          </div>
          <div class="field" style="max-width: 140px">
            <label>Instances max</label>
            <input type="number" min="1" v-model.number="probe.scaling.instances.max" />
          </div>
          <div class="field">
            <label>Flavors (séparés par des virgules)</label>
            <input type="text" v-model="flavorsText" placeholder="XS, S, M, L" />
          </div>
        </div>
      </template>

      <div class="row">
        <div class="field">
          <label>Seuils scale-up (métrique → valeur, ANY)</label>
          <MapEditor v-model="probe.scaling.scale_up_threshold" value-type="number" key-placeholder="cpu" value-placeholder="70" />
        </div>
        <div class="field">
          <label>Seuils scale-down (métrique → valeur, ALL)</label>
          <MapEditor v-model="probe.scaling.scale_down_threshold" value-type="number" key-placeholder="cpu" value-placeholder="30" />
        </div>
      </div>

      <div class="field">
        <label>Commande upscale</label>
        <input type="text" v-model="probe.scaling.upscale_command" placeholder="clever scale --app ${APP_ID} --flavor ${FLAVOR} --instances ${INSTANCES}" />
      </div>
      <div class="field">
        <label>Commande downscale</label>
        <input type="text" v-model="probe.scaling.downscale_command" placeholder="clever scale --app ${APP_ID} --flavor ${FLAVOR} --instances ${INSTANCES}" />
      </div>
    </div>

    <details class="advanced">
      <summary>Options avancées (commande d'échec)</summary>
      <div class="field">
        <label>Commande en cas d'échec sonde (on_failure_command)</label>
        <input type="text" v-model="probe.on_failure_command" placeholder="clever restart --app ${APP_ID} --quiet" />
      </div>
      <div class="row">
        <div class="field">
          <label>Timeout commande (s)</label>
          <input type="number" v-model.number="probe.command_timeout_seconds" />
        </div>
        <div class="field">
          <label>Échecs avant commande</label>
          <input type="number" v-model.number="probe.failure_retries_before_command" />
        </div>
      </div>
      <label class="checkbox">
        <input type="checkbox" v-model="probe.suppress_command_output" />
        Masquer la sortie des commandes dans les logs
      </label>
    </details>
  </div>
</template>
