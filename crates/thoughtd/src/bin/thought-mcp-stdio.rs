//! Bridges one configured reviewer connection from stdio to the daemon.
//!
//! The setup command carries only a stable connection ID. This process reads
//! the raw reviewer credential from native storage and uses it for loopback
//! requests. It reuses the published daemon or safely starts a replacement for
//! stale discovery. There is no no-argument shared-token fallback.

use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};
use thought_credentials::{CredentialError, CredentialStore};
use thoughtd::connections::REVIEWER_INSTANCE_HEADER;
use thoughtd::discovery::{self, Daemon};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FailureCode {
    Transport,
    Protocol,
    CredentialMissing,
    CredentialStore,
}

impl FailureCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Transport => "transport",
            Self::Protocol => "protocol",
            Self::CredentialMissing => "credential_missing",
            Self::CredentialStore => "credential_store",
        }
    }
}

#[derive(Debug)]
struct ShimFailure {
    code: FailureCode,
    message: String,
}

impl ShimFailure {
    fn new(code: FailureCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for ShimFailure {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for ShimFailure {}

struct ReviewerDaemon {
    daemon: Daemon,
    credential: String,
    instance_id: String,
    failure_reporter: Option<FailureReporter>,
}

#[derive(Clone)]
struct FailureReporter {
    daemon: Daemon,
    connection_id: String,
    instance_id: String,
}

#[derive(Default)]
struct RuntimeFailureState(Mutex<Option<FailureCode>>);

impl RuntimeFailureState {
    fn record(&self, code: FailureCode) -> bool {
        let mut current = self.0.lock().expect("runtime failure lock poisoned");
        let changed = *current != Some(code);
        *current = Some(code);
        changed
    }

    fn clear_after_correlated_response(&self) {
        *self.0.lock().expect("runtime failure lock poisoned") = None;
    }

    fn current(&self) -> Option<FailureCode> {
        *self.0.lock().expect("runtime failure lock poisoned")
    }
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let connection_id = parse_connection_id()?;
    let instance_id = discovery::random_token()
        .map_err(|error| format!("could not create a reviewer process identity: {error}"))?;
    let mut published = discovery::read();
    let failure_daemon = native_failure_daemon(published.as_ref());
    if published.is_none() {
        published = failure_daemon.clone();
    }
    let failure_reporter = failure_daemon
        .as_ref()
        .and_then(|daemon| prepare_failure_reporter(daemon, &connection_id, &instance_id));
    let credential = match CredentialStore::platform(discovery::home()).get(&connection_id) {
        Ok(credential) => credential,
        Err(error) => {
            report_failure(failure_reporter.as_ref(), credential_failure_code(&error));
            return Err(format!(
                "could not load reviewer connection `{connection_id}`: {error}. Reconnect it from Proof of Thought"
            )
            .into());
        }
    };
    let credential = match String::from_utf8(credential) {
        Ok(credential) => credential,
        Err(_) => {
            report_failure(failure_reporter.as_ref(), FailureCode::CredentialStore);
            return Err(
                "the stored reviewer credential is not valid text; reset the connection".into(),
            );
        }
    };
    let daemon = match connect(
        &connection_id,
        credential,
        published,
        instance_id,
        failure_reporter.clone(),
    ) {
        Ok(daemon) => daemon,
        Err(error) => {
            report_failure(failure_reporter.as_ref(), error.code);
            return Err(error.into());
        }
    };
    let runtime_failure = Arc::new(RuntimeFailureState::default());
    let heartbeat = start_heartbeat(&daemon, runtime_failure.clone());

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut session: Option<String> = None;

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        let expected_id = match request_id(&line) {
            Ok(id) => id,
            Err(error) => {
                if runtime_failure.record(error.code) && runtime_failure_is_reportable(error.code) {
                    report_failure(daemon.failure_reporter.as_ref(), error.code);
                }
                let response = unavailable_response(&line, &error);
                writeln!(stdout, "{response}")?;
                stdout.flush()?;
                continue;
            }
        };

        match forward(&daemon, &line, &mut session, expected_id.as_ref()) {
            Ok(Some(response)) => {
                runtime_failure.clear_after_correlated_response();
                writeln!(stdout, "{response}")?;
                stdout.flush()?;
            }
            Ok(None) => {}
            Err(error) => {
                if runtime_failure.record(error.code) && runtime_failure_is_reportable(error.code) {
                    report_failure(daemon.failure_reporter.as_ref(), error.code);
                }
                let response = unavailable_response(&line, &error);
                writeln!(stdout, "{response}")?;
                stdout.flush()?;
            }
        }
    }

    heartbeat.stop();
    if runtime_failure.current().is_none() {
        disconnect(&daemon);
    }
    Ok(())
}

fn parse_connection_id() -> Result<String, Box<dyn std::error::Error>> {
    let mut arguments = std::env::args().skip(1);
    match (arguments.next(), arguments.next(), arguments.next()) {
        (Some(flag), Some(id), None) if flag == "--connection" && valid_connection_id(&id) => {
            Ok(id)
        }
        _ => Err(
            "this launcher now requires `--connection <id>`. Reconnect the reviewer from Proof of Thought and copy its new setup command"
                .into(),
        ),
    }
}

fn valid_connection_id(connection_id: &str) -> bool {
    !connection_id.is_empty()
        && connection_id.len() <= 64
        && connection_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn credential_failure_code(error: &CredentialError) -> FailureCode {
    match error {
        CredentialError::Missing => FailureCode::CredentialMissing,
        CredentialError::Io(error) if error.kind() == std::io::ErrorKind::NotFound => {
            FailureCode::CredentialMissing
        }
        CredentialError::InvalidConnectionId
        | CredentialError::InvalidProviderId
        | CredentialError::Io(_)
        | CredentialError::Platform(_)
        | CredentialError::InvalidStoredCredential => FailureCode::CredentialStore,
    }
}

/// Cold launches still have a durable reviewer row in SQLite. Start the sibling
/// daemon only when discovery is truly absent so the native failure can reach
/// that row. Never replace legacy or incompatible published discovery.
fn native_failure_daemon(published: Option<&Daemon>) -> Option<Daemon> {
    if let Some(daemon) = published {
        return Some(daemon.clone());
    }
    if discovery::discovery_path().exists() {
        return None;
    }
    spawn().ok()
}

/// Reuse a healthy published daemon. Otherwise start a candidate. Lifetime
/// locks decide whether it may replace stale discovery. Actual MCP traffic
/// always uses the reviewer-specific credential.
fn connect(
    connection_id: &str,
    credential: String,
    published: Option<Daemon>,
    instance_id: String,
    failure_reporter: Option<FailureReporter>,
) -> Result<ReviewerDaemon, ShimFailure> {
    let incompatible_discovery = published.is_none() && discovery::discovery_path().exists();
    if let Some(daemon) = published
        && discovery::authenticated_reachable(&daemon)
    {
        let mut reviewer = ReviewerDaemon {
            daemon,
            credential,
            instance_id,
            failure_reporter,
        };
        ensure_reviewer_reachable(&reviewer)?;
        reviewer.failure_reporter =
            failure_reporter_reference(&reviewer.daemon, connection_id, &reviewer.instance_id);
        return Ok(reviewer);
    }

    if incompatible_discovery {
        return Err(ShimFailure::new(
            FailureCode::Protocol,
            format!(
                "the thought daemon discovery record uses an incompatible or invalid format; quit any older Proof of Thought or thoughtd process, then remove {}",
                discovery::discovery_path().display()
            ),
        ));
    }

    let mut reviewer = ReviewerDaemon {
        daemon: spawn()?,
        credential,
        instance_id,
        failure_reporter,
    };
    ensure_reviewer_reachable(&reviewer)?;
    reviewer.failure_reporter =
        failure_reporter_reference(&reviewer.daemon, connection_id, &reviewer.instance_id);
    Ok(reviewer)
}

fn spawn() -> Result<Daemon, ShimFailure> {
    let executable = std::env::current_exe().map_err(|error| {
        ShimFailure::new(
            FailureCode::Transport,
            format!("could not locate the reviewer launcher: {error}"),
        )
    })?;
    let thoughtd = executable.with_file_name("thoughtd");

    Command::new(&thoughtd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|error| {
            ShimFailure::new(
                FailureCode::Transport,
                format!("could not start {}: {error}", thoughtd.display()),
            )
        })?;

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(daemon) = discovery::read()
            && discovery::authenticated_reachable(&daemon)
        {
            return Ok(daemon);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err(ShimFailure::new(
        FailureCode::Transport,
        "daemon did not become reachable within 10 seconds; another process may own the workspace",
    ))
}

fn health_url(daemon: &Daemon) -> Result<String, ShimFailure> {
    daemon
        .url
        .strip_suffix("/mcp")
        .map(|base| format!("{base}{}", discovery::MCP_HEALTH_PATH))
        .ok_or_else(|| {
            ShimFailure::new(
                FailureCode::Protocol,
                "published daemon URL does not end in /mcp",
            )
        })
}

fn ensure_reviewer_reachable(reviewer: &ReviewerDaemon) -> Result<(), ShimFailure> {
    let url = health_url(&reviewer.daemon)?;
    let response = ureq::get(&url)
        .header("Authorization", &format!("Bearer {}", reviewer.credential))
        .header(REVIEWER_INSTANCE_HEADER, &reviewer.instance_id)
        .header("Accept", "application/json")
        .call();
    match response {
        Ok(response) if response.status().as_u16() == 200 => Ok(()),
        Err(ureq::Error::StatusCode(401)) => Err(ShimFailure::new(
            FailureCode::CredentialStore,
            "this reviewer connection is no longer authorized. Reconnect it from Proof of Thought",
        )),
        Err(error) => Err(failure_from_transport(error)),
        Ok(response) => Err(ShimFailure::new(
            FailureCode::Protocol,
            format!(
                "reviewer health check returned HTTP {}",
                response.status().as_u16()
            ),
        )),
    }
}

struct Heartbeat {
    stop: mpsc::Sender<()>,
    thread: std::thread::JoinHandle<()>,
}

impl Heartbeat {
    fn stop(self) {
        let _ = self.stop.send(());
        let _ = self.thread.join();
    }
}

fn start_heartbeat(
    reviewer: &ReviewerDaemon,
    runtime_failure: Arc<RuntimeFailureState>,
) -> Heartbeat {
    let (stop, stopped) = mpsc::channel();
    let daemon = reviewer.daemon.clone();
    let credential = reviewer.credential.clone();
    let instance_id = reviewer.instance_id.clone();
    let failure_reporter = reviewer.failure_reporter.clone();
    let thread = std::thread::spawn(move || {
        let reviewer = ReviewerDaemon {
            daemon,
            credential,
            instance_id,
            failure_reporter,
        };
        loop {
            match stopped.recv_timeout(Duration::from_secs(15)) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {
                    // Once a runtime failure is pending, reviewer-authenticated
                    // health would change the durable row back to connected.
                    // Probe with the daemon's platform bearer instead.
                    // Health alone never clears the pending failure.
                    let result = if runtime_failure.current().is_some() {
                        discovery::authenticated_reachable(&reviewer.daemon)
                            .then_some(())
                            .ok_or(ShimFailure::new(
                                FailureCode::Transport,
                                "the daemon health probe failed",
                            ))
                    } else {
                        ensure_reviewer_reachable(&reviewer)
                    };
                    if let Err(error) = result
                        && runtime_failure.record(error.code)
                        && runtime_failure_is_reportable(error.code)
                    {
                        report_failure(reviewer.failure_reporter.as_ref(), error.code);
                    }
                }
            }
        }
    });
    Heartbeat { stop, thread }
}

fn disconnect(reviewer: &ReviewerDaemon) {
    let Some(base) = reviewer.daemon.url.strip_suffix("/mcp") else {
        return;
    };
    let _ = ureq::delete(format!("{base}/reviewer/status"))
        .header("Authorization", &format!("Bearer {}", reviewer.credential))
        .header(REVIEWER_INSTANCE_HEADER, &reviewer.instance_id)
        .call();
}

/// One JSON-RPC message across, one response back. Transport failures retry
/// once because idle keep-alive sockets can be closed between requests.
fn forward(
    reviewer: &ReviewerDaemon,
    body: &str,
    session: &mut Option<String>,
    expected_id: Option<&serde_json::Value>,
) -> Result<Option<String>, ShimFailure> {
    let raw = match send_once(reviewer, body, session) {
        Err(error) if is_transport_failure(&error) => {
            send_once(reviewer, body, session).map_err(failure_from_transport)
        }
        Err(error) => Err(failure_from_transport(error)),
        Ok(response) => Ok(response),
    }?;
    decode_mcp_response(&raw, expected_id)
}

fn request_id(body: &str) -> Result<Option<serde_json::Value>, ShimFailure> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|_| {
        ShimFailure::new(
            FailureCode::Protocol,
            "the client sent malformed JSON-RPC data",
        )
    })?;
    let object = value.as_object().ok_or_else(|| {
        ShimFailure::new(
            FailureCode::Protocol,
            "the client sent a non-object JSON-RPC message",
        )
    })?;
    if object.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
        return Err(ShimFailure::new(
            FailureCode::Protocol,
            "the client request is not JSON-RPC 2.0",
        ));
    }
    if object
        .get("method")
        .and_then(serde_json::Value::as_str)
        .is_none()
        || object.contains_key("result")
        || object.contains_key("error")
    {
        return Err(ShimFailure::new(
            FailureCode::Protocol,
            "the client sent an invalid JSON-RPC request",
        ));
    }
    let Some(id) = object.get("id") else {
        return Ok(None);
    };
    if !(id.is_string() || id.is_number() || id.is_null()) {
        return Err(ShimFailure::new(
            FailureCode::Protocol,
            "the client request has an invalid JSON-RPC id",
        ));
    }
    Ok(Some(id.clone()))
}

