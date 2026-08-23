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

use polard::sync;

use polard::discovery;

use axum::http::{HeaderValue, Method, Request, StatusCode};
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
    let sync_state = sync::SyncState::new(workspace.clone());
    let app = axum::Router::new()
        .nest_service("/mcp", service)
        // The editor connects here and speaks the same protocol the relay will
        // (M2.1): one protocol, two transports.
        .route("/sync", axum::routing::any(sync::handler))
        .with_state(sync_state)
        .layer(middleware::from_fn(
            move |req: Request<axum::body::Body>, next: Next| {
                let expected = expected.clone();
                async move {
                    // Any local process can reach a loopback port, and documents are
                    // the user's private writing. Possession of the 0600 discovery
                    // file is the grant.
                    let headers = req.headers();
                    let bearer = headers
                        .get(axum::http::header::AUTHORIZATION)
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| v.strip_prefix("Bearer "))
                        .map(str::to_string);

                    // The browser WebSocket API cannot set headers, and a token
                    // in the URL would reach logs and history. Carry it as a
                    // subprotocol instead: header-borne, never part of the URL.
                    let subprotocol = headers
                        .get("sec-websocket-protocol")
                        .and_then(|v| v.to_str().ok())
                        .and_then(|v| {
                            v.split(',')
                                .map(str::trim)
                                .find_map(|p| p.strip_prefix("polar.token."))
                        })
                        .map(str::to_string);

                    match bearer.or(subprotocol) {
                        Some(t) if t == expected => Ok::<Response, StatusCode>(next.run(req).await),
                        _ => Err(StatusCode::UNAUTHORIZED),
                    }
                }
            },
        ))
        // CORS, outermost so it runs before auth.
        //
        // The webview is cross-origin whichever way it loads — tauri://localhost
        // in a build, http://localhost:1420 in dev — so without this the window
        // cannot call its own daemon. Preflight must be answered *before* the
        // auth layer, because a preflight request carries no Authorization
        // header and would otherwise be rejected as unauthenticated.
        //
        // Only loopback and the Tauri scheme are echoed back, and every real
        // request still needs the bearer token, so a page on the open web can
        // neither read a response nor authenticate one.
        .layer(middleware::from_fn(
            |req: Request<axum::body::Body>, next: Next| async move {
                let origin = req
                    .headers()
                    .get(axum::http::header::ORIGIN)
                    .and_then(|v| v.to_str().ok())
                    .filter(|o| {
                        *o == "tauri://localhost"
                            || o.starts_with("http://localhost:")
                            || o.starts_with("http://127.0.0.1:")
                    })
                    .map(str::to_string);

                let mut response = if req.method() == Method::OPTIONS {
                    Response::new(axum::body::Body::empty())
                } else {
                    next.run(req).await
                };

                if let Some(origin) = origin
                    && let Ok(value) = HeaderValue::from_str(&origin)
                {
                    let headers = response.headers_mut();
                    headers.insert("access-control-allow-origin", value);
                    headers.insert(
                        "access-control-allow-headers",
                        HeaderValue::from_static("authorization, content-type, mcp-session-id"),
                    );
                    headers.insert(
                        "access-control-allow-methods",
                        HeaderValue::from_static("GET, POST, DELETE, OPTIONS"),
                    );
                    headers.insert(
                        "access-control-expose-headers",
                        HeaderValue::from_static("mcp-session-id"),
                    );
                }
                response
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
