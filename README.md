# cc-sonde

Application Rust de monitoring HTTP et d'auto-scaling pilote par des metriques Warp 10. Concue pour tourner sur **Clever Cloud** mais utilisable partout.

## Fonctionnalites

- **Healthcheck probes** — surveillance periodique d'endpoints HTTP (status, body, regex, headers) avec execution d'une commande en cas d'echec repete
- **WarpScript probes** — requetes Warp 10 avec auto-scaling level-based (flavors + instances) ou stateless (webhook/alerte a chaque depassement de seuil)
- **Agent probes** — meme logique de scaling, mais pilotee par les metriques CPU/RAM poussees par un agent local installe sur les VM (moyenne glissante sur une fenetre configurable)
- **Multi-metric** — scale UP si un seuil est depasse (OR), scale DOWN si tous sont en dessous (AND)
- **Multi-instance** — verrou distribue Redis pour eviter les actions en doublon entre replicas
- **Dry run** — validation de configuration sans effets de bord (`--dry-run`)
- **Persistance** — in-memory (defaut) ou Redis (avec `--features redis-persistence`)
- **UI web de configuration** — page unique VueJS protegee par HTTP Basic Auth pour editer la configuration ; stockage dans Redis et **hot-reload** des sondes sans redemarrage

## Interface web de configuration

Une SPA VueJS (une seule page) permet d'editer les sondes depuis le navigateur.

- **Activation** : definir `CONFIG_UI_USER` et `CONFIG_UI_PASSWORD` (les deux non vides). Sans cela, seul `/healthz` est expose.
- **Hebergement** : servie par le serveur de health check (`--healthcheck`, port `--healthcheck-port`, defaut `8080`).
- **Authentification** : HTTP Basic Auth (prompt natif du navigateur).
- **Source de verite** : au demarrage la config est lue depuis Redis si presente, sinon **bootstrap depuis le fichier TOML** puis persistee. Chaque sauvegarde via l'UI ecrit dans Redis et recharge les sondes a chaud.
- **Endpoints** : `GET /` (SPA), `GET/PUT /api/config` (JSON, auth requise), `GET /healthz` (liveness, sans auth).

Le front se reconstruit avec :

```bash
cd web && npm install && npm run build   # genere web/dist, embarque dans le binaire
```

> Note : `web/dist` est embarque dans le binaire au moment de `cargo build`. Lancez le build du front avant le build Rust si vous modifiez l'UI.

## Quickstart

```bash
# Build
cargo build --release
# ou avec Redis
cargo build --release --features redis-persistence

# Lancer
./target/release/cc-sonde --config config.toml

# Avec le endpoint de liveness
./target/release/cc-sonde --config config.toml --healthcheck
```

