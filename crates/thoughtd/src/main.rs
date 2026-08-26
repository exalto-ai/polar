//! The Thought daemon.
//!
//! Owns the CRDT documents and the SQLite store, and serves them to agents over
//! MCP. The UI, when it exists, will be one more client of this process rather
//! than the place the documents live (AD-2) — which is what lets an agent edit
//! a document with no window open.
//!
//! Transport is HTTP on loopback, never stdio (AD-10). A stdio MCP server would
//! be spawned per client, and two processes writing one SQLite store is a
//! corruption bug waiting to happen. Clients find the port and capabilities in
//! `daemon.json`; a shim binary bridges stdio clients to it.

mod editor_api;
mod tools;

use thoughtd::sync;

use thoughtd::{discovery, logging};

use axum::http::{HeaderValue, Method, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use std::sync::Arc;
use thought_mcp::Workspace;

fn bearer(req: &Request<axum::body::Body>) -> Option<String> {
    req.headers()
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .map(str::to_string)
}

fn websocket_capability(req: &Request<axum::body::Body>) -> Option<String> {
    req.headers()
        .get("sec-websocket-protocol")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| {
            value
                .split(',')
                .map(str::trim)
                .find_map(|protocol| protocol.strip_prefix("thought.token."))
        })
        .map(str::to_string)
}

async fn mcp_health() -> axum::Json<discovery::HealthResponse> {
    axum::Json(discovery::HealthResponse::mcp())
}

async fn identity() -> axum::Json<discovery::IdentityResponse> {
    axum::Json(discovery::IdentityResponse::current())
}

async fn editor_health() -> axum::Json<discovery::HealthResponse> {
    axum::Json(discovery::HealthResponse::editor())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let db_path = std::env::args()
        .nth(1)
        .map(std::path::PathBuf::from)
        .unwrap_or_else(discovery::default_db_path);

    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    // Held for the process's lifetime; dropping it discards buffered lines.
    let _logs = logging::init(&discovery::home());
    tracing::info!(store = %db_path.display(), "starting");

    // This must precede `Workspace::open`: discovery is published later, so
    // app and stdio clients can race to launch here. Only the process holding
    // this OS lock may ever open the store as its daemon writer.
    let _store_lock = discovery::try_lock_store(&db_path)?.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "another thought daemon already owns this store",
        )
    })?;

    let workspace = Arc::new(Workspace::open(&db_path)?);
    let token = discovery::random_token()?;
    let editor_token = loop {
        let candidate = discovery::random_token()?;
        if candidate != token {
            break candidate;
        }
    };

    let service_workspace = workspace.clone();
    let service = StreamableHttpService::new(
        move || Ok(tools::Thought::new(service_workspace.clone())),
        Arc::new(LocalSessionManager::default()),
        StreamableHttpServerConfig::default()
            .with_max_request_body_bytes(thoughtd::MAX_MCP_REQUEST_BODY_BYTES),
    );

    // MCP and editor sync are separate protocol capabilities. This prevents a
    // client given only MCP access from choosing editor-only Observed source
    // labels. It is not a sandbox against a hostile same-user process that can
    // read the private discovery file or the app's own state.
    let mcp_expected = token.clone();
    let mcp_routes = axum::Router::new()
        .route(discovery::MCP_HEALTH_PATH, axum::routing::get(mcp_health))
        .nest_service("/mcp", service)
        .layer(middleware::from_fn(
            move |req: Request<axum::body::Body>, next: Next| {
                let expected = mcp_expected.clone();
                async move {
                    match bearer(&req) {
                        Some(presented) if presented == expected => {
                            Ok::<Response, StatusCode>(next.run(req).await)
                        }
                        _ => Err(StatusCode::UNAUTHORIZED),
                    }
                }
            },
        ));

    let sync_expected = editor_token.clone();
    let sync_state = sync::SyncState::new(workspace.clone());
    let sync_routes = axum::Router::new()
        .route(
            discovery::EDITOR_HEALTH_PATH,
            axum::routing::get(editor_health),
        )
        // The editor connects here using the framing a future relay can reuse.
        // A relay must still get its own capability and conservative assurance;
        // it must not inherit this editor-only Observed trust.
        .route("/sync", axum::routing::any(sync::handler))
        .with_state(sync_state)
        .merge(editor_api::routes(workspace.clone()))
        .layer(middleware::from_fn(
            move |req: Request<axum::body::Body>, next: Next| {
                let expected = sync_expected.clone();
                async move {
                    // The browser WebSocket API cannot set headers, and a
                    // capability in the URL would reach logs and history. Carry
                    // it as a subprotocol: header-borne, never part of the URL.
                    match bearer(&req).or_else(|| websocket_capability(&req)) {
                        Some(presented) if presented == expected => {
                            Ok::<Response, StatusCode>(next.run(req).await)
                        }
                        _ => Err(StatusCode::UNAUTHORIZED),
                    }
                }
            },
        ));

    let app = axum::Router::new()
        .route(discovery::IDENTITY_PATH, axum::routing::get(identity))
        .merge(mcp_routes)
        .merge(sync_routes)
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

    let discovery_path = discovery::write(port, &token, &editor_token, &db_path)?;
    // The readiness line stays on stderr verbatim: the app and the test harness
    // both wait for it, and a logger's prefixes would change what they match.
    eprintln!("thoughtd listening on http://127.0.0.1:{port}/mcp");
    tracing::info!(
        port,
        discovery = %discovery_path.display(),
        "listening"
    );

    // Remove the discovery file on the way out: a stale one points clients at a
    // port that is either dead or, worse, someone else's.
    let cleanup = discovery_path.clone();
    let result = axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await;
    tracing::info!("shutting down");
    let _ = std::fs::remove_file(&cleanup);
    result?;
    Ok(())
}
