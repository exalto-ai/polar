//! Bridges one configured reviewer from stdio to the loopback daemon.

use std::io::{BufRead, Write};
use std::process::{Command, Stdio};
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
    let mut session: Option<String> = None;

    for line in stdin.lock().lines() {
        let line = line?;
        if line.trim().is_empty() {
            continue;
        }
        let notification = serde_json::from_str::<serde_json::Value>(&line)
            .map(|value| value.get("id").is_none())
            .unwrap_or(false);
        match forward(&daemon, &credential, &line, &mut session) {
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
    session: &mut Option<String>,
) -> Result<Option<String>, Box<dyn std::error::Error>> {
    match send_once(daemon, credential, body, session) {
        Err(error) if is_transport_failure(&error) => {
            Ok(send_once(daemon, credential, body, session)?)
        }
        other => Ok(other?),
    }
}

fn is_transport_failure(error: &ureq::Error) -> bool {
    matches!(error, ureq::Error::ConnectionFailed | ureq::Error::Io(_))
}

fn send_once(
    daemon: &Daemon,
    credential: &str,
    body: &str,
    session: &mut Option<String>,
) -> Result<Option<String>, ureq::Error> {
    let mut request = ureq::post(&daemon.url)
        .header("Authorization", &format!("Bearer {credential}"))
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
    Ok(raw.lines().find_map(|line| {
        line.strip_prefix("data: ")
            .filter(|payload| !payload.trim().is_empty())
            .map(str::to_string)
    }))
}
