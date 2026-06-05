use crate::config::{ComputedLevel, WarpScriptProbe};
use crate::persistence::PersistenceBackend;
use crate::scaling::{run_scaling_loop, MetricSource, ScalingProbeSpec};
use crate::warpscript_probe;
use std::collections::HashMap;
use std::env;
use std::sync::Arc;
use std::time::Duration;
use tokio::task::JoinSet;
use tokio::time;
use tracing::{error, info};

impl ScalingProbeSpec for WarpScriptProbe {
    fn name(&self) -> &str {
        &self.name
    }
    fn lock_kind(&self) -> &str {
        "warpscript"
    }
    fn app_id(&self) -> Option<&str> {
        self.apps.first().map(|a| a.id.as_str())
    }
    fn interval_seconds(&self) -> u64 {
        self.interval_seconds
    }
    fn command_timeout_seconds(&self) -> u64 {
        self.command_timeout_seconds
    }
    fn lock_extra_seconds(&self) -> u64 {
        // The metric fetch is an HTTP call; cover its timeout in the lock TTL.
        self.get_request_timeout()
    }
    fn suppress_command_output(&self) -> bool {
        self.suppress_command_output
    }
    fn on_failure_command(&self) -> Option<&str> {
        self.on_failure_command.as_deref()
    }
    fn get_failure_retries_before_command(&self) -> u32 {
        WarpScriptProbe::get_failure_retries_before_command(self)
    }
    fn get_delay_after_onf_command_success(&self) -> u64 {
        WarpScriptProbe::get_delay_after_onf_command_success(self)
    }
    fn get_delay_after_onf_command_failure(&self) -> u64 {
        WarpScriptProbe::get_delay_after_onf_command_failure(self)
    }
    fn is_stateless(&self) -> bool {
        WarpScriptProbe::is_stateless(self)
    }
    fn min_level(&self) -> u32 {
        WarpScriptProbe::min_level(self)
    }
    fn max_level(&self) -> u32 {
        WarpScriptProbe::max_level(self)
    }
    fn get_computed_level(&self, n: u32) -> Option<&ComputedLevel> {
        WarpScriptProbe::get_computed_level(self, n)
    }
    fn should_scale_up(&self, current_level: u32, values: &HashMap<String, f64>) -> bool {
        WarpScriptProbe::should_scale_up(self, current_level, values)
    }
    fn should_scale_down(&self, current_level: u32, values: &HashMap<String, f64>) -> bool {
        WarpScriptProbe::should_scale_down(self, current_level, values)
    }
    fn upscale_command(&self) -> &str {
        &self.scaling.upscale_command
    }
    fn downscale_command(&self) -> &str {
        &self.scaling.downscale_command
    }
    fn delay_after_upscale_then_upscale(&self) -> u64 {
        WarpScriptProbe::delay_after_upscale_then_upscale(self)
    }
    fn delay_after_upscale_then_downscale(&self) -> u64 {
        WarpScriptProbe::delay_after_upscale_then_downscale(self)
    }
    fn delay_after_downscale_then_downscale(&self) -> u64 {
        WarpScriptProbe::delay_after_downscale_then_downscale(self)
    }
    fn delay_after_downscale_then_upscale(&self) -> u64 {
        WarpScriptProbe::delay_after_downscale_then_upscale(self)
    }
}

/// Metric source backed by WarpScript HTTP calls. `${WARP_TOKEN}` and `${APP_ID}`
/// are substituted once at construction (both are stable for the probe's life).
struct WarpScriptMetricSource {
    probe_name: String,
    client: reqwest::Client,
    /// metric name → fully-substituted WarpScript body
    scripts: HashMap<String, String>,
    endpoint: String,
    token: String,
    app_id: Option<String>,
    request_timeout: u64,
}

