//! Shared scaling engine.
//!
//! The per-cycle loop (distributed lock, state refresh, directional cooldowns,
//! scale up/down decision, command execution, state persistence) is identical
//! for every probe type that scales. The only thing that differs is *where the
//! metric values come from*: a WarpScript HTTP call, or the average of
//! agent-pushed samples. That difference is captured by the [`MetricSource`]
//! trait; the probe's scaling parameters by [`ScalingProbeSpec`].

use crate::config::ComputedLevel;
use crate::executor;
use crate::persistence::{self, PersistenceBackend, WarpScriptProbeState};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;
use tracing::{debug, error, info, warn};

/// Scaling parameters a probe must expose for the engine to drive it.
/// Implemented by `WarpScriptProbe` and `AgentProbe`.
pub trait ScalingProbeSpec: Send + Sync {
    fn name(&self) -> &str;
    /// Distinguishes lock keys between probe kinds (e.g. "warpscript", "agent").
    fn lock_kind(&self) -> &str;
    fn app_id(&self) -> Option<&str>;
    fn interval_seconds(&self) -> u64;
    fn command_timeout_seconds(&self) -> u64;
    /// Extra seconds added to the lock TTL on top of `command_timeout_seconds`
    /// (e.g. the metric-fetch request timeout). 0 when the source is local.
    fn lock_extra_seconds(&self) -> u64;
    fn suppress_command_output(&self) -> bool;
    fn on_failure_command(&self) -> Option<&str>;
    fn get_failure_retries_before_command(&self) -> u32;
    fn get_delay_after_onf_command_success(&self) -> u64;
    fn get_delay_after_onf_command_failure(&self) -> u64;
    fn is_stateless(&self) -> bool;
    fn min_level(&self) -> u32;
    fn max_level(&self) -> u32;
    fn get_computed_level(&self, n: u32) -> Option<&ComputedLevel>;
    fn should_scale_up(&self, current_level: u32, values: &HashMap<String, f64>) -> bool;
    fn should_scale_down(&self, current_level: u32, values: &HashMap<String, f64>) -> bool;
    fn upscale_command(&self) -> &str;
    fn downscale_command(&self) -> &str;
    fn delay_after_upscale_then_upscale(&self) -> u64;
    fn delay_after_upscale_then_downscale(&self) -> u64;
    fn delay_after_downscale_then_downscale(&self) -> u64;
    fn delay_after_downscale_then_upscale(&self) -> u64;
}

/// Provides the metric values for one scaling cycle.
#[async_trait::async_trait]
pub trait MetricSource: Send + Sync {
    /// Returns the metric values to compare against thresholds. `Err(reason)`
    /// drives the failure path (consecutive_failures, optional on_failure_command).
    async fn gather(&self) -> Result<HashMap<String, f64>, String>;
}

pub(crate) struct ScalingCommandArgs<'a> {
    pub probe_name: &'a str,
    pub command: &'a str,
    pub app_id: Option<&'a str>,
    pub flavor: &'a str,
    pub instances: u32,
    pub timeout_seconds: u64,
    /// `"upscale"` or `"downscale"`
    pub action: &'a str,
    pub log_output: bool,
}

/// Execute a scaling command with variable substitution.
///
/// Substitutes `${APP_ID}`, `${FLAVOR}`, and `${INSTANCES}` in the command string.
/// Returns `true` if the command succeeded, `false` otherwise.
pub(crate) async fn execute_scaling_command(args: ScalingCommandArgs<'_>) -> bool {
    let ScalingCommandArgs {
        probe_name,
        command,
        app_id,
        flavor,
        instances,
        timeout_seconds,
        action,
        log_output,
    } = args;

    let mut cmd = command.to_string();
    if let Some(id) = app_id {
        cmd = cmd.replace("${APP_ID}", id);
    }
    cmd = cmd.replace("${FLAVOR}", flavor);
    cmd = cmd.replace("${INSTANCES}", &instances.to_string());

    warn!(probe_name = %probe_name, action = %action, "Executing {} command", action);
    debug!(command = %cmd, "Scaling command detail");

    match executor::execute_command(&cmd, timeout_seconds, log_output).await {
        Ok(output) if output.status.success() => {
            if log_output {
                warn!(probe_name = %probe_name, "{} command completed successfully", action);
            }
            true
        }
        Ok(output) => {
            error!(
                probe_name = %probe_name,
                exit_code = output.status.code().unwrap_or(-1),
                "{} command completed with errors", action
            );
            false
        }
        Err(e) => {
            error!(probe_name = %probe_name, error = %e, "Failed to execute {} command", action);
            false
        }
    }
}

