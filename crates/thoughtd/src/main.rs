//! The Thought daemon.
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

use thoughtd::sync;

use thoughtd::{discovery, logging};

use axum::http::{HeaderValue, Method, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use rmcp::transport::streamable_http_server::session::local::LocalSessionManager;
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use std::sync::Arc;
use thought_mcp::Workspace;

#[cfg(unix)]
async fn shutdown_signal() {
    let Ok(mut terminate) =
        tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
    else {
        let _ = tokio::signal::ctrl_c().await;
        return;
    };
    tokio::select! {
        _ = tokio::signal::ctrl_c() => {}
        _ = terminate.recv() => {}
    }
}

#[cfg(not(unix))]
async fn shutdown_signal() {
    let _ = tokio::signal::ctrl_c().await;
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

    // These must precede `Workspace::open`. The home lock makes discovery
    // single-writer; the store lock also covers two homes aimed at one custom
    // database. Lock files may remain after a crash because ownership is the
    // process-held OS lock, not file presence.
    let _home_lock = discovery::try_lock_home()?.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "another thought daemon already owns this home",
        )
    })?;
    let _store_lock = discovery::try_lock_store(&db_path)?.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "another thought daemon already owns this store",
        )
    })?;

    let workspace = Arc::new(Workspace::open(&db_path)?);
    let token = discovery::random_token()?;
    let instance_id = loop {
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

    let health_instance = instance_id.clone();
    let expected = token.clone();
    let sync_state = sync::SyncState::new(workspace.clone());
    let protected = axum::Router::new()
        .route(
            discovery::MCP_HEALTH_PATH,
            axum::routing::get(move || {
                let instance_id = health_instance.clone();
                async move { axum::Json(discovery::HealthResponse::mcp(&instance_id)) }
            }),
        )
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
                                .find_map(|p| p.strip_prefix("thought.token."))
                        })
                        .map(str::to_string);

                    match bearer.or(subprotocol) {
                        Some(t) if t == expected => Ok::<Response, StatusCode>(next.run(req).await),
                        _ => Err(StatusCode::UNAUTHORIZED),
                    }
                }
            },
        ));

    let public_instance = instance_id.clone();
    let app = axum::Router::new()
        .route(
            discovery::IDENTITY_PATH,
            axum::routing::get(move || {
                let instance_id = public_instance.clone();
                async move { axum::Json(discovery::IdentityResponse::current(&instance_id)) }
            }),
        )
        .merge(protected)
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

    let discovery_path = discovery::write(port, &token, &instance_id, &db_path)?;
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
        .with_graceful_shutdown(shutdown_signal())
        .await;
    tracing::info!("shutting down");
    let _ = std::fs::remove_file(&cleanup);
    result?;
    Ok(())
}