fn is_transport_failure(error: &ureq::Error) -> bool {
    matches!(error, ureq::Error::ConnectionFailed | ureq::Error::Io(_))
}

fn failure_from_transport(error: ureq::Error) -> ShimFailure {
    let code = match &error {
        ureq::Error::ConnectionFailed | ureq::Error::Io(_) => FailureCode::Transport,
        ureq::Error::StatusCode(401) => FailureCode::CredentialStore,
        _ => FailureCode::Protocol,
    };
    ShimFailure::new(
        code,
        format!("could not reach the reviewer connection: {error}"),
    )
}

fn runtime_failure_is_reportable(code: FailureCode) -> bool {
    matches!(code, FailureCode::Transport | FailureCode::Protocol)
}

fn unavailable_response(request: &str, error: &ShimFailure) -> serde_json::Value {
    let id = serde_json::from_str::<serde_json::Value>(request)
        .ok()
        .and_then(|value| value.get("id").cloned())
        .unwrap_or(serde_json::Value::Null);
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": -32603,
            "message": format!("Proof of Thought connection unavailable: {error}")
        }
    })
}

fn failure_report_body(reporter: &FailureReporter, code: FailureCode) -> String {
    serde_json::json!({
        "failure_code": code.as_str(),
        "instance_id": reporter.instance_id,
    })
    .to_string()
}

