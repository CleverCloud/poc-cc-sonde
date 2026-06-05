mod agent_scheduler;
mod config;
mod executor;
mod healthcheck_probe;
mod healthcheck_scheduler;
mod persistence;
mod scaling;
mod supervisor;
mod utils;
mod warpscript_probe;
mod warpscript_scheduler;
mod web;

use clap::Parser;
use percent_encoding::{utf8_percent_encode, NON_ALPHANUMERIC};
use std::env;
use std::time::Duration;
use tracing::{debug, info, warn};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};

#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// Configuration file path
    #[arg(long, default_value = "config.toml")]
    config: String,

    /// Enable health check HTTP server
    #[arg(long, default_value_t = false)]
    healthcheck: bool,

    /// Port for health check server (requires --healthcheck)
    #[arg(long, default_value_t = 8080)]
    healthcheck_port: u16,

    /// Host/address to bind the health check server on (requires --healthcheck)
    #[arg(long, default_value = "0.0.0.0", env = "HEALTHCHECK_HOST")]
    healthcheck_host: String,

    /// Dry run mode: probe checks are executed but remediation commands are not
    #[arg(long, default_value_t = false)]
    dry_run: bool,

    /// Multi-instance mode: Redis is required for distributed locking.
    /// Can also be set via the MULTI_INSTANCE environment variable.
    #[arg(long, default_value_t = false, env = "MULTI_INSTANCE")]
    multi_instance: bool,

    /// Maximum time (seconds) to wait for tasks to finish after shutdown signal.
    /// If exceeded, the process exits immediately. Default: 2.
    /// Can also be set via the SHUTDOWN_TIMEOUT environment variable.
    #[arg(long, default_value_t = 2, env = "SHUTDOWN_TIMEOUT")]
    shutdown_timeout: u64,
}

/// Get Redis URL from environment variables
/// Priority: REDIS_URL > (REDIS_HOST + REDIS_PORT + REDIS_PASSWORD)
fn get_redis_url() -> Option<String> {
    // First, try REDIS_URL
    if let Ok(url) = env::var("REDIS_URL") {
        return Some(url);
    }

    // Otherwise, try to build from components
    if let Ok(host) = env::var("REDIS_HOST") {
        let port = env::var("REDIS_PORT").unwrap_or_else(|_| "6379".to_string());
        let password = env::var("REDIS_PASSWORD").ok();

        let url = if let Some(pwd) = password {
            let encoded_pwd = utf8_percent_encode(&pwd, NON_ALPHANUMERIC).to_string();
            format!("redis://:{}@{}:{}", encoded_pwd, host, port)
        } else {
            format!("redis://{}:{}", host, port)
        };

        return Some(url);
    }

    None
}


