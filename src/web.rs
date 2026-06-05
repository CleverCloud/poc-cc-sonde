use base64::Engine;
use hyper::service::{make_service_fn, service_fn};
use hyper::{header, Body, Method, Request, Response, Server, StatusCode};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::net::{SocketAddr, TcpListener};
use std::sync::Arc;
use tokio::sync::Mutex;
use tracing::{debug, error, info, warn};

use crate::config::Config;
use crate::persistence::{self, MetricSample, PersistenceBackend};
use crate::supervisor::Supervisor;

/// Embedded Vite build of the single-page config UI (see `web/`).
#[derive(rust_embed::RustEmbed)]
#[folder = "web/dist"]
struct Assets;

/// Placeholder returned in place of a stored `warp_token` so secrets never leave
/// the server. A PUT that sends this value back is treated as "leave unchanged".
const TOKEN_MASK: &str = "••••••••";

/// Replace every set `warp_token` with [`TOKEN_MASK`] before sending the config
/// to the browser. Unset tokens stay `null`.
fn mask_tokens(config: &mut Config) {
    for probe in &mut config.warpscript_probes {
        for app in &mut probe.apps {
            if app.warp_token.is_some() {
                app.warp_token = Some(TOKEN_MASK.to_string());
            }
        }
    }
}

/// For each `warp_token` still equal to [`TOKEN_MASK`] in the incoming config,
/// restore the real token from the previously stored config (matched by probe
/// name + app id). A token set to anything else is taken as a genuine change;
/// an absent/empty token clears it (falls back to the `WARP_TOKEN` env var).
fn unmask_tokens(new_config: &mut Config, old: &Config) {
    use std::collections::HashMap;
    let mut old_tokens: HashMap<(&str, &str), &str> = HashMap::new();
    for probe in &old.warpscript_probes {
        for app in &probe.apps {
            if let Some(token) = app.warp_token.as_deref() {
                old_tokens.insert((probe.name.as_str(), app.id.as_str()), token);
            }
        }
    }
    for probe in &mut new_config.warpscript_probes {
        for app in &mut probe.apps {
            if app.warp_token.as_deref() == Some(TOKEN_MASK) {
                app.warp_token = old_tokens
                    .get(&(probe.name.as_str(), app.id.as_str()))
                    .map(|t| t.to_string());
            }
        }
    }
}

/// Basic-Auth credentials for the config UI, sourced from the environment.
#[derive(Clone)]
pub struct UiAuth {
    user: String,
    password: String,
}

impl UiAuth {
    /// Build credentials from `CONFIG_UI_USER` / `CONFIG_UI_PASSWORD`.
    /// Returns `None` (UI disabled) when either variable is missing or empty.
    pub fn from_env() -> Option<Self> {
        let user = std::env::var("CONFIG_UI_USER").ok().filter(|s| !s.is_empty())?;
        let password = std::env::var("CONFIG_UI_PASSWORD").ok().filter(|s| !s.is_empty())?;
        Some(Self { user, password })
    }
}

/// Shared state handed to every request handler.
pub struct WebState {
    pub backend: Arc<dyn PersistenceBackend>,
    pub supervisor: Arc<Supervisor>,
    /// `None` disables the config UI and its API; only `/healthz` and the
    /// metrics ingest endpoint stay available.
    pub auth: Option<UiAuth>,
    /// Apps that currently have an agent probe declared (shared with the supervisor).
    pub agent_apps: Arc<Mutex<HashSet<String>>>,
    /// Optional shared secret required (as `X-Agent-Token`) to push metrics.
    pub ingest_token: Option<String>,
}

/// Body posted by the local agent to `POST /api/metrics/{app_id}`.
#[derive(Deserialize)]
struct MetricsPayload {
    instance_id: String,
    /// Measured values keyed by metric name (e.g. {"cpu": 42.0, "memory": 55.0}).
    metrics: HashMap<String, f64>,
}

pub fn bind(host: &str, port: u16) -> Result<TcpListener, std::io::Error> {
    let addr: SocketAddr = format!("{}:{}", host, port)
        .parse()
        .expect("Invalid web server bind address");
    TcpListener::bind(addr)
}

