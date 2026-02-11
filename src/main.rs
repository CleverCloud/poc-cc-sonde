mod config;
mod executor;
mod probe;
mod scheduler;

use std::env;
use tracing::info;
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing/logging
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "poc_sonde=info".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    info!("Starting HTTP monitoring application");

    // Get config file path from command line args or use default
    let args: Vec<String> = env::args().collect();
    let config_path = args.get(1).map(|s| s.as_str()).unwrap_or("config.toml");

    info!(config_path = %config_path, "Loading configuration");

    // Load and validate configuration
    let config = config::Config::from_file(config_path)?;

    info!(
        probe_count = config.probes.len(),
        "Configuration loaded successfully"
    );

    // Spawn a task for each probe
    let mut handles = vec![];

    for probe in config.probes {
        info!(
            probe_name = %probe.name,
            url = %probe.url,
            interval_seconds = probe.interval_seconds,
            "Spawning probe task"
        );

        let handle = tokio::spawn(scheduler::schedule_probe(probe));
        handles.push(handle);
    }

    info!("All probe tasks spawned, waiting for shutdown signal");

    // Wait for Ctrl+C
    tokio::signal::ctrl_c().await?;

    info!("Shutdown signal received, terminating...");

    Ok(())
}
