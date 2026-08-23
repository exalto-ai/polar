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
    /// The stdio shim an MCP client should spawn. Shown in the connections
    /// panel so adding an agent is a copy-paste rather than a scavenger hunt.
    stdio_command: String,
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
        stdio_command: find_binary("polar-mcp-stdio")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "polar-mcp-stdio".into()),
    }
}

/// Open another window on the same daemon.
///
/// Two windows are two peers, which is the cheapest way to see the CRDT
/// actually working — and, until agents carry presence, the only way to see
/// live carets at all.
#[tauri::command]
fn new_window(app: tauri::AppHandle) -> Result<String, String> {
    let label = format!("window-{}", std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis())
        .unwrap_or(0));
    tauri::WebviewWindowBuilder::new(&app, &label, tauri::WebviewUrl::default())
        .title("Polar")
        .inner_size(820.0, 720.0)
        .min_inner_size(480.0, 400.0)
        .title_bar_style(tauri::TitleBarStyle::Overlay)
        .hidden_title(true)
        .build()
        .map_err(|e| e.to_string())?;
    Ok(label)
}

/// Reuse a running daemon, or start one. Never assume: a discovery file
/// outlives the process that wrote it.
fn ensure_daemon() -> Result<Daemon, String> {
    if let Some(daemon) = discovery::read()
        && reachable(&daemon)
    {
        return Ok(daemon);
    }

    let polard =
        find_binary("polard").ok_or_else(|| "could not find the polard binary".to_string())?;

    Command::new(&polard)
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

/// Binaries ship beside the app in a build; during `tauri dev` the app lives in
/// app/src-tauri/target while the workspace binaries are in the workspace target.
fn find_binary(name: &str) -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    [
        exe.with_file_name(name),
        exe.with_file_name(format!("../../../../target/debug/{name}")),
    ]
    .into_iter()
    .find(|p| p.exists())
    .map(|p| p.canonicalize().unwrap_or(p))
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
        .invoke_handler(tauri::generate_handler![connection, new_window])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