pub async fn serve(listener: TcpListener, state: Arc<WebState>) {
    let addr = listener.local_addr().unwrap_or_else(|_| "unknown".parse().unwrap());

    let make_svc = make_service_fn(move |_conn| {
        let state = state.clone();
        async move {
            Ok::<_, Infallible>(service_fn(move |req| {
                let state = state.clone();
                async move { Ok::<_, Infallible>(handle_request(req, state).await) }
            }))
        }
    });

    let server = match Server::from_tcp(listener) {
        Ok(builder) => builder.serve(make_svc),
        Err(e) => {
            error!(error = %e, "Failed to create web server from listener");
            return;
        }
    };

    info!(address = %addr, "Config UI / health check server started");

    if let Err(e) = server.await {
        error!(error = %e, "Web server error");
    }
}

async fn handle_request(req: Request<Body>, state: Arc<WebState>) -> Response<Body> {
    let path = req.uri().path().to_string();

    // Liveness endpoint, always available and unauthenticated.
    if path == "/healthz" {
        return text(StatusCode::OK, "Probe is running");
    }

    // Metrics ingestion from local agents. Available without the UI Basic Auth
    // (it is machine-to-machine); guarded instead by an optional shared token.
    // The endpoint only exists for apps that have an agent probe declared.
    if req.method() == Method::POST {
        if let Some(app_id) = path.strip_prefix("/api/metrics/") {
            let app_id = app_id.to_string();
            return ingest_metrics(req, &state, &app_id).await;
        }
    }

    // When the UI is disabled, keep the legacy behaviour: `/` returns the
    // health text, everything else is 404.
    let Some(auth) = state.auth.as_ref() else {
        return if path == "/" {
            text(StatusCode::OK, "Probe is running")
        } else {
            text(StatusCode::NOT_FOUND, "Not found")
        };
    };

    // Everything below requires Basic Auth.
    if !is_authorized(&req, auth) {
        return Response::builder()
            .status(StatusCode::UNAUTHORIZED)
            .header(header::WWW_AUTHENTICATE, "Basic realm=\"poc-sonde config\"")
            .body(Body::from("Unauthorized"))
            .unwrap();
    }

    match (req.method(), path.as_str()) {
        (&Method::GET, "/api/config") => get_config(&state).await,
        (&Method::PUT, "/api/config") => put_config(req, &state).await,
        (&Method::GET, _) => serve_asset(&path),
        _ => text(StatusCode::METHOD_NOT_ALLOWED, "Method not allowed"),
    }
}

/// Validate the `Authorization: Basic ...` header against the configured credentials.
fn is_authorized(req: &Request<Body>, auth: &UiAuth) -> bool {
    let Some(value) = req.headers().get(header::AUTHORIZATION) else {
        return false;
    };
    let Ok(value) = value.to_str() else { return false };
    let Some(b64) = value.strip_prefix("Basic ") else {
        return false;
    };
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(b64.trim()) else {
        return false;
    };
    let Ok(decoded) = String::from_utf8(decoded) else {
        return false;
    };
    let Some((user, password)) = decoded.split_once(':') else {
        return false;
    };
    // Length-leaking but constant-time within equal-length inputs is overkill
    // for a POC admin UI; a plain comparison is acceptable here.
    user == auth.user && password == auth.password
}

async fn get_config(state: &WebState) -> Response<Body> {
    match state.backend.load_config().await {
        Ok(Some(json)) => {
            // Parse, mask secrets, re-serialize so tokens never reach the browser.
            let masked = match Config::from_json_str(&json) {
                Ok(mut config) => {
                    mask_tokens(&mut config);
                    config.to_json_string()
                }
                Err(e) => {
                    error!(error = %e, "Stored config is not valid JSON");
                    return text(StatusCode::INTERNAL_SERVER_ERROR, "Stored configuration is invalid");
                }
            };
            match masked {
                Ok(json) => Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "application/json")
                    .body(Body::from(json))
                    .unwrap(),
                Err(e) => {
                    error!(error = %e, "Failed to serialize masked config");
                    text(StatusCode::INTERNAL_SERVER_ERROR, "Failed to serialize configuration")
                }
            }
        }
        Ok(None) => text(StatusCode::NOT_FOUND, "No configuration stored"),
        Err(e) => {
            error!(error = %e, "Failed to load config from backend");
            text(StatusCode::INTERNAL_SERVER_ERROR, "Failed to load configuration")
        }
    }
}

