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

use thoughtd::connections::{
    AuthenticatedPrincipal, ConnectionRegistry, REVIEWER_INSTANCE_HEADER, now_ms,
};
use thoughtd::sessions::LifecycleSessionManager;
use thoughtd::{discovery, logging};

use axum::Extension;
use axum::extract::State;
use axum::http::{HeaderValue, Method, Request, StatusCode};
use axum::middleware::{self, Next};
use axum::response::Response;
use rmcp::transport::streamable_http_server::session::{
    SessionId, SessionManager, local::LocalSessionManager,
};
use rmcp::transport::streamable_http_server::{StreamableHttpServerConfig, StreamableHttpService};
use std::sync::Arc;
use std::time::Duration;
use thought_mcp::Workspace;

const SESSION_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);
const SERVER_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(unix)]
struct ShutdownSignals {
    interrupt: tokio::signal::unix::Signal,
    terminate: tokio::signal::unix::Signal,
}

#[cfg(unix)]
impl ShutdownSignals {
    fn install() -> std::io::Result<Self> {
        Ok(Self {
            interrupt: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::interrupt())?,
            terminate: tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())?,
        })
    }

    async fn wait(&mut self) -> std::io::Result<()> {
        let received = tokio::select! {
            received = self.interrupt.recv() => received,
            received = self.terminate.recv() => received,
        };
        received.ok_or_else(|| std::io::Error::other("shutdown signal listener closed"))
    }
}

#[cfg(windows)]
struct ShutdownSignals {
    interrupt: tokio::signal::windows::CtrlC,
}

#[cfg(windows)]
impl ShutdownSignals {
    fn install() -> std::io::Result<Self> {
        Ok(Self {
            interrupt: tokio::signal::windows::ctrl_c()?,
        })
    }

    async fn wait(&mut self) -> std::io::Result<()> {
        self.interrupt
            .recv()
            .await
            .ok_or_else(|| std::io::Error::other("shutdown signal listener closed"))
    }
}

#[cfg(not(any(unix, windows)))]
struct ShutdownSignals;

#[cfg(not(any(unix, windows)))]
impl ShutdownSignals {
    fn install() -> std::io::Result<Self> {
        Ok(Self)
    }

    async fn wait(&mut self) -> std::io::Result<()> {
        tokio::signal::ctrl_c().await
    }
}

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

async fn reviewer_disconnected(
    State(reviewers): State<Arc<ConnectionRegistry>>,
    Extension(principal): Extension<AuthenticatedPrincipal>,
    headers: axum::http::HeaderMap,
) -> StatusCode {
    let instance_id = reviewer_instance(&headers);
    match reviewers.note_disconnected(&principal, instance_id.as_deref(), now_ms()) {
        Ok(()) => StatusCode::NO_CONTENT,
        Err(_) => StatusCode::FORBIDDEN,
    }
}

