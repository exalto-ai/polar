//! Bridges a stdio MCP client to the running daemon (AD-10).
//!
//! MCP clients overwhelmingly speak stdio and spawn their server as a child.
//! Doing that literally would give every client its own `thoughtd`, and several
//! processes writing one SQLite store is a corruption bug waiting to happen. So
//! the real server is HTTP on loopback and this shim is what clients spawn: it
//! finds the daemon, starts one only if none is running, and proxies.

use thoughtd::discovery::{self, Daemon};
use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let daemon = connect()?;

    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    let mut session: Option<String> = None;

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }

        // A notification has no `id` and expects no reply. Writing one anyway
        // desynchronises the client.
        let is_notification = serde_json::from_str::<serde_json::Value>(&line)
            .map(|v| v.get("id").is_none())
            .unwrap_or(false);

        match forward(&daemon, &line, &mut session) {
            Ok(Some(response)) if !is_notification => {
                writeln!(stdout, "{response}")?;
                stdout.flush()?;
            }
            Ok(_) => {}
            Err(e) => {
                // Report transport failure as JSON-RPC rather than dying: a
                // client that loses its server mid-session has no way to tell
                // what happened.
                let id = serde_json::from_str::<serde_json::Value>(&line)
                    .ok()
                    .and_then(|v| v.get("id").cloned())
                    .unwrap_or(serde_json::Value::Null);
                let error = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "error": { "code": -32603, "message": format!("thought daemon unreachable: {e}") }
                });
                writeln!(stdout, "{error}")?;
                stdout.flush()?;
            }
        }
    }
    Ok(())
}

/// Use the published daemon if it answers; otherwise start one.
fn connect() -> Result<Daemon, Box<dyn std::error::Error>> {
    if let Some(daemon) = discovery::read()
        && alive(&daemon)
    {
        return Ok(daemon);
    }
    spawn()
}

/// A stale discovery file outlives the process that wrote it, so ask.
///
/// The question is "is something answering on that port", not "did it like my
/// request". An HTTP error status *is* an answer — rejecting an uninitialized
/// `ping` is exactly what a healthy MCP server should do — so only a transport
/// failure counts as absent. Treating a status code as death made the shim
/// spawn a second daemon every time and then time out waiting for it.
fn alive(daemon: &Daemon) -> bool {
    let sent = ureq::post(&daemon.url)
        .header("Authorization", &format!("Bearer {}", daemon.token))
        .header("Accept", "application/json, text/event-stream")
        .send_json(serde_json::json!({
            "jsonrpc": "2.0", "id": 0, "method": "ping"
        }));
    !matches!(
        sent,
        Err(ureq::Error::ConnectionFailed | ureq::Error::Io(_))
    )
}

fn spawn() -> Result<Daemon, Box<dyn std::error::Error>> {
    // The daemon sits beside this binary; both ship together.
    let exe = std::env::current_exe()?;
    let thoughtd = exe.with_file_name("thoughtd");

    Command::new(&thoughtd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start {}: {e}", thoughtd.display()))?;

    // Poll for a daemon that both published itself and answers, rather than
    // sleeping a guessed interval.
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(daemon) = discovery::read()
            && alive(&daemon)
        {
            return Ok(daemon);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("daemon did not become reachable within 10s".into())
}

/// One JSON-RPC message across, one response back.
///
/// Retries once on a *transport* failure. The shim idles between messages —
/// often for minutes while a person thinks — and an idle keep-alive connection
/// gets closed by the server; the pooled socket then fails on write with
/// ECONNRESET. Retrying an HTTP error status would be wrong, but a connection
/// that died while idle has not delivered anything to retry.
fn forward(
    daemon: &Daemon,
    body: &str,
    session: &mut Option<String>,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    match send_once(daemon, body, session) {
        Err(e) if is_transport_failure(&e) => Ok(send_once(daemon, body, session)?),
        other => Ok(other?),
    }
}

fn is_transport_failure(error: &ureq::Error) -> bool {
    matches!(error, ureq::Error::ConnectionFailed | ureq::Error::Io(_))
}

fn send_once(
    daemon: &Daemon,
    body: &str,
    session: &mut Option<String>,
) -> Result<Option<String>, ureq::Error> {
    let mut request = ureq::post(&daemon.url)
        .header("Authorization", &format!("Bearer {}", daemon.token))
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

    let raw = response.body_mut().read_to_string()?;
    // The streamable-HTTP transport answers as SSE; stdio clients expect one
    // JSON object per line, so unwrap the framing.
    for line in raw.lines() {
        if let Some(payload) = line.strip_prefix("data: ")
            && !payload.trim().is_empty()
        {
            return Ok(Some(payload.to_string()));
        }
    }
    Ok(None)
}
