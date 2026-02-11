use hyper::service::{make_service_fn, service_fn};
use hyper::{Body, Request, Response, Server, StatusCode};
use std::convert::Infallible;
use std::net::SocketAddr;
use tracing::{error, info};

async fn handle_request(_req: Request<Body>) -> Result<Response<Body>, Infallible> {
    Ok(Response::builder()
        .status(StatusCode::OK)
        .body(Body::from("Probe is running"))
        .unwrap())
}

pub async fn start_healthcheck_server(port: u16) -> Result<(), Box<dyn std::error::Error>> {
    let addr = SocketAddr::from(([0, 0, 0, 0], port));

    let make_svc = make_service_fn(|_conn| async { Ok::<_, Infallible>(service_fn(handle_request)) });

    let server = Server::bind(&addr).serve(make_svc);

    info!(
        port = port,
        address = %addr,
        "Health check server started"
    );

    if let Err(e) = server.await {
        error!(
            error = %e,
            "Health check server error"
        );
        return Err(e.into());
    }

    Ok(())
}