async fn put_config(req: Request<Body>, state: &WebState) -> Response<Body> {
    let body = match hyper::body::to_bytes(req.into_body()).await {
        Ok(b) => b,
        Err(e) => return text(StatusCode::BAD_REQUEST, &format!("Cannot read body: {}", e)),
    };
    let body_str = match std::str::from_utf8(&body) {
        Ok(s) => s,
        Err(_) => return text(StatusCode::BAD_REQUEST, "Body is not valid UTF-8"),
    };

    // Parse without validating yet: masked tokens must be restored first,
    // otherwise the placeholder would be persisted as a real token.
    let mut config: Config = match serde_json::from_str(body_str) {
        Ok(c) => c,
        Err(e) => return text(StatusCode::BAD_REQUEST, &format!("Invalid configuration: {}", e)),
    };

    // Restore secrets the UI sent back masked, using the currently stored config.
    if let Ok(Some(old_json)) = state.backend.load_config().await {
        if let Ok(old) = Config::from_json_str(&old_json) {
            unmask_tokens(&mut config, &old);
        }
    }

    // Validate the resolved config: rejects malformed configs with a 400 + message.
    if let Err(e) = config.validate() {
        return text(StatusCode::BAD_REQUEST, &format!("Invalid configuration: {}", e));
    }

    // Persist the canonical (re-serialized) form so what we store matches what we run.
    let canonical = match config.to_json_string() {
        Ok(s) => s,
        Err(e) => {
            error!(error = %e, "Failed to serialize validated config");
            return text(StatusCode::INTERNAL_SERVER_ERROR, "Failed to serialize configuration");
        }
    };
    if let Err(e) = state.backend.save_config(&canonical).await {
        error!(error = %e, "Failed to save config to backend");
        return text(StatusCode::INTERNAL_SERVER_ERROR, "Failed to persist configuration");
    }

    // Hot-reload: abort the running probe fleet and respawn from the new config.
    state.supervisor.reload(config).await;
    info!("Configuration updated via web UI and probes reloaded");

    Response::builder()
        .status(StatusCode::OK)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(r#"{"status":"ok","reloaded":true}"#))
        .unwrap()
}

/// Ingest a metric sample pushed by a local agent for `app_id`.
async fn ingest_metrics(req: Request<Body>, state: &WebState, app_id: &str) -> Response<Body> {
    // Optional shared-secret check.
    if let Some(expected) = state.ingest_token.as_deref() {
        let provided = req
            .headers()
            .get("x-agent-token")
            .and_then(|v| v.to_str().ok());
        if provided != Some(expected) {
            return text(StatusCode::UNAUTHORIZED, "Invalid or missing X-Agent-Token");
        }
    }

    // The endpoint only exists for apps with a declared agent probe.
    {
        let apps = state.agent_apps.lock().await;
        if !apps.contains(app_id) {
            return text(
                StatusCode::NOT_FOUND,
                "No agent probe declared for this app id",
            );
        }
    }

    let body = match hyper::body::to_bytes(req.into_body()).await {
        Ok(b) => b,
        Err(e) => return text(StatusCode::BAD_REQUEST, &format!("Cannot read body: {}", e)),
    };
    let payload: MetricsPayload = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => return text(StatusCode::BAD_REQUEST, &format!("Invalid metrics body: {}", e)),
    };

    let sample = MetricSample {
        instance_id: payload.instance_id,
        timestamp_ms: persistence::current_timestamp_ms(),
        values: payload.metrics,
    };

    match state.backend.record_metric_sample(app_id, &sample).await {
        Ok(()) => {
            debug!(app_id = %app_id, instance = %sample.instance_id, "Recorded metric sample");
            Response::builder()
                .status(StatusCode::ACCEPTED)
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"status":"accepted"}"#))
                .unwrap()
        }
        Err(e) => {
            error!(app_id = %app_id, error = %e, "Failed to record metric sample");
            text(StatusCode::INTERNAL_SERVER_ERROR, "Failed to record sample")
        }
    }
}

/// Serve an embedded SPA asset, falling back to `index.html` so client-side
/// routing keeps working on deep links.
fn serve_asset(path: &str) -> Response<Body> {
    let rel = if path == "/" { "index.html" } else { path.trim_start_matches('/') };

    let asset = Assets::get(rel).or_else(|| Assets::get("index.html"));
    match asset {
        Some(content) => {
            let mime = content.metadata.mimetype();
            Response::builder()
                .status(StatusCode::OK)
                .header(header::CONTENT_TYPE, mime)
                .body(Body::from(content.data.into_owned()))
                .unwrap()
        }
        None => {
            warn!("SPA assets not embedded (web/dist empty); run the front-end build");
            text(StatusCode::NOT_FOUND, "UI assets not built")
        }
    }
}

fn text(status: StatusCode, body: &str) -> Response<Body> {
    Response::builder()
        .status(status)
        .header(header::CONTENT_TYPE, "text/plain; charset=utf-8")
        .body(Body::from(body.to_string()))
        .unwrap()
}
