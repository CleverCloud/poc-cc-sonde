use crate::config::{self, AgentProbe, ComputedLevel};
use crate::persistence::PersistenceBackend;
use crate::scaling::{run_scaling_loop, MetricSource, ScalingProbeSpec};
use std::collections::HashMap;
use std::sync::Arc;

impl ScalingProbeSpec for AgentProbe {
    fn name(&self) -> &str {
        &self.name
    }
    fn lock_kind(&self) -> &str {
        "agent"
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
        // Metrics are read locally (Redis/in-memory); no external fetch timeout to cover.
        0
    }
    fn suppress_command_output(&self) -> bool {
        self.suppress_command_output
    }
    fn on_failure_command(&self) -> Option<&str> {
        self.on_failure_command.as_deref()
    }
    fn get_failure_retries_before_command(&self) -> u32 {
        AgentProbe::get_failure_retries_before_command(self)
    }
    fn get_delay_after_onf_command_success(&self) -> u64 {
        AgentProbe::get_delay_after_onf_command_success(self)
    }
    fn get_delay_after_onf_command_failure(&self) -> u64 {
        AgentProbe::get_delay_after_onf_command_failure(self)
    }
    fn is_stateless(&self) -> bool {
        AgentProbe::is_stateless(self)
    }
    fn min_level(&self) -> u32 {
        AgentProbe::min_level(self)
    }
    fn max_level(&self) -> u32 {
        AgentProbe::max_level(self)
    }
    fn get_computed_level(&self, n: u32) -> Option<&ComputedLevel> {
        AgentProbe::get_computed_level(self, n)
    }
    fn should_scale_up(&self, current_level: u32, values: &HashMap<String, f64>) -> bool {
        AgentProbe::should_scale_up(self, current_level, values)
    }
    fn should_scale_down(&self, current_level: u32, values: &HashMap<String, f64>) -> bool {
        AgentProbe::should_scale_down(self, current_level, values)
    }
    fn upscale_command(&self) -> &str {
        &self.scaling.upscale_command
    }
    fn downscale_command(&self) -> &str {
        &self.scaling.downscale_command
    }
    fn delay_after_upscale_then_upscale(&self) -> u64 {
        config::delay_after_upscale_then_upscale(&self.scaling, self.interval_seconds)
    }
    fn delay_after_upscale_then_downscale(&self) -> u64 {
        config::delay_after_upscale_then_downscale(&self.scaling, self.interval_seconds)
    }
    fn delay_after_downscale_then_downscale(&self) -> u64 {
        config::delay_after_downscale_then_downscale(&self.scaling, self.interval_seconds)
    }
    fn delay_after_downscale_then_upscale(&self) -> u64 {
        config::delay_after_downscale_then_upscale(&self.scaling, self.interval_seconds)
    }
}

/// Metric source backed by the agent-pushed samples averaged over the window.
struct AgentMetricSource {
    backend: Arc<dyn PersistenceBackend>,
    app_id: String,
    window_seconds: u64,
}

#[async_trait::async_trait]
impl MetricSource for AgentMetricSource {
    async fn gather(&self) -> Result<HashMap<String, f64>, String> {
        // An empty map (no samples in the window) is NOT a failure: the scaling
        // logic simply takes no action when values are missing.
        self.backend
            .average_metrics(&self.app_id, self.window_seconds)
            .await
            .map_err(|e| e.to_string())
    }
}

pub async fn schedule_agent_probe(
    probe: AgentProbe,
    backend: Arc<dyn PersistenceBackend>,
    dry_run: bool,
    multi_instance: bool,
) {
    let app_id = match probe.apps.first() {
        Some(app) => app.id.clone(),
        None => {
            // validate() guarantees at least one app, but stay defensive.
            tracing::error!(probe_name = %probe.name, "Agent probe has no app, not scheduling");
            return;
        }
    };
    let source = AgentMetricSource {
        backend: backend.clone(),
        app_id,
        window_seconds: probe.window_seconds,
    };
    run_scaling_loop(probe, source, backend, dry_run, multi_instance).await;
}