/// Register the launcher before reading its credential. The daemon identity is
/// verified first, so a forged discovery URL never receives the platform bearer.
fn prepare_failure_reporter(
    daemon: &Daemon,
    connection_id: &str,
    instance_id: &str,
) -> Option<FailureReporter> {
    if !valid_connection_id(connection_id)
        || instance_id.is_empty()
        || !editor_daemon_reachable(daemon)
    {
        return None;
    }
    let base = daemon.url.strip_suffix("/mcp")?;
    let url = format!("{base}/editor/reviewer-connections/{connection_id}/failure-reporter");
    let body = serde_json::json!({ "instance_id": instance_id }).to_string();
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(1)))
        .max_idle_connections(0)
        .build()
        .into();
    let mut response = agent
        .post(url)
        .header("Authorization", &format!("Bearer {}", daemon.token))
        .header("Content-Type", "application/json")
        .send(&body)
        .ok()?;
    let payload: serde_json::Value = response.body_mut().read_json().ok()?;
    if payload.get("credential_version")?.as_i64()? <= 0 {
        return None;
    }
    Some(FailureReporter {
        daemon: daemon.clone(),
        connection_id: connection_id.to_string(),
        instance_id: instance_id.to_string(),
    })
}

fn failure_reporter_reference(
    daemon: &Daemon,
    connection_id: &str,
    instance_id: &str,
) -> Option<FailureReporter> {
    (valid_connection_id(connection_id) && !instance_id.is_empty()).then(|| FailureReporter {
        daemon: daemon.clone(),
        connection_id: connection_id.to_string(),
        instance_id: instance_id.to_string(),
    })
}

