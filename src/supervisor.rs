use crate::config::Config;
use crate::persistence::PersistenceBackend;
use crate::{agent_scheduler, healthcheck_scheduler, utils, warpscript_scheduler};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::Mutex;
use tokio::task::JoinHandle;
use tracing::info;

/// Spawns and supervises every probe task. Holds the live `JoinHandle`s so the
/// whole fleet can be aborted and respawned from a new configuration (hot-reload)
/// without restarting the process.
pub struct Supervisor {
    backend: Arc<dyn PersistenceBackend>,
    dry_run: bool,
    multi_instance: bool,
    handles: Mutex<Vec<JoinHandle<()>>>,
    /// App ids that currently have an agent probe declared. The metrics ingest
    /// endpoint only accepts these. Updated on every reload.
    agent_apps: Arc<Mutex<HashSet<String>>>,
}

impl Supervisor {
    pub fn new(
        backend: Arc<dyn PersistenceBackend>,
        dry_run: bool,
        multi_instance: bool,
    ) -> Arc<Self> {
        Arc::new(Self {
            backend,
            dry_run,
            multi_instance,
            handles: Mutex::new(Vec::new()),
            agent_apps: Arc::new(Mutex::new(HashSet::new())),
        })
    }

    /// Shared handle to the set of agent app ids, for the web ingest endpoint.
    pub fn agent_apps(&self) -> Arc<Mutex<HashSet<String>>> {
        self.agent_apps.clone()
    }

    /// Spawn all probe tasks described by `config`. Replaces any tasks currently
    /// running: existing handles are aborted first, then a fresh set is spawned.
    pub async fn reload(&self, config: Config) {
        // Refresh the set of apps the ingest endpoint accepts.
        let new_agent_apps: HashSet<String> = config
            .agent_probes
            .iter()
            .flat_map(|p| p.apps.iter().map(|a| a.id.clone()))
            .collect();
        *self.agent_apps.lock().await = new_agent_apps;

        let mut handles = self.handles.lock().await;

        if !handles.is_empty() {
            info!(count = handles.len(), "Aborting current probe tasks before reload");
            for handle in handles.iter() {
                handle.abort();
            }
            // Drain and await aborted tasks so their resources are released.
            for handle in handles.drain(..) {
                let _ = handle.await;
            }
        }

        *handles = spawn_probes(config, &self.backend, self.dry_run, self.multi_instance);
        info!(count = handles.len(), "Probe tasks (re)spawned");
    }

    /// Abort all running probe tasks (used during shutdown).
    pub async fn abort_all(&self) {
        let handles = self.handles.lock().await;
        for handle in handles.iter() {
            handle.abort();
        }
    }
}

/// Build the full list of probe tasks from a validated configuration.
/// Health-check and WarpScript probes with `apps` are expanded into one task
/// per app, mirroring the historical startup behaviour.
fn spawn_probes(
    config: Config,
    backend: &Arc<dyn PersistenceBackend>,
    dry_run: bool,
    multi_instance: bool,
) -> Vec<JoinHandle<()>> {
    let mut handles = Vec::new();

    for probe in config.healthcheck_probes {
        if probe.apps.is_empty() {
            info!(
                probe_name = %probe.name,
                url = %utils::sanitize_url_for_log(probe.url.as_deref().unwrap_or("")),
                interval_seconds = probe.interval_seconds,
                "Spawning healthcheck probe task"
            );
            let backend_clone = backend.clone();
            handles.push(tokio::spawn(healthcheck_scheduler::schedule_probe(
                probe,
                backend_clone,
                dry_run,
                multi_instance,
            )));
        } else {
            info!(
                probe_name = %probe.name,
                apps_count = probe.apps.len(),
                "Expanding healthcheck probe for each app"
            );
            for app in &probe.apps {
                let mut probe_instance = probe.clone();
                probe_instance.name = format!("{} - {}", probe.name, app.id);
                probe_instance.url = Some(app.url.clone());
                probe_instance.apps = vec![app.clone()];

                info!(
                    probe_name = %probe_instance.name,
                    app_id = %app.id,
                    url = %utils::sanitize_url_for_log(&app.url),
                    interval_seconds = probe_instance.interval_seconds,
                    "Spawning healthcheck probe instance"
                );
                let backend_clone = backend.clone();
                handles.push(tokio::spawn(healthcheck_scheduler::schedule_probe(
                    probe_instance,
                    backend_clone,
                    dry_run,
                    multi_instance,
                )));
            }
        }
    }

    for probe in config.warpscript_probes {
        if probe.apps.is_empty() {
            info!(
                probe_name = %probe.name,
                interval_seconds = probe.interval_seconds,
                metrics_count = probe.warpscript_files.len(),
                "Spawning WarpScript probe task"
            );
            let backend_clone = backend.clone();
            handles.push(tokio::spawn(warpscript_scheduler::schedule_warpscript_probe(
                probe,
                backend_clone,
                dry_run,
                multi_instance,
            )));
        } else {
            info!(
                probe_name = %probe.name,
                apps_count = probe.apps.len(),
                "Expanding WarpScript probe for each app"
            );
            for app in &probe.apps {
                let mut probe_instance = probe.clone();
                probe_instance.name = format!("{} - {}", probe.name, app.id);
                probe_instance.apps = vec![app.clone()];

                info!(
                    probe_name = %probe_instance.name,
                    app_id = %app.id,
                    has_custom_token = app.warp_token.is_some(),
                    interval_seconds = probe_instance.interval_seconds,
                    metrics_count = probe_instance.warpscript_files.len(),
                    "Spawning WarpScript probe instance"
                );
                let backend_clone = backend.clone();
                handles.push(tokio::spawn(warpscript_scheduler::schedule_warpscript_probe(
                    probe_instance,
                    backend_clone,
                    dry_run,
                    multi_instance,
                )));
            }
        }
    }

    // Agent probes: one task per app (the ingest endpoint is keyed by app id).
    for probe in config.agent_probes {
        info!(
            probe_name = %probe.name,
            apps_count = probe.apps.len(),
            window_seconds = probe.window_seconds,
            "Expanding agent probe for each app"
        );
        for app in &probe.apps {
            let mut probe_instance = probe.clone();
            probe_instance.name = format!("{} - {}", probe.name, app.id);
            probe_instance.apps = vec![app.clone()];

            info!(
                probe_name = %probe_instance.name,
                app_id = %app.id,
                interval_seconds = probe_instance.interval_seconds,
                "Spawning agent probe instance"
            );
            let backend_clone = backend.clone();
            handles.push(tokio::spawn(agent_scheduler::schedule_agent_probe(
                probe_instance,
                backend_clone,
                dry_run,
                multi_instance,
            )));
        }
    }

    handles
}