/// Drive a scaling probe forever: each cycle gathers metrics from `source` and
/// applies the shared scaling logic. This is the former WarpScript scheduler
/// loop, generalised over the metric source.
pub async fn run_scaling_loop<P, S>(
    probe: P,
    source: S,
    backend: Arc<dyn PersistenceBackend>,
    dry_run: bool,
    multi_instance: bool,
) where
    P: ScalingProbeSpec,
    S: MetricSource,
{
    if probe.is_stateless() {
        info!(
            probe_name = %probe.name(),
            interval_seconds = probe.interval_seconds(),
            "Starting scaling probe scheduler (stateless mode)"
        );
    } else {
        info!(
            probe_name = %probe.name(),
            interval_seconds = probe.interval_seconds(),
            min_level = probe.min_level(),
            max_level = probe.max_level(),
            "Starting scaling probe scheduler"
        );
    }

    let previous_state = match backend.load_warpscript_state(probe.name()).await {
        Ok(state) => state,
        Err(e) => {
            warn!(probe_name = %probe.name(), error = %e,
                  "Failed to load initial state, starting fresh");
            None
        }
    };

    let (mut current_level, mut next_delay) = match &previous_state {
        Some(state) => {
            let now = persistence::current_timestamp();
            let delay = if state.next_check_timestamp > now {
                let remaining = state.next_check_timestamp - now;
                info!(
                    probe_name = %probe.name(),
                    remaining_seconds = remaining,
                    current_level = state.current_level,
                    "Resuming scaling probe from saved state"
                );
                remaining
            } else {
                info!(
                    probe_name = %probe.name(),
                    current_level = state.current_level,
                    "Saved state expired, starting immediately"
                );
                0
            };
            let loaded_level = state.current_level;
            let current_level = if probe.is_stateless() {
                probe.min_level()
            } else if probe.get_computed_level(loaded_level).is_some() {
                loaded_level
            } else {
                let clamped = probe.min_level();
                warn!(
                    probe_name = %probe.name(),
                    loaded = loaded_level,
                    clamped,
                    "Loaded level not in config, resetting to min"
                );
                clamped
            };
            (current_level, delay)
        }
        None => {
            let initial_level = probe.min_level();
            info!(
                probe_name = %probe.name(),
                initial_level,
                "No previous state found, starting immediately"
            );
            (initial_level, 0)
        }
    };

    let mut consecutive_failures: u32 = previous_state
        .as_ref()
        .map(|s| s.consecutive_failures)
        .unwrap_or(0);
    let mut last_values: HashMap<String, f64> = previous_state
        .as_ref()
        .map(|s| s.last_values.clone())
        .unwrap_or_default();
    let mut upscale_blocked_until: u64 = previous_state
        .as_ref()
        .map(|s| s.upscale_blocked_until)
        .unwrap_or(0);
    let mut downscale_blocked_until: u64 = previous_state
        .as_ref()
        .map(|s| s.downscale_blocked_until)
        .unwrap_or(0);
    let mut consecutive_scaling_failures: u32 = previous_state
        .as_ref()
        .map(|s| s.consecutive_scaling_failures)
        .unwrap_or(0);

    loop {
        if next_delay > 0 {
            debug!(probe_name = %probe.name(), delay_seconds = next_delay, "Waiting before next execution");
            time::sleep(Duration::from_secs(next_delay)).await;
        }

        let lock_key = format!("poc-sonde:lock:{}:{}", probe.lock_kind(), probe.name());
        let ttl_ms = (probe.lock_extra_seconds() + probe.command_timeout_seconds() + 10) * 1000;

        let lock_token = match backend.acquire_lock(&lock_key, ttl_ms).await {
            Ok(None) => {
                debug!(probe_name = %probe.name(), "Lock held by another instance, skipping cycle");
                next_delay = probe.interval_seconds();
                continue;
            }
            Err(e) => {
                if multi_instance {
                    error!(probe_name = %probe.name(), error = %e,
                           "Lock acquisition failed in multi-instance mode, skipping cycle");
                    next_delay = probe.interval_seconds();
                    continue;
                }
                warn!(probe_name = %probe.name(), error = %e,
                      "Lock acquisition failed, proceeding without lock");
                None
            }
            Ok(Some(token)) => Some(token),
        };

        // Re-read state under the lock to pick up changes from other instances.
        let mut new_upscale_blocked = upscale_blocked_until;
        let mut new_downscale_blocked = downscale_blocked_until;
        let mut refresh_failed_multi = false;
        let mut skip_with_delay: Option<u64> = None;
        match backend.load_warpscript_state(probe.name()).await {
            Ok(Some(fresh_state)) => {
                if fresh_state.current_level != current_level {
                    info!(
                        probe_name = %probe.name(),
                        stale = current_level,
                        fresh = fresh_state.current_level,
                        "State refreshed from Redis after lock acquisition"
                    );
                }
                let loaded_level = fresh_state.current_level;
                current_level = if probe.is_stateless() {
                    probe.min_level()
                } else if probe.get_computed_level(loaded_level).is_some() {
                    loaded_level
                } else {
                    let clamped = probe.min_level();
                    warn!(
                        probe_name = %probe.name(),
                        loaded = loaded_level,
                        clamped,
                        "Refreshed level not in config, resetting to min"
                    );
                    clamped
                };
                consecutive_failures = fresh_state.consecutive_failures;
                consecutive_scaling_failures = fresh_state.consecutive_scaling_failures;
                last_values = fresh_state.last_values.clone();
                new_upscale_blocked = fresh_state.upscale_blocked_until;
                new_downscale_blocked = fresh_state.downscale_blocked_until;
                let now_ts = persistence::current_timestamp();
                if fresh_state.next_check_timestamp > now_ts {
                    skip_with_delay = Some(fresh_state.next_check_timestamp - now_ts);
                }
            }
            Ok(None) => {}
            Err(e) => {
                let e_str = e.to_string();
                if multi_instance {
                    error!(probe_name = %probe.name(), error = %e_str,
                           "Failed to refresh state from Redis, skipping cycle (fail-close)");
                    refresh_failed_multi = true;
                } else {
                    warn!(probe_name = %probe.name(), error = %e_str,
                          "Failed to refresh state, proceeding with cached values");
                }
            }
        }
        upscale_blocked_until = new_upscale_blocked;
        downscale_blocked_until = new_downscale_blocked;

        if refresh_failed_multi {
            release_lock(&backend, &lock_key, &lock_token, probe.name()).await;
            next_delay = probe.interval_seconds();
            continue;
        }

        if let Some(delay) = skip_with_delay {
            debug!(probe_name = %probe.name(), remaining_seconds = delay,
                   "Another instance scheduled a future check; releasing lock");
            release_lock(&backend, &lock_key, &lock_token, probe.name()).await;
            next_delay = delay;
            continue;
        }

        let now = persistence::current_timestamp();
        if now < upscale_blocked_until && now < downscale_blocked_until {
            debug!(
                probe_name = %probe.name(),
                upscale_remaining = upscale_blocked_until - now,
                downscale_remaining = downscale_blocked_until - now,
                "Both scaling directions still in cooldown, releasing lock and waiting"
            );
            release_lock(&backend, &lock_key, &lock_token, probe.name()).await;
            next_delay = upscale_blocked_until.min(downscale_blocked_until).saturating_sub(now);
            continue;
        }

        info!(probe_name = %probe.name(), current_level, "Executing scaling probe");

        let check_timestamp = persistence::current_timestamp();
        let app_id = probe.app_id();

        // Gather metric values from the source.
        let gather = source.gather().await;

        if let Err(reason) = gather {
            consecutive_failures += 1;
            error!(probe_name = %probe.name(), consecutive_failures, error = %reason,
                   "Metric gathering failed");

            next_delay = probe.interval_seconds();

            if let Some(command) = probe.on_failure_command() {
                let threshold = probe.get_failure_retries_before_command();
                if consecutive_failures > threshold {
                    let cmd = if let Some(id) = app_id {
                        command.replace("${APP_ID}", id)
                    } else {
                        command.to_string()
                    };
                    warn!(probe_name = %probe.name(), consecutive_failures, threshold,
                          "Failure threshold reached, executing command");
                    if dry_run {
                        warn!(probe_name = %probe.name(), "DRY RUN: skipping failure command");
                        debug!(command = %cmd, "DRY RUN command detail");
                        next_delay = probe.get_delay_after_onf_command_success();
                    } else {
                        match executor::execute_command(&cmd, probe.command_timeout_seconds(), !probe.suppress_command_output()).await {
                            Ok(output) if output.status.success() => {
                                warn!(probe_name = %probe.name(), "Failure command completed successfully");
                                next_delay = probe.get_delay_after_onf_command_success();
                            }
                            Ok(_) => {
                                error!(probe_name = %probe.name(), "Failure command completed with errors");
                                next_delay = probe.get_delay_after_onf_command_failure();
                            }
                            Err(e) => {
                                error!(probe_name = %probe.name(), error = %e, "Failed to execute failure command");
                                next_delay = probe.get_delay_after_onf_command_failure();
                            }
                        }
                    }
                } else {
                    info!(probe_name = %probe.name(), consecutive_failures, threshold,
                          remaining = threshold - consecutive_failures,
                          "Failure threshold not reached, retrying without command");
                }
            }

            save_state(&backend, &probe, check_timestamp, current_level, &last_values,
                       next_delay, consecutive_failures, upscale_blocked_until,
                       downscale_blocked_until, consecutive_scaling_failures).await;
            release_lock(&backend, &lock_key, &lock_token, probe.name()).await;
            continue;
        }

        let metric_values = gather.unwrap();
        consecutive_failures = 0;
        last_values = metric_values.clone();

        // Determine scaling action.
        if probe.should_scale_up(current_level, &metric_values) {
            if check_timestamp < upscale_blocked_until {
                debug!(probe_name = %probe.name(),
                       remaining_seconds = upscale_blocked_until - check_timestamp,
                       "Upscale cooldown active, skipping upscale");
                next_delay = probe.interval_seconds();
            } else {
                let command_ok = run_scale_command(&probe, app_id, true, current_level, dry_run).await;
                if command_ok {
                    consecutive_scaling_failures = 0;
                    if !probe.is_stateless() {
                        current_level += 1;
                    }
                    let now = persistence::current_timestamp();
                    upscale_blocked_until = now + probe.delay_after_upscale_then_upscale();
                    downscale_blocked_until = now + probe.delay_after_upscale_then_downscale();
                    next_delay = upscale_blocked_until.min(downscale_blocked_until).saturating_sub(now);
                } else {
                    consecutive_scaling_failures += 1;
                    warn!(probe_name = %probe.name(), current_level, consecutive_scaling_failures,
                          "Scaling command failed — level not updated");
                    next_delay = probe.interval_seconds();
                }
            }
        } else if probe.should_scale_down(current_level, &metric_values) {
            if check_timestamp < downscale_blocked_until {
                debug!(probe_name = %probe.name(),
                       remaining_seconds = downscale_blocked_until - check_timestamp,
                       "Downscale cooldown active, skipping downscale");
                next_delay = probe.interval_seconds();
            } else {
                let command_ok = run_scale_command(&probe, app_id, false, current_level, dry_run).await;
                if command_ok {
                    consecutive_scaling_failures = 0;
                    if !probe.is_stateless() {
                        current_level -= 1;
                    }
                    let now = persistence::current_timestamp();
                    downscale_blocked_until = now + probe.delay_after_downscale_then_downscale();
                    upscale_blocked_until = now + probe.delay_after_downscale_then_upscale();
                    next_delay = upscale_blocked_until.min(downscale_blocked_until).saturating_sub(now);
                } else {
                    consecutive_scaling_failures += 1;
                    warn!(probe_name = %probe.name(), current_level, consecutive_scaling_failures,
                          "Scaling command failed — level not updated");
                    next_delay = probe.interval_seconds();
                }
            }
        } else {
            debug!(probe_name = %probe.name(), level = current_level,
                   "No scaling action needed, level unchanged");
            next_delay = probe.interval_seconds();
        }

        save_state(&backend, &probe, check_timestamp, current_level, &last_values,
                   next_delay, consecutive_failures, upscale_blocked_until,
                   downscale_blocked_until, consecutive_scaling_failures).await;
        release_lock(&backend, &lock_key, &lock_token, probe.name()).await;

        debug!(probe_name = %probe.name(), next_delay_seconds = next_delay, level = current_level,
               "Scheduled next execution");
    }
}