fn editor_daemon_reachable(daemon: &Daemon) -> bool {
    discovery::authenticated_reachable(daemon)
}

/// Report only through the short-lived server-side process binding. Re-verify
/// the authenticated daemon before sending the platform bearer, and silently
/// skip a stale or unrelated publisher.
fn report_failure(reporter: Option<&FailureReporter>, code: FailureCode) {
    let Some(reporter) = reporter else {
        return;
    };
    if !editor_daemon_reachable(&reporter.daemon) {
        return;
    }
    let Some(base) = reporter.daemon.url.strip_suffix("/mcp") else {
        return;
    };
    let url = format!(
        "{base}/editor/reviewer-connections/{}/failure",
        reporter.connection_id
    );
    let body = failure_report_body(reporter, code);
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(1)))
        .max_idle_connections(0)
        .build()
        .into();
    let _ = agent
        .post(url)
        .header(
            "Authorization",
            &format!("Bearer {}", reporter.daemon.token),
        )
        .header("Content-Type", "application/json")
        .send(&body);
}

fn send_once(
    reviewer: &ReviewerDaemon,
    body: &str,
    session: &mut Option<String>,
) -> Result<String, ureq::Error> {
    let mut request = ureq::post(&reviewer.daemon.url)
        .header("Authorization", &format!("Bearer {}", reviewer.credential))
        .header(REVIEWER_INSTANCE_HEADER, &reviewer.instance_id)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
    if let Some(id) = session.as_ref() {
        request = request.header("Mcp-Session-Id", id);
    }

    let mut response = request.send(body)?;
    if session.is_none()
        && let Some(id) = response.headers().get("mcp-session-id")
        && let Ok(id) = id.to_str()
    {
        *session = Some(id.to_string());
    }

    response.body_mut().read_to_string()
}

