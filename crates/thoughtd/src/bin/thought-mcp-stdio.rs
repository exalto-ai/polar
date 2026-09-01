//! Bridges one configured reviewer from stdio to the loopback daemon.

use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};
use thoughtd::connections::{CredentialFiles, valid_connection_id};
use thoughtd::discovery::{self, Daemon};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let connection_id = connection_id()?;
    let credential = CredentialFiles::platform()
        .read(&connection_id)
        .map_err(|error| format!("could not load reviewer `{connection_id}`: {error}"))?;
    let daemon = connect()?;

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut session = ProxySession::default();
    let mut keepalive =
        SessionKeepalive::start(daemon.clone(), credential.clone(), keepalive_interval());

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let notification = serde_json::from_str::<serde_json::Value>(&line)
            .map(|value| value.get("id").is_none())
            .unwrap_or(false);
        let result = forward(&daemon, &credential, &line, &mut session);
        keepalive.set_session(session.id.as_deref(), session.protocol_version.as_deref());
        match result {
            Ok(Some(response)) if !notification => {
                writeln!(stdout, "{response}")?;
                stdout.flush()?;
            }
            Ok(_) => {}
            Err(error) => {
                let id = serde_json::from_str::<serde_json::Value>(&line)
                    .ok()
                    .and_then(|value| value.get("id").cloned())
                    .unwrap_or(serde_json::Value::Null);
                writeln!(
                    stdout,
                    "{}",
                    serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": id,
                        "error": {
                            "code": -32603,
                            "message": format!("thought reviewer unavailable: {error}")
                        }
                    })
                )?;
                stdout.flush()?;
            }
        }
    }
    keepalive.stop();
    close_session(
        &daemon,
        &credential,
        session.id.as_deref(),
        session.protocol_version.as_deref(),
    );
    Ok(())
}

fn connection_id() -> Result<String, Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    match (arguments.next(), arguments.next(), arguments.next()) {
        (Some(flag), Some(id), None) if flag == "--connection" && valid_connection_id(&id) => {
            Ok(id)
        }
        _ => Err("expected --connection <id>; copy setup again from Proof of Thought".into()),
    }
}

fn connect() -> Result<Daemon, Box<dyn std::error::Error>> {
    require_published_store_compatibility()?;
    if discovery::discovery_path().exists() {
        discovery::remove_definitively_stale_discovery()
            .map_err(|error| format!("could not remove stale daemon discovery: {error}"))?;
    }
    if let Some(daemon) = discovery::read() {
        if discovery::authenticated_reachable(&daemon) {
            return Ok(daemon);
        }
    } else if discovery::discovery_path().exists() {
        let message = if discovery::read_published().is_some() {
            "the daemon discovery record is invalid or belongs to an older running Proof of Thought daemon; open the current app once to diagnose it or complete its verified upgrade, and keep the discovery record with its store"
                .to_string()
        } else {
            format!(
                "the daemon discovery record is invalid; open Proof of Thought to diagnose it, and keep it with the published store instead of removing either file alone ({})",
                discovery::discovery_path().display()
            )
        };
        return Err(message.into());
    }
    spawn()
}

fn require_published_store_compatibility() -> Result<(), Box<dyn std::error::Error>> {
    let Some(published) = discovery::read_published() else {
        return Ok(());
    };
    if published.store != discovery::default_db_path()
        || !(1..=discovery::PROTOCOL_VERSION).contains(&published.protocol_version)
    {
        return Ok(());
    }
    match thought_store::inspect_compatibility(&published.store)? {
        thought_store::StoreCompatibility::Current => Ok(()),
        thought_store::StoreCompatibility::Missing => Err(format!(
            "the published thought store is missing; Proof of Thought left {} untouched because stopping its daemon could discard an unlinked store",
            discovery::discovery_path().display(),
        )
        .into()),
        thought_store::StoreCompatibility::Unsupported => Err(format!(
            "the published thought store uses an unsupported format; Proof of Thought left {} and {} untouched, and neither file should be removed without an explicit backup or migration",
            published.store.display(),
            discovery::discovery_path().display(),
        )
        .into()),
    }
}