/// Run the up/downscale command for the current cycle, handling stateless vs
/// level-based mode and dry-run. Returns whether the command succeeded.
async fn run_scale_command<P: ScalingProbeSpec>(
    probe: &P,
    app_id: Option<&str>,
    up: bool,
    current_level: u32,
    dry_run: bool,
) -> bool {
    let (cmd, action) = if up {
        (probe.upscale_command(), "upscale")
    } else {
        (probe.downscale_command(), "downscale")
    };

    if probe.is_stateless() {
        warn!(probe_name = %probe.name(), "Scaling {} detected (stateless)", action.to_uppercase());
        if dry_run {
            warn!(probe_name = %probe.name(), "DRY RUN: skipping {} command", action);
            debug!(command = %cmd, "DRY RUN command detail");
            return true;
        }
        return execute_scaling_command(ScalingCommandArgs {
            probe_name: probe.name(),
            command: cmd,
            app_id,
            flavor: "",
            instances: 0,
            timeout_seconds: probe.command_timeout_seconds(),
            action,
            log_output: !probe.suppress_command_output(),
        })
        .await;
    }

    let new_level = if up { current_level + 1 } else { current_level - 1 };
    warn!(probe_name = %probe.name(), from_level = current_level, to_level = new_level,
          "Scaling {} detected", action.to_uppercase());
    // Level bounds are enforced by should_scale_up/down, so the level exists.
    let computed = probe.get_computed_level(new_level).unwrap();
    if dry_run {
        warn!(probe_name = %probe.name(), flavor = %computed.flavor, instances = computed.instances,
              from_level = current_level, to_level = new_level, "DRY RUN: skipping {} command", action);
        debug!(command = %cmd, "DRY RUN command detail");
        return true;
    }
    let (flavor, instances) = (computed.flavor.clone(), computed.instances);
    execute_scaling_command(ScalingCommandArgs {
        probe_name: probe.name(),
        command: cmd,
        app_id,
        flavor: &flavor,
        instances,
        timeout_seconds: probe.command_timeout_seconds(),
        action,
        log_output: !probe.suppress_command_output(),
    })
    .await
}