fn reviewer_instance(headers: &axum::http::HeaderMap) -> Option<String> {
    headers
        .get(REVIEWER_INSTANCE_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::trim)
        .filter(|value| !value.is_empty() && value.len() <= 128)
        .map(str::to_string)
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Install signal handlers before discovery can be published. A shutdown
    // arriving during startup is then queued for the normal cleanup path.
    let mut shutdown_signals = ShutdownSignals::install()?;
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

    // One publisher owns this Thought home even when a custom store path is
    // used. Stale discovery cleanup takes the same lock before unlinking.
    let _discovery_lock = discovery::try_lock_discovery()?.ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::AlreadyExists,
            "another thought daemon already owns this discovery home",
        )
    })?;

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
    let local_session_manager = Arc::new(LocalSessionManager::default());
    let close_session_manager = local_session_manager.clone();
    let reviewers = Arc::new(
        ConnectionRegistry::platform(workspace.clone()).with_session_closer(move |session_id| {
            let manager = close_session_manager.clone();
            let session_id = SessionId::from(session_id);
            tokio::spawn(async move {
                if let Err(error) = manager.close_session(&session_id).await {
                    tracing::warn!(%session_id, %error, "could not close reviewer MCP session");
                }
            });
        }),
    );
    let weak_reviewers = Arc::downgrade(&reviewers);
    let session_manager = Arc::new(LifecycleSessionManager::new(
        local_session_manager,
        move |session_id| {
            let Some(reviewers) = weak_reviewers.upgrade() else {
                return;
            };
            if let Err(error) = reviewers.forget_session_binding(session_id.as_ref()) {
                tracing::warn!(%session_id, %error, "could not forget closed MCP session");
            }
        },
    ));
    let token = discovery::random_token()?;
    let editor_token = loop {
        let candidate = discovery::random_token()?;
        if candidate != token {
            break candidate;
        }
    };

    let service_workspace = workspace.clone();
    let service_reviewers = reviewers.clone();
    let service = StreamableHttpService::new(
        move || {
            Ok(tools::Thought::new(
                service_workspace.clone(),
                service_reviewers.clone(),
            ))
        },
        session_manager.clone(),
        StreamableHttpServerConfig::default()
            .with_max_request_body_bytes(thoughtd::MAX_MCP_REQUEST_BODY_BYTES),
    );

    // MCP and editor sync are separate protocol capabilities. This prevents a
    // client given only MCP access from choosing editor-only Observed source
    // labels. It is not a sandbox against a hostile same-user process that can
    // read the private discovery file or the app's own state.
    let mcp_expected = token.clone();
    let mcp_reviewers = reviewers.clone();
    let mcp_session_manager = session_manager.clone();
    let mcp_routes = axum::Router::new()
        .route(discovery::MCP_HEALTH_PATH, axum::routing::get(mcp_health))
        .route(
            "/reviewer/status",
            axum::routing::delete(reviewer_disconnected),
        )
        .nest_service("/mcp", service)
        .layer(middleware::from_fn(
            move |mut req: Request<axum::body::Body>, next: Next| {
                let expected = mcp_expected.clone();
                let reviewers = mcp_reviewers.clone();
                let session_manager = mcp_session_manager.clone();
                async move {
                    let instance_id = reviewer_instance(req.headers());
                    let Some(presented) = bearer(&req) else {
                        return Err(StatusCode::UNAUTHORIZED);
                    };
                    let principal = if presented == expected {
                        reviewers.internal_principal()
                    } else {
                        match reviewers.authenticate_reviewer(&presented, now_ms()) {
                            Ok(Some(principal)) => principal,
                            Ok(None) => return Err(StatusCode::UNAUTHORIZED),
                            Err(error) => {
                                tracing::warn!(error = %error, "reviewer authentication failed");
                                return Err(StatusCode::SERVICE_UNAVAILABLE);
                            }
                        }
                    };

                    if let Some(session_id) = req
                        .headers()
                        .get("mcp-session-id")
                        .and_then(|value| value.to_str().ok())
                        && !reviewers
                            .session_matches(
                                session_id,
                                &principal,
                                instance_id.as_deref(),
                                now_ms(),
                            )
                            .unwrap_or(false)
                    {
                        return Err(StatusCode::UNAUTHORIZED);
                    }

                    if let Err(error) =
                        reviewers.note_authenticated(&principal, instance_id.as_deref(), now_ms())
                    {
                        tracing::warn!(error = %error, "reviewer heartbeat failed");
                        return Err(StatusCode::UNAUTHORIZED);
                    }
                    req.extensions_mut().insert(principal.clone());
                    let response = next.run(req).await;
                    if let Some(session_id) = response
                        .headers()
                        .get("mcp-session-id")
                        .and_then(|value| value.to_str().ok())
                        .map(str::to_string)
                    {
                        if !reviewers
                            .bind_session(&session_id, &principal, instance_id.as_deref(), now_ms())
                            .unwrap_or(false)
                        {
                            return Err(StatusCode::UNAUTHORIZED);
                        }
                        // If a worker closed just before its initialize
                        // response reached this layer, its callback ran before
                        // the binding existed. Bind first, then verify the
                        // transport is still live. A later close invokes the
                        // callback after the binding exists.
                        let transport_id = SessionId::from(session_id.as_str());
                        if !session_manager
                            .has_session(&transport_id)
                            .await
                            .unwrap_or(false)
                        {
                            let _ = reviewers.forget_session_binding(&session_id);
                            return Err(StatusCode::UNAUTHORIZED);
                        }
                    }
                    Ok::<Response, StatusCode>(response)
                }
            },
        ))
        .with_state(reviewers.clone());

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
        .merge(editor_api::routes(workspace.clone(), reviewers.clone()))
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
                        HeaderValue::from_static("GET, POST, PATCH, DELETE, OPTIONS"),
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

    let discovery_path = discovery::write(&_discovery_lock, port, &token, &editor_token, &db_path)?;
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
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);
    let server = axum::serve(listener, app).with_graceful_shutdown(async move {
        while !*shutdown_rx.borrow() {
            if shutdown_rx.changed().await.is_err() {
                break;
            }
        }
    });
    let server = async move { server.await };
    tokio::pin!(server);
    let result = tokio::select! {
        result = &mut server => result,
        signal = shutdown_signals.wait() => {
            if let Err(error) = signal {
                tracing::warn!(%error, "could not listen for shutdown signal");
            }
            // Persist the stop state before draining. The server future is not
            // polled during the drain, so it accepts no new sockets, and the
            // watch value cannot lose the notification when polling resumes.
            let _ = shutdown_tx.send(true);
            match tokio::time::timeout(SESSION_DRAIN_TIMEOUT, session_manager.shutdown()).await {
                Ok(closed) => tracing::info!(closed, "drained MCP sessions"),
                Err(_) => tracing::warn!("timed out while draining MCP sessions"),
            }
            match tokio::time::timeout(SERVER_SHUTDOWN_TIMEOUT, &mut server).await {
                Ok(result) => result,
                Err(_) => {
                    tracing::warn!("timed out waiting for HTTP connections to close");
                    Ok(())
                }
            }
        }
    };
    tracing::info!("shutting down");
    let _ = std::fs::remove_file(&cleanup);
    result?;
    Ok(())
}