fn decode_mcp_response(
    raw: &str,
    expected_id: Option<&serde_json::Value>,
) -> Result<Option<String>, ShimFailure> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return if expected_id.is_some() {
            Err(ShimFailure::new(
                FailureCode::Protocol,
                "the daemon returned an empty response to a JSON-RPC request",
            ))
        } else {
            Ok(None)
        };
    }

    let sse_payloads = raw
        .lines()
        .filter_map(|line| line.strip_prefix("data:").map(str::trim))
        .filter(|payload| !payload.is_empty())
        .collect::<Vec<_>>();
    let payloads = if sse_payloads.is_empty() {
        vec![trimmed]
    } else {
        sse_payloads
    };

    let mut correlated_response = None;
    for payload in payloads {
        let value: serde_json::Value = serde_json::from_str(payload).map_err(|_| {
            ShimFailure::new(
                FailureCode::Protocol,
                "the daemon returned malformed JSON-RPC data",
            )
        })?;
        let Some(object) = value.as_object() else {
            return Err(ShimFailure::new(
                FailureCode::Protocol,
                "the daemon returned a non-object JSON-RPC message",
            ));
        };
        if object.get("jsonrpc").and_then(serde_json::Value::as_str) != Some("2.0") {
            return Err(ShimFailure::new(
                FailureCode::Protocol,
                "the daemon returned a message without JSON-RPC 2.0 framing",
            ));
        }
        let has_result = object.contains_key("result");
        let has_error = object.contains_key("error");
        if let Some(actual_id) = object.get("id") {
            let Some(expected_id) = expected_id else {
                return Err(ShimFailure::new(
                    FailureCode::Protocol,
                    "the daemon returned a response to a JSON-RPC notification",
                ));
            };
            if actual_id != expected_id {
                return Err(ShimFailure::new(
                    FailureCode::Protocol,
                    "the daemon returned a response for a different JSON-RPC id",
                ));
            }
            if has_result == has_error {
                return Err(ShimFailure::new(
                    FailureCode::Protocol,
                    "the daemon response must contain exactly one of result or error",
                ));
            }
            if correlated_response.replace(value.to_string()).is_some() {
                return Err(ShimFailure::new(
                    FailureCode::Protocol,
                    "the daemon returned more than one correlated JSON-RPC response",
                ));
            }
        } else if has_result
            || has_error
            || object
                .get("method")
                .and_then(serde_json::Value::as_str)
                .is_none()
        {
            return Err(ShimFailure::new(
                FailureCode::Protocol,
                "the daemon returned an invalid JSON-RPC notification",
            ));
        }
    }

    if expected_id.is_some() && correlated_response.is_none() {
        return Err(ShimFailure::new(
            FailureCode::Protocol,
            "the daemon response did not contain a JSON-RPC result",
        ));
    }
    Ok(correlated_response)
}