Voir [`INSTALL.md`](INSTALL.md) pour la documentation complete (configuration, variables d'environnement, WarpScript, troubleshooting, etc.).

## Deploiement sur Clever Cloud

### Pre-requis

- Une application **Docker** ou **Rust** sur Clever Cloud
- (Optionnel) Un add-on **Redis** si vous utilisez la persistance Redis ou le mode multi-instance

### Variables d'environnement

Configurez ces variables dans le panneau de l'application Clever Cloud :

| Variable | Obligatoire | Description |
|----------|-------------|-------------|
| `WARP_ENDPOINT` | si WarpScript probes | URL de l'API exec Warp 10 |
| `WARP_TOKEN` | non | Token de lecture Warp 10 (fallback global) |
| `REDIS_URL` | non | URL Redis (fournie automatiquement par l'add-on Redis) |
| `MULTI_INSTANCE` | non | `true` pour le mode multi-instance (requiert Redis) |
| `CONFIG_UI_USER` | non | Identifiant Basic Auth de l'UI web (active l'UI avec `CONFIG_UI_PASSWORD`) |
| `CONFIG_UI_PASSWORD` | non | Mot de passe Basic Auth de l'UI web |
| `AGENT_INGEST_TOKEN` | non | Secret partage exige (header `X-Agent-Token`) pour pousser des metriques d'agent |
| `RUST_LOG` | non | Niveau de log (`info` par defaut) |
| `CC_RUN_COMMAND` | oui | Commande de lancement (voir ci-dessous) |

### Commande de lancement

Dans `CC_RUN_COMMAND` (ou dans le fichier de run de votre application) :

```bash
./target/release/cc-sonde --config config.toml --healthcheck --healthcheck-port 8080
```

Le port `8080` est le port par defaut expose par Clever Cloud. Le endpoint `/healthz` repond `200 OK` et sert de health check pour la plateforme (configurez-le comme cible du health check Clever Cloud). Lorsque l'UI est activee, `/` sert la page de configuration ; sinon `/` repond aussi `200 OK`.

Pour le mode multi-instance avec Redis :

```bash
./target/release/cc-sonde --config config.toml --healthcheck --multi-instance
```

### Arret gracieux

Clever Cloud envoie un `SIGTERM` avant de stopper une instance. cc-sonde intercepte ce signal et termine proprement les taches en cours. Le timeout d'arret est configurable via `--shutdown-timeout` (defaut : 2s).

### Add-on Redis

Si vous ajoutez un add-on Redis a votre application, Clever Cloud injecte automatiquement `REDIS_URL` dans l'environnement. cc-sonde l'utilise directement — aucune configuration supplementaire n'est necessaire.

Pour activer la persistance Redis, le binaire doit etre compile avec `--features redis-persistence`.

### Exemple de configuration pour Clever Cloud

```toml
# Healthcheck d'une app Clever Cloud
[[healthcheck_probes]]
name = "Mon App"
interval_seconds = 60
on_failure_command = "clever restart --app ${APP_ID}"
failure_retries_before_command = 2

[healthcheck_probes.checks]
expected_status = 200

[[healthcheck_probes.apps]]
id = "app_xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"
url = "https://mon-app.cleverapps.io/health"
```

```toml
# Auto-scaling WarpScript
[[warpscript_probes]]
name = "CPU Scaler"
warpscript_file = {cpu = "warpscript/cpu.mc2"}
interval_seconds = 60

[[warpscript_probes.apps]]
id = "app_xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"

[warpscript_probes.scaling]
instances = {min = 1, max = 3}
flavors = ["S", "M", "L"]
scale_up_threshold = {cpu = 70.0}
scale_down_threshold = {cpu = 40.0}
upscale_command = "clever scale --app ${APP_ID} --flavor ${FLAVOR} --instances ${INSTANCES}"
downscale_command = "clever scale --app ${APP_ID} --flavor ${FLAVOR} --instances ${INSTANCES}"
```

## Agent local (sondes `agent_probes`)

Une sonde `agent_probes` se comporte exactement comme une sonde WarpScript (memes niveaux, cooldowns, commandes up/down, verrou multi-instance), mais les valeurs ne proviennent pas de Warp 10 : elles sont **poussees par un agent local** tournant sur chaque VM monitoree.

- L'agent envoie periodiquement CPU et RAM vers `POST /api/metrics/<app_id>` (l'app_id est le suffixe d'URL ; le corps JSON contient l'instance_id et les mesures).
- Le serveur conserve les echantillons pendant `window_seconds` et en calcule la **moyenne** (toutes instances confondues) pour decider du scaling.
- L'endpoint d'ingestion n'existe que pour les app_id ayant une sonde agent declaree (sinon `404`). Il est accessible sans l'auth de l'UI ; on peut le proteger via `AGENT_INGEST_TOKEN` (header `X-Agent-Token`).

```toml
[[agent_probes]]
name = "Agent Scaler"
interval_seconds = 30        # frequence d'evaluation du scaling
window_seconds = 120         # fenetre de retention/moyenne des mesures

[[agent_probes.apps]]
id = "app_xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx"

[agent_probes.scaling]
instances = {min = 1, max = 3}
flavors = ["S", "M", "L"]
scale_up_threshold = {cpu = 70.0, memory = 80.0}
scale_down_threshold = {cpu = 30.0, memory = 40.0}
upscale_command = "clever scale --app ${APP_ID} --flavor ${FLAVOR} --instances ${INSTANCES}"
downscale_command = "clever scale --app ${APP_ID} --flavor ${FLAVOR} --instances ${INSTANCES}"
```

### Lancer l'agent sur une VM

L'agent est un second binaire du projet (`cargo build --release --bin agent`). Il echantillonne CPU/RAM via `sysinfo` et poste a la frequence indiquee :

```bash
agent \
  --endpoint http://monitor.example.com:8080/api/metrics \
  --app-id app_xxxxxxxx-xxxx-xxxx-xxxx-xxxxxxxxxxxx \
  --instance-id "$INSTANCE_ID" \
  --interval 15
  # --token <AGENT_INGEST_TOKEN>   # si l'ingestion est protegee
```

Chaque argument a aussi une variable d'environnement (`SONDE_ENDPOINT`, `SONDE_APP_ID`, `SONDE_INSTANCE_ID`, `SONDE_INTERVAL`, `SONDE_INGEST_TOKEN`). Le corps poste est `{"instance_id": "...", "metrics": {"cpu": 42.0, "memory": 55.0}}`.

## Documentation complete

Toute la documentation detaillee (parametres de configuration, WarpScript, persistance, multi-instance, troubleshooting, securite) se trouve dans [`INSTALL.md`](INSTALL.md).

## License

MIT
