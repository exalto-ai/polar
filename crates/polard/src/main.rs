//! The Polar daemon.
//!
//! Owns the CRDT documents and the SQLite store, and serves them to agents over
//! MCP. The UI, when it exists, will be one more client of this process rather
//! than the place the documents live (AD-2) — which is what lets an agent edit
//! a document with no window open.
//!
//! Transport is HTTP on loopback, never stdio (AD-10). A stdio MCP server would
//! be spawned per client, and two processes writing one SQLite store is a
//! corruption bug waiting to happen. Clients find the port and token in
//! `daemon.json`; a shim binary bridges stdio clients to it.

mod tools;

use polard::discovery;

use axum::http::{Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use polar_mcp::Workspace;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use std::sync::Arc;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(discovery::default_db_path);

    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let workspace = Arc::new(Workspace::open(&db_path)?);
    let token = discovery::random_token()?;

    let service_workspace = workspace.clone();
    let service = StreamableHttpService::new(
        move || Ok(tools::Polar::new(service_workspace.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default(),
    );

    let expected = token.clone();
    let app = axum::Router::new()
        .nest_service("/mcp", service)
        .layer(middleware::from_fn(
            move |req: Request<axum::body::Body>, next: Next| {
                let expected = expected.clone();
                async move {
                    // Any local process can reach a loopback port, and documents are
                    // the user's private writing. Possession of the 0600 discovery
                    // file is the grant.
                    let presented = req
                        .headers()
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.strip_prefix("Bearer "));
                    match presented {
                        Some(t) if t == expected => Ok::<Response, StatusCode>(next.run(req).await),
                        _ => Err(StatusCode::UNAUTHORIZED),
                    }
                }
            },
        ));

    // Port 0: the OS picks a free port, which is then published rather than
    // assumed. A fixed port would collide with anything else on the machine.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await?;
    let port = listener.local_addr()?.port();

    let discovery_path = discovery::write(port, &token, &db_path)?;
    eprintln!("polard listening on http://127.0.0.1:{port}/mcp");
    eprintln!("discovery: {}", discovery_path.display());
    eprintln!("store: {}", db_path.display());

    // Remove the discovery file on the way out: a stale one points clients at a
    // port that is either dead or, worse, someone else's.
    let cleanup = discovery_path.clone();
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    let _ = std::fs::remove_file(&cleanup);
    result?;
    Ok(())
}