#[cfg(test)]
mod tests {
    use super::{
        FailureCode, FailureReporter, RuntimeFailureState, decode_mcp_response,
        failure_report_body, request_id, runtime_failure_is_reportable, unavailable_response,
        valid_connection_id,
    };

    #[test]
    fn native_failure_payload_contains_only_non_secret_process_metadata() {
        let old_reviewer_secret = "old-reviewer-secret-sentinel";
        let new_reviewer_secret = "new-reviewer-secret-sentinel";
        let platform_capability = "platform-capability-sentinel";
        let reporter = FailureReporter {
            daemon: thoughtd::discovery::Daemon {
                url: "http://127.0.0.1:1234/mcp".into(),
                protocol_version: thoughtd::discovery::PROTOCOL_VERSION,
                instance_id: "c".repeat(64),
                token: platform_capability.into(),
            },
            connection_id: "reviewer-safe".into(),
            instance_id: "process-safe".into(),
        };
        let body = failure_report_body(&reporter, FailureCode::Transport);
        let value: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(value["failure_code"], "transport");
        assert!(value.get("credential_version").is_none());
        assert_eq!(value["instance_id"], "process-safe");
        for secret in [
            old_reviewer_secret,
            new_reviewer_secret,
            platform_capability,
        ] {
            assert!(!body.contains(secret));
        }
    }

