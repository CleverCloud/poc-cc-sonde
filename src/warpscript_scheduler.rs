use crate::config::WarpScriptProbe;
use crate::executor;
use crate::persistence::{self, PersistenceBackend, WarpScriptProbeState};
use crate::warpscript_probe;
use std::sync::Arc;
use std::time::Duration;
use tokio::time;
use tracing::{debug, error, info};

/// Execute a command with app_id substitution
async fn execute_scaling_command(
    probe_name: &str,
    command: &str,
    app_id: Option<&str>,
    timeout_seconds: u64,
    action: &str, // "upscale" or "downscale"
) {
    // Substitute ${APP_ID} if present
    let cmd = if let Some(id) = app_id {
        command.replace("${APP_ID}", id)
    } else {
        command.to_string()
    };

    info!(
        probe_name = %probe_name,
        command = %cmd,
        action = %action,
        "Executing {} command", action
    );

    match executor::execute_command(&cmd, timeout_seconds).await {
        Ok(output) => {
            if output.status.success() {
                info!(
                    probe_name = %probe_name,
                    "{} command completed successfully", action
                );
            } else {
                error!(
                    probe_name = %probe_name,
                    exit_code = output.status.code().unwrap_or(-1),
                    "{} command completed with errors", action
                );
            }
        }
        Err(e) => {
            error!(
                probe_name = %probe_name,
                error = %e,
                "Failed to execute {} command", action
            );
        }
    }
}

pub async fn schedule_warpscript_probe(probe: WarpScriptProbe, backend: Arc<dyn PersistenceBackend>) {
    info!(
        probe_name = %probe.name,
        interval_seconds = probe.interval_seconds,
        levels_count = probe.levels.len(),
        min_level = probe.min_level(),
        max_level = probe.max_level(),
        "Starting WarpScript probe scheduler"
    );

    // Validate levels
    if probe.levels.is_empty() {
        error!(
            probe_name = %probe.name,
            "No levels defined for WarpScript probe"
        );
        return;
    }

    // Load previous state if exists
    let previous_state = backend.load_warpscript_state(&probe.name).await.ok().flatten();

    let mut current_level = match &previous_state {
        Some(state) => {
            info!(
                probe_name = %probe.name,
                current_level = state.current_level,
                last_value = state.last_value,
                "Resuming WarpScript probe from saved state"
            );
            state.current_level
        }
        None => {
            // Start at minimum level
            let initial_level = probe.min_level();
            info!(
                probe_name = %probe.name,
                initial_level = initial_level,
                "No previous state found, starting at minimum level"
            );
            initial_level
        }
    };

    let mut next_delay = 0u64;

    loop {
        // Wait for the calculated delay
        if next_delay > 0 {
            debug!(
                probe_name = %probe.name,
                delay_seconds = next_delay,
                "Waiting before next execution"
            );
            time::sleep(Duration::from_secs(next_delay)).await;
        }

        info!(
            probe_name = %probe.name,
            current_level = current_level,
            "Executing WarpScript probe"
        );

        let check_timestamp = persistence::current_timestamp();

        // Get app (should have exactly one if expanded correctly)
        let app = probe.apps.first();
        let app_id = app.map(|a| a.id.as_str());
        let custom_token = app.and_then(|a| a.warp_token.as_deref());

        // Execute WarpScript and get value
        let value = match warpscript_probe::execute_warpscript(&probe.name, &probe.warpscript_file, app_id, custom_token).await {
            Ok(v) => {
                info!(
                    probe_name = %probe.name,
                    value = v,
                    current_level = current_level,
                    "WarpScript execution successful"
                );
                v
            }
            Err(e) => {
                error!(
                    probe_name = %probe.name,
                    error = %e,
                    "WarpScript execution failed"
                );
                // On error, keep current level and retry after interval
                next_delay = probe.interval_seconds;
                continue;
            }
        };

        // Check if we should scale up
        if probe.should_scale_up(current_level, value) {
            let new_level = current_level + 1;
            info!(
                probe_name = %probe.name,
                from_level = current_level,
                to_level = new_level,
                value = value,
                "Scaling UP detected"
            );

            // Execute upscale command for current level
            if let Some(level_config) = probe.get_level(current_level) {
                if let Some(ref cmd) = level_config.upscale_command {
                    execute_scaling_command(
                        &probe.name,
                        cmd,
                        app_id,
                        probe.command_timeout_seconds,
                        "upscale"
                    ).await;
                } else {
                    debug!(
                        probe_name = %probe.name,
                        level = current_level,
                        "No upscale command defined for this level"
                    );
                }
            }

            current_level = new_level;
            next_delay = probe.get_delay_after_scale();
        }
        // Check if we should scale down
        else if probe.should_scale_down(current_level, value) {
            let new_level = current_level - 1;
            info!(
                probe_name = %probe.name,
                from_level = current_level,
                to_level = new_level,
                value = value,
                "Scaling DOWN detected"
            );

            // Execute downscale command for current level
            if let Some(level_config) = probe.get_level(current_level) {
                if let Some(ref cmd) = level_config.downscale_command {
                    execute_scaling_command(
                        &probe.name,
                        cmd,
                        app_id,
                        probe.command_timeout_seconds,
                        "downscale"
                    ).await;
                } else {
                    debug!(
                        probe_name = %probe.name,
                        level = current_level,
                        "No downscale command defined for this level"
                    );
                }
            }

            current_level = new_level;
            next_delay = probe.get_delay_after_scale();
        }
        // No scaling needed
        else {
            debug!(
                probe_name = %probe.name,
                level = current_level,
                value = value,
                "No scaling action needed, level unchanged"
            );
            next_delay = probe.interval_seconds;
        }

        // Save state
        let state = WarpScriptProbeState {
            probe_name: probe.name.clone(),
            last_check_timestamp: check_timestamp,
            current_level,
            last_value: value,
            next_check_timestamp: check_timestamp + next_delay,
        };

        if let Err(e) = backend.save_warpscript_state(&state).await {
            error!(
                probe_name = %probe.name,
                error = %e,
                "Failed to save WarpScript state"
            );
        }

        debug!(
            probe_name = %probe.name,
            next_delay_seconds = next_delay,
            level = current_level,
            "Scheduled next execution"
        );
    }
}