fn spawn() -> Result<Daemon, Box<dyn std::error::Error>> {
    let thoughtd = std::env::current_exe()?.with_file_name("thoughtd");
    Command::new(&thoughtd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| format!("could not start {}: {error}", thoughtd.display()))?;

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(daemon) = discovery::read()
            && discovery::authenticated_reachable(&daemon)
        {
            return Ok(daemon);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("daemon did not become reachable within 10 seconds".into())
}

fn forward(
    daemon: &Daemon,
    credential: &str,
    body: &str,
    session: &mut ProxySession,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    let initialize_request = session.observe(body);
    let response = match send_once(
        daemon,
        credential,
        body,
        session.protocol_version.as_deref(),
        &mut session.id,
    ) {
        Err(error) if is_transport_failure(&error) => send_once(
            daemon,
            credential,
            body,
            session.protocol_version.as_deref(),
            &mut session.id,
        )?,
        Err(ureq::Error::StatusCode(404)) if session.initialize.is_some() => {
            session.reinitialize(daemon, credential)?;
            send_once(
                daemon,
                credential,
                body,
                session.protocol_version.as_deref(),
                &mut session.id,
            )?
        }
        other => other?,
    };
    if initialize_request {
        session.observe_initialize_response(response.as_deref());
    }
    Ok(response)
}

#[derive(Default)]
struct ProxySession {
    id: Option<String>,
    initialize: Option<String>,
    initialized: Option<String>,
    protocol_version: Option<String>,
}

impl ProxySession {
    fn observe(&mut self, body: &str) -> bool {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(body) else {
            return false;
        };
        match value.get("method").and_then(|method| method.as_str()) {
            Some("initialize") => {
                self.initialize = Some(body.to_string());
                self.protocol_version = None;
                true
            }
            Some("notifications/initialized") => {
                self.initialized = Some(body.to_string());
                false
            }
            _ => false,
        }
    }

    fn observe_initialize_response(&mut self, response: Option<&str>) {
        self.protocol_version = response
            .and_then(|response| serde_json::from_str::<serde_json::Value>(response).ok())
            .and_then(|value| {
                value
                    .pointer("/result/protocolVersion")
                    .and_then(|version| version.as_str())
                    .map(str::to_string)
            });
    }

    fn reinitialize(
        &mut self,
        daemon: &Daemon,
        credential: &str,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let initialize = self
            .initialize
            .clone()
            .ok_or("the MCP client did not provide an initialize request")?;
        self.id = None;
        self.protocol_version = None;
        let response = send_once(daemon, credential, &initialize, None, &mut self.id)?;
        self.observe_initialize_response(response.as_deref());
        if let Some(initialized) = self.initialized.as_deref() {
            send_once(
                daemon,
                credential,
                initialized,
                self.protocol_version.as_deref(),
                &mut self.id,
            )?;
        }
        Ok(())
    }
}

const KEEPALIVE_INTERVAL: Duration = Duration::from_secs(60);
const LIFECYCLE_HTTP_TIMEOUT: Duration = Duration::from_secs(2);

#[cfg(debug_assertions)]
fn keepalive_interval() -> Duration {
    std::env::var("THOUGHT_TEST_MCP_KEEPALIVE_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|milliseconds| *milliseconds > 0)
        .map(Duration::from_millis)
        .unwrap_or(KEEPALIVE_INTERVAL)
}

#[cfg(not(debug_assertions))]
fn keepalive_interval() -> Duration {
    KEEPALIVE_INTERVAL
}

#[derive(Clone)]
struct KeepaliveSession {
    id: String,
    protocol_version: String,
}

#[derive(Default)]
struct KeepaliveState {
    session: Option<KeepaliveSession>,
    stopped: bool,
}

/// While the parent MCP client keeps stdio open, standard MCP ping requests
/// distinguish a live but quiet reviewer from an abandoned transport. The
/// daemon keeps its independent five-minute idle cleanup as the crash path.
struct SessionKeepalive {
    shared: Arc<(Mutex<KeepaliveState>, Condvar)>,
    thread: Option<JoinHandle<()>>,
}

impl SessionKeepalive {
    fn start(daemon: Daemon, credential: String, interval: Duration) -> Self {
        let shared = Arc::new((Mutex::new(KeepaliveState::default()), Condvar::new()));
        let thread_shared = shared.clone();
        let thread = std::thread::spawn(move || {
            let agent = lifecycle_agent();
            let mut sequence = 0_u64;
            loop {
                let (state, wake) = &*thread_shared;
                let guard = state
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if guard.stopped {
                    break;
                }
                let (guard, _) = wake
                    .wait_timeout(guard, interval)
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                if guard.stopped {
                    break;
                }
                let session = guard.session.clone();
                drop(guard);

                if let Some(session) = session {
                    sequence = sequence.wrapping_add(1);
                    send_keepalive(&agent, &daemon, &credential, &session, sequence);
                }
            }
        });
        Self {
            shared,
            thread: Some(thread),
        }
    }

    fn set_session(&self, session_id: Option<&str>, protocol_version: Option<&str>) {
        let (state, _) = &*self.shared;
        let mut state = state
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        state.session = session_id
            .zip(protocol_version)
            .map(|(id, version)| KeepaliveSession {
                id: id.to_string(),
                protocol_version: version.to_string(),
            });
    }

    fn stop(&mut self) {
        let (state, wake) = &*self.shared;
        {
            let mut state = state
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            state.stopped = true;
            wake.notify_all();
        }
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

fn lifecycle_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(LIFECYCLE_HTTP_TIMEOUT))
        .build()
        .new_agent()
}

impl Drop for SessionKeepalive {
    fn drop(&mut self) {
        self.stop();
    }
}

fn send_keepalive(
    agent: &ureq::Agent,
    daemon: &Daemon,
    credential: &str,
    session: &KeepaliveSession,
    sequence: u64,
) {
    let body = serde_json::json!({
        "jsonrpc": "2.0",
        "id": format!("thought-stdio-keepalive-{}-{sequence}", std::process::id()),
        "method": "ping"
    })
    .to_string();
    let response = mcp_post_request(
        agent.post(&daemon.url),
        credential,
        Some(&session.protocol_version),
        Some(&session.id),
    )
    .send(&body);
    if let Ok(mut response) = response {
        let _ = response.body_mut().read_to_string();
    }
}

fn is_transport_failure(error: &ureq::Error) -> bool {
    matches!(error, ureq::Error::ConnectionFailed | ureq::Error::Io(_))
}

fn send_once(
    daemon: &Daemon,
    credential: &str,
    body: &str,
    protocol_version: Option<&str>,
    session: &mut Option<String>,
) -> Result<Option<String>, ureq::Error> {
    let request = mcp_post_request(
        ureq::post(&daemon.url),
        credential,
        protocol_version,
        session.as_deref(),
    );

    let mut response = request.send(body)?;
    if session.is_none()
        && let Some(id) = response.headers().get("mcp-session-id")
        && let Ok(id) = id.to_str()
    {
        *session = Some(id.to_string());
    }

    let raw = response.body_mut().read_to_string()?;
    Ok(raw.lines().find_map(|line| {
        line.strip_prefix("data: ")
            .filter(|payload| !payload.trim().is_empty())
            .map(str::to_string)
    }))
}

fn mcp_post_request(
    mut request: ureq::RequestBuilder<ureq::typestate::WithBody>,
    credential: &str,
    protocol_version: Option<&str>,
    session_id: Option<&str>,
) -> ureq::RequestBuilder<ureq::typestate::WithBody> {
    request = request
        .header("Authorization", &format!("Bearer {credential}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if let Some(protocol_version) = protocol_version {
        request = request.header("Mcp-Protocol-Version", protocol_version);
    }
    if let Some(session_id) = session_id {
        request = request.header("Mcp-Session-Id", session_id);
    }
    request
}

/// Normal stdio EOF means the client ended this MCP session. Closing it over
/// streamable HTTP lets the daemon revoke any session-bound direct-edit grant
/// immediately. Failure is intentionally best-effort: an already-stopped or
/// unreachable daemon has no surviving in-memory grant to clean up.
fn close_session(
    daemon: &Daemon,
    credential: &str,
    session_id: Option<&str>,
    protocol_version: Option<&str>,
) {
    let Some(session_id) = session_id else {
        return;
    };
    let agent = lifecycle_agent();
    let mut request = agent
        .delete(&daemon.url)
        .header("Authorization", &format!("Bearer {credential}"))
        .header("Mcp-Session-Id", session_id);
    if let Some(protocol_version) = protocol_version {
        request = request.header("Mcp-Protocol-Version", protocol_version);
    }
    let _ = request.call();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn keepalive_uses_the_negotiated_protocol_and_a_bounded_http_call() {
        let mut session = ProxySession::default();
        assert!(session.observe(
            r#"{"jsonrpc":"2.0","id":1,"method":"initialize","params":{"protocolVersion":"2025-11-25"}}"#,
        ));
        session.observe_initialize_response(Some(
            r#"{"jsonrpc":"2.0","id":1,"result":{"protocolVersion":"2025-06-18"}}"#,
        ));
        assert_eq!(session.protocol_version.as_deref(), Some("2025-06-18"));
        let request = mcp_post_request(
            ureq::post("http://127.0.0.1:1/mcp"),
            "credential",
            session.protocol_version.as_deref(),
            Some("daemon-session"),
        );
        let headers = request.headers_ref().unwrap();
        assert_eq!(headers["mcp-protocol-version"], "2025-06-18");
        assert_eq!(headers["mcp-session-id"], "daemon-session");
        assert_eq!(
            lifecycle_agent().config().timeouts().global,
            Some(LIFECYCLE_HTTP_TIMEOUT)
        );
    }
}
