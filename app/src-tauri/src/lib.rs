//! The window process.
//!
//! Starts `polard` if it is not already running and hands the webview its
//! connection details. The daemon is a child now and a launchd agent later
//! (AD-10) — the same standalone binary either way, so the switch costs no
//! code here.

use polard::discovery::{self, Daemon};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

#[derive(serde::Serialize)]
struct Connection {
    sync_url: String,
    mcp_url: String,
    token: String,
}

#[tauri::command]
fn connection(state: tauri::State<'_, Daemon>) -> Connection {
    Connection {
        // Same origin, different path: the editor is a sync peer, agents come
        // in over MCP.
        sync_url: state
            .url
            .replace("http://", "ws://")
            .replace("/mcp", "/sync"),
        mcp_url: state.url.clone(),
        token: state.token.clone(),
    }
}

/// Reuse a running daemon, or start one. Never assume: a discovery file
/// outlives the process that wrote it.
fn ensure_daemon() -> Result<Daemon, String> {
    if let Some(daemon) = discovery::read()
        && reachable(&daemon)
    {
        return Ok(daemon);
    }

    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let candidates = [
        exe.with_file_name("polard"),
        // During `tauri dev` the app is built into app/src-tauri/target while
        // the daemon lives in the workspace target.
        exe.with_file_name("../../../../target/debug/polard"),
    ];
    let polard = candidates
        .iter()
        .find(|p| p.exists())
        .ok_or_else(|| "could not find the polard binary".to_string())?;

    Command::new(polard)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start polard: {e}"))?;

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(daemon) = discovery::read()
            && reachable(&daemon)
        {
            return Ok(daemon);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("polard did not become reachable".into())
}

/// An HTTP error status is still an answer. Only a transport failure means
/// nothing is listening — the distinction the stdio shim got wrong first.
fn reachable(daemon: &Daemon) -> bool {
    let sent = ureq::post(&daemon.url)
        .header("Authorization", &format!("Bearer {}", daemon.token))
        .header("Accept", "application/json, text/event-stream")
        .send_json(serde_json::json!({"jsonrpc": "2.0", "id": 0, "method": "ping"}));
    !matches!(sent, Err(ureq::Error::ConnectionFailed | ureq::Error::Io(_)))
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let daemon = ensure_daemon().expect("polar daemon");
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(daemon)
        .invoke_handler(tauri::generate_handler![connection])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