    #[test]
    fn request_requires_a_framed_json_rpc_response() {
        let expected_id = serde_json::json!(1);
        for invalid in ["", "not-json", r#"{"jsonrpc":"2.0","method":"notice"}"#] {
            let error = decode_mcp_response(invalid, Some(&expected_id)).unwrap_err();
            assert_eq!(error.code, FailureCode::Protocol);
        }
        let response = decode_mcp_response(
            "event: message\ndata: {\"jsonrpc\":\"2.0\",\"id\":1,\"result\":{}}\n\n",
            Some(&expected_id),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&response).unwrap()["id"],
            1
        );
    }

    #[test]
    fn empty_request_response_is_reportable_and_returns_json_rpc_error() {
        let failure = decode_mcp_response("", Some(&serde_json::json!(17))).unwrap_err();
        assert_eq!(failure.code, FailureCode::Protocol);
        assert!(runtime_failure_is_reportable(failure.code));

        let response = unavailable_response(
            r#"{"jsonrpc":"2.0","id":17,"method":"tools/list"}"#,
            &failure,
        );
        assert_eq!(response["id"], 17);
        assert_eq!(response["error"]["code"], -32603);
        assert!(
            response["error"]["message"]
                .as_str()
                .unwrap()
                .contains("empty response")
        );
    }

    #[test]
    fn empty_success_remains_valid_for_a_notification() {
        assert_eq!(decode_mcp_response("", None).unwrap(), None);
    }

    #[test]
    fn json_rpc_ids_and_response_shapes_are_strictly_correlated() {
        let numeric_id = serde_json::json!(7);
        for invalid in [
            r#"{"jsonrpc":"1.0","id":7,"result":{}}"#,
            r#"{"jsonrpc":"2.0","id":"7","result":{}}"#,
            r#"{"jsonrpc":"2.0","id":7,"result":{},"error":{}}"#,
            r#"{"jsonrpc":"2.0","id":7}"#,
        ] {
            let error = decode_mcp_response(invalid, Some(&numeric_id)).unwrap_err();
            assert_eq!(error.code, FailureCode::Protocol, "accepted {invalid}");
        }

        let response = decode_mcp_response(
            concat!(
                "data: {\"jsonrpc\":\"2.0\",\"method\":\"notifications/progress\"}\n\n",
                "data: {\"jsonrpc\":\"2.0\",\"id\":7,\"error\":{\"code\":-1,\"message\":\"no\"}}\n\n"
            ),
            Some(&numeric_id),
        )
        .unwrap()
        .unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&response).unwrap()["id"],
            7
        );
    }

    #[test]
    fn outgoing_requests_require_json_rpc_two_and_valid_ids() {
        assert_eq!(
            request_id(r#"{"jsonrpc":"2.0","id":"same","method":"tools/list"}"#).unwrap(),
            Some(serde_json::json!("same"))
        );
        assert_eq!(
            request_id(r#"{"jsonrpc":"2.0","method":"notifications/initialized"}"#).unwrap(),
            None
        );
        for invalid in [
            r#"{"jsonrpc":"1.0","id":1,"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":{},"method":"tools/list"}"#,
            r#"{"jsonrpc":"2.0","id":1,"result":{}}"#,
        ] {
            assert_eq!(request_id(invalid).unwrap_err().code, FailureCode::Protocol);
        }
    }

    #[test]
    fn runtime_failure_survives_health_until_a_correlated_response() {
        let state = RuntimeFailureState::default();
        assert!(state.record(FailureCode::Protocol));
        assert_eq!(state.current(), Some(FailureCode::Protocol));
        assert!(!state.record(FailureCode::Protocol));
        assert_eq!(state.current(), Some(FailureCode::Protocol));
        state.clear_after_correlated_response();
        assert_eq!(state.current(), None);
    }

    #[test]
    fn connection_ids_cannot_escape_the_native_failure_route() {
        assert!(valid_connection_id(
            "reviewer-0123456789abcdef0123456789abcdef0123456789abcdef"
        ));
        for invalid in ["", "../other", "with/slash", "UPPER", "has space"] {
            assert!(!valid_connection_id(invalid), "accepted {invalid:?}");
        }
    }
}