#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing/logging
    tracing_subscriber::registry()
        .with(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "cc_sonde=warn".into()),
        )
        .with(tracing_subscriber::fmt::layer())
        .init();

    // Parse command line arguments
    let args = Args::parse();

    info!("Starting HTTP monitoring application");

    if args.dry_run {
        warn!("Dry run mode enabled: remediation commands will not be executed");
    }

    if args.multi_instance {
        warn!("Multi-instance mode enabled: Redis is required for distributed locking");
    }

    // Initialize persistence backend first: the active configuration may live in Redis.
    let redis_url = get_redis_url();
    if let Some(ref url) = redis_url {
        let masked_url = utils::sanitize_url_for_log(url);
        info!(redis_url = %masked_url, "Redis configuration detected");
    } else {
        info!("No Redis configuration found, using in-memory persistence");
    }

    let backend = persistence::create_backend(redis_url.clone(), args.multi_instance)
        .await
        .unwrap_or_else(|e| {
            eprintln!("Fatal: Redis connection failed in multi-instance mode: {}", e);
            std::process::exit(1);
        });

    if redis_url.is_some() && !args.multi_instance {
        warn!(
            "Redis is configured but --multi-instance is not set. \
             If running multiple replicas, add --multi-instance (or MULTI_INSTANCE=true) \
             to enable distributed locking."
        );
    }

    // Resolve the active configuration: prefer the one stored in the backend
    // (Redis), otherwise bootstrap from the TOML file and persist it.
    let config = match backend.load_config().await {
        Ok(Some(json)) => {
            info!("Loading configuration from persistence backend");
            config::Config::from_json_str(&json)?
        }
        Ok(None) => {
            info!(config_path = %args.config, "No stored configuration found, bootstrapping from TOML file");
            let config = config::Config::from_file(&args.config)?;
            match config.to_json_string() {
                Ok(json) => {
                    if let Err(e) = backend.save_config(&json).await {
                        warn!(error = %e, "Failed to persist bootstrap configuration");
                    }
                }
                Err(e) => warn!(error = %e, "Failed to serialize bootstrap configuration"),
            }
            config
        }
        Err(e) => {
            warn!(error = %e, "Failed to read configuration from backend, falling back to TOML file");
            config::Config::from_file(&args.config)?
        }
    };

    info!(
        http_probe_count = config.healthcheck_probes.len(),
        warpscript_probe_count = config.warpscript_probes.len(),
        "Configuration loaded successfully"
    );

    // Check WarpScript environment variables if WarpScript probes are configured
    if !config.warpscript_probes.is_empty() {
        // WARP_ENDPOINT is always required
        let endpoint = env::var("WARP_ENDPOINT").map_err(|_| {
            "WARP_ENDPOINT environment variable not set, but WarpScript probes are configured"
        })?;

        debug!(
            warp_endpoint = %utils::sanitize_url_for_log(&endpoint),
            "WarpScript environment configured"
        );
    }

    // Supervisor owns the probe tasks and can hot-reload them from a new config.
    let supervisor = supervisor::Supervisor::new(backend.clone(), args.dry_run, args.multi_instance);
    supervisor.reload(config).await;

    // Web server: hosts the health check endpoint and (when credentials are set)
    // the single-page config UI + its API. Started when --healthcheck is enabled
    // or when the UI is configured via CONFIG_UI_USER / CONFIG_UI_PASSWORD.
    let auth = web::UiAuth::from_env();
    if auth.is_some() {
        info!("Config UI enabled (CONFIG_UI_USER / CONFIG_UI_PASSWORD set)");
    } else {
        info!("Config UI disabled (set CONFIG_UI_USER and CONFIG_UI_PASSWORD to enable it)");
    }

    if args.healthcheck || auth.is_some() {
        info!(host = %args.healthcheck_host, port = args.healthcheck_port, "Starting web server");
        let listener = web::bind(&args.healthcheck_host, args.healthcheck_port)?;
        let state = std::sync::Arc::new(web::WebState {
            backend: backend.clone(),
            supervisor: supervisor.clone(),
            auth,
            agent_apps: supervisor.agent_apps(),
            ingest_token: env::var("AGENT_INGEST_TOKEN").ok().filter(|t| !t.is_empty()),
        });
        tokio::spawn(async move { web::serve(listener, state).await });
    }

    info!("All probe tasks spawned, waiting for shutdown signal");

    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        let mut sigterm = signal(SignalKind::terminate())?;
        tokio::select! {
            _ = tokio::signal::ctrl_c() => {}
            _ = sigterm.recv() => {}
        }
    }
    #[cfg(not(unix))]
    {
        tokio::signal::ctrl_c().await?;
    }

    info!("Shutdown signal received, terminating...");

    // Watchdog : garantit la sortie dans `shutdown_timeout` secondes,
    // même si le runtime Tokio est bloqué par getaddrinfo() ou un connect TCP.
    let watchdog_timeout = args.shutdown_timeout;
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(watchdog_timeout));
        warn!(
            shutdown_timeout = watchdog_timeout,
            "Shutdown timeout reached, forcing exit"
        );
        std::process::exit(1);
    });

    supervisor.abort_all().await;
    info!("All tasks terminated");
    std::process::exit(0);
}
