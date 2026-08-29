//! The window process.
//!
//! Starts `thoughtd` if it is not already running and hands the webview its
//! connection details. The daemon is a child now and a launchd agent later
//! (AD-10) — the same standalone binary either way, so the switch costs no
//! code here.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use thoughtd::discovery::{self, Daemon};

#[derive(serde::Serialize)]
struct Connection {
    sync_url: String,
    mcp_url: String,
    token: String,
    /// The stdio shim an MCP client should spawn. Shown in the connections
    /// panel so adding an agent is a copy-paste rather than a scavenger hunt.
    stdio_command: String,
    /// Who the window writes as, so the provenance rails can tell the user's
    /// own blocks from everyone else's without spelling out an id that
    /// `sync.rs` owns.
    actor_id: String,
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
        stdio_command: find_binary("thought-mcp-stdio")
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_else(|| "thought-mcp-stdio".into()),
        actor_id: thoughtd::EDITOR_ACTOR_ID.to_string(),
    }
}

/// Open another window on the same daemon.
///
/// Two windows are two peers, which is the cheapest way to see the CRDT
/// actually working — and, until agents carry presence, the only way to see
/// live carets at all.
#[tauri::command]
fn new_window(app: tauri::AppHandle) -> Result<String, String> {
    let label = format!(
        "window-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
    );
    // The overlay title bar and the hidden title are macOS-only: the builder
    // does not carry those methods at all on other platforms.
    #[allow(unused_mut)]
    let mut builder = tauri::WebviewWindowBuilder::new(&app, &label, tauri::WebviewUrl::default())
        .title("Proof of Thought")
        // Matches tauri.conf.json: the measure is 768px wide, and anything
        // narrower than ~860 pushes the provenance rails on top of the text.
        .inner_size(1040.0, 820.0)
        .min_inner_size(560.0, 400.0);
    #[cfg(target_os = "macos")]
    {
        builder = builder
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true);
    }
    builder.build().map_err(|e| e.to_string())?;
    Ok(label)
}

/// Reuse a running daemon, or start one only when none is published. A stale
/// or unauthenticated record is surfaced for the developer to resolve; this
/// process never signals or silently replaces another possible store owner.
fn ensure_daemon() -> Result<Daemon, String> {
    if let Some(daemon) = discovery::read() {
        if discovery::authenticated_reachable(&daemon) {
            return Ok(daemon);
        }
        return Err(format!(
            "A thought daemon is already published but did not accept its bearer token. Quit any running Proof of Thought or thoughtd process, then remove {} if the problem persists.",
            discovery::discovery_path().display()
        ));
    }

    let thoughtd =
        find_binary("thoughtd").ok_or_else(|| "could not find the thoughtd binary".to_string())?;

    Command::new(&thoughtd)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map_err(|e| format!("could not start thoughtd: {e}"))?;

    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline {
        if let Some(daemon) = discovery::read()
            && discovery::authenticated_reachable(&daemon)
        {
            return Ok(daemon);
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    Err("thoughtd did not become reachable".into())
}

/// Find `thoughtd` or `thought-mcp-stdio`.
///
/// In a bundle they sit beside the app executable as Tauri sidecars. In
/// development they sit there too — Tauri copies the staged sidecar into the
/// dev target — but that copy is only as fresh as the last `stage-sidecars.sh`
/// run, while the workspace build is whatever `cargo` last produced. Preferring
/// the sidecar in dev means running a daemon from whenever sidecars were last
/// staged, which presents as features silently missing from a binary you just
/// rebuilt.
///
/// So: workspace first in debug builds, sidecar first in release.
fn find_binary(name: &str) -> Option<std::path::PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let bundled = exe.with_file_name(name);
    let workspace = exe.with_file_name(format!("../../../../target/debug/{name}"));

    let candidates = if cfg!(debug_assertions) {
        [workspace, bundled]
    } else {
        [bundled, workspace]
    };

    candidates
        .into_iter()
        .find(|p| p.exists())
        .map(|p| p.canonicalize().unwrap_or(p))
}

fn report_startup_error(error: &str) {
    eprintln!("Proof of Thought could not start: {error}");
    #[cfg(any(
        target_os = "macos",
        target_os = "windows",
        target_os = "linux",
        target_os = "dragonfly",
        target_os = "freebsd",
        target_os = "openbsd",
        target_os = "netbsd"
    ))]
    rfd::MessageDialog::new()
        .set_title("Proof of Thought could not start")
        .set_description(error)
        .set_level(rfd::MessageLevel::Error)
        .show();
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    let daemon = match ensure_daemon() {
        Ok(daemon) => daemon,
        Err(error) => {
            report_startup_error(&error);
            return;
        }
    };
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .manage(daemon)
        .invoke_handler(tauri::generate_handler![connection, new_window])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