#[allow(clippy::too_many_arguments)]
async fn save_state<P: ScalingProbeSpec>(
    backend: &Arc<dyn PersistenceBackend>,
    probe: &P,
    check_timestamp: u64,
    current_level: u32,
    last_values: &HashMap<String, f64>,
    next_delay: u64,
    consecutive_failures: u32,
    upscale_blocked_until: u64,
    downscale_blocked_until: u64,
    consecutive_scaling_failures: u32,
) {
    let state = WarpScriptProbeState {
        probe_name: probe.name().to_string(),
        last_check_timestamp: check_timestamp,
        current_level,
        last_values: last_values.clone(),
        next_check_timestamp: check_timestamp + next_delay,
        consecutive_failures,
        upscale_blocked_until,
        downscale_blocked_until,
        consecutive_scaling_failures,
    };
    if let Err(e) = backend.save_warpscript_state(&state).await {
        error!(probe_name = %probe.name(), error = %e, "Failed to save scaling probe state");
    }
}

async fn release_lock(
    backend: &Arc<dyn PersistenceBackend>,
    lock_key: &str,
    lock_token: &Option<String>,
    probe_name: &str,
) {
    if let Some(token) = lock_token {
        if let Err(e) = backend.release_lock(lock_key, token).await {
            debug!(probe_name = %probe_name, error = %e, "Failed to release lock (will expire via TTL)");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_scaling_command_failure_returns_false() {
        let ok = execute_scaling_command(ScalingCommandArgs {
            probe_name: "test",
            command: "exit 1",
            app_id: None,
            flavor: "S",
            instances: 1,
            timeout_seconds: 5,
            action: "upscale",
            log_output: true,
        })
        .await;
        assert!(!ok);
    }

    #[tokio::test]
    async fn test_scaling_command_success_returns_true() {
        let ok = execute_scaling_command(ScalingCommandArgs {
            probe_name: "test",
            command: "true",
            app_id: None,
            flavor: "S",
            instances: 1,
            timeout_seconds: 5,
            action: "upscale",
            log_output: true,
        })
        .await;
        assert!(ok);
    }

    #[tokio::test]
    async fn test_scaling_command_spawn_error_returns_false() {
        let ok = execute_scaling_command(ScalingCommandArgs {
            probe_name: "test",
            command: "nonexistent_xyz_cmd_42",
            app_id: None,
            flavor: "S",
            instances: 1,
            timeout_seconds: 5,
            action: "upscale",
            log_output: true,
        })
        .await;
        assert!(!ok);
    }

    #[tokio::test]
    async fn test_scaling_command_substitutes_flavor_and_instances() {
        let ok = execute_scaling_command(ScalingCommandArgs {
            probe_name: "test",
            command: "echo ${FLAVOR} ${INSTANCES}",
            app_id: Some("myapp"),
            flavor: "XL",
            instances: 3,
            timeout_seconds: 5,
            action: "upscale",
            log_output: true,
        })
        .await;
        assert!(ok);
    }
}