#[async_trait::async_trait]
impl MetricSource for WarpScriptMetricSource {
    async fn gather(&self) -> Result<HashMap<String, f64>, String> {
        let mut join_set: JoinSet<(String, Result<f64, warpscript_probe::WarpScriptError>)> =
            JoinSet::new();

        for (metric, script) in &self.scripts {
            let probe_name = self.probe_name.clone();
            let script = script.clone();
            let app_id = self.app_id.clone();
            let token = self.token.clone();
            let endpoint = self.endpoint.clone();
            let timeout = self.request_timeout;
            let client = self.client.clone();
            let metric = metric.clone();
            join_set.spawn(async move {
                let result = warpscript_probe::execute_warpscript(
                    &probe_name, &script, app_id.as_deref(), &token, &endpoint, timeout, &client,
                )
                .await;
                (metric, result)
            });
        }

        let mut values: HashMap<String, f64> = HashMap::new();
        let mut errors: Vec<String> = Vec::new();
        while let Some(joined) = join_set.join_next().await {
            match joined {
                Ok((metric, Ok(v))) => {
                    info!(probe_name = %self.probe_name, metric = %metric, value = v,
                          "WarpScript execution successful");
                    values.insert(metric, v);
                }
                Ok((metric, Err(e))) => {
                    error!(probe_name = %self.probe_name, metric = %metric, error = %e,
                           "WarpScript execution failed");
                    errors.push(format!("{}: {}", metric, e));
                }
                Err(e) => {
                    error!(probe_name = %self.probe_name, error = %e, "Metric task panicked");
                    errors.push(format!("task panicked: {}", e));
                }
            }
        }

        if errors.is_empty() {
            Ok(values)
        } else {
            Err(errors.join("; "))
        }
    }
}

pub async fn schedule_warpscript_probe(
    probe: WarpScriptProbe,
    backend: Arc<dyn PersistenceBackend>,
    dry_run: bool,
    multi_instance: bool,
) {
    let client = match warpscript_probe::build_client() {
        Ok(c) => c,
        Err(e) => {
            error!(probe_name = %probe.name, error = %e, "Failed to build HTTP client");
            return;
        }
    };

    // Resolve environment + token once (stable for the probe's lifetime).
    let endpoint = match env::var("WARP_ENDPOINT") {
        Ok(v) => v,
        Err(_) => {
            error!(probe_name = %probe.name, "WARP_ENDPOINT environment variable not set");
            return;
        }
    };
    let fallback_token = env::var("WARP_TOKEN").ok().filter(|t| !t.is_empty());
    let app = probe.apps.first();
    let app_id = app.map(|a| a.id.clone());
    let token = match app
        .and_then(|a| a.warp_token.as_deref().filter(|t| !t.is_empty()))
        .or(fallback_token.as_deref())
    {
        Some(t) => t.to_string(),
        None => {
            error!(probe_name = %probe.name,
                   "No Warp token available (neither app warp_token nor WARP_TOKEN env var set)");
            return;
        }
    };

    // Load all WarpScript files before the loop, retrying on transient errors.
    let scripts: HashMap<String, String> = loop {
        let mut loaded: HashMap<String, String> = HashMap::new();
        let mut all_ok = true;
        for (metric, path) in &probe.warpscript_files {
            match tokio::fs::read_to_string(path).await {
                Ok(content) => {
                    loaded.insert(metric.clone(), content);
                }
                Err(e) => {
                    error!(
                        probe_name = %probe.name,
                        metric = %metric,
                        file = %path,
                        error = %e,
                        retry_in_seconds = probe.interval_seconds,
                        "Failed to read WarpScript file, will retry"
                    );
                    all_ok = false;
                }
            }
        }
        if all_ok {
            break loaded;
        }
        time::sleep(Duration::from_secs(probe.interval_seconds)).await;
    };

    // Pre-substitute the stable placeholders once.
    let substituted_scripts: HashMap<String, String> = scripts
        .iter()
        .map(|(k, v)| {
            let s = v.replace("${WARP_TOKEN}", &token);
            let s = if let Some(ref id) = app_id { s.replace("${APP_ID}", id) } else { s };
            (k.clone(), s)
        })
        .collect();

    let request_timeout = probe.get_request_timeout();
    let source = WarpScriptMetricSource {
        probe_name: probe.name.clone(),
        client,
        scripts: substituted_scripts,
        endpoint,
        token,
        app_id,
        request_timeout,
    };

    run_scaling_loop(probe, source, backend, dry_run, multi_instance).await;
}
