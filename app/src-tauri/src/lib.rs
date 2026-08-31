//! The window process.
//!
//! Starts `thoughtd` if it is not already running and hands the webview its
//! connection details. The daemon is a child now and a launchd agent later
//! (AD-10) — the same standalone binary either way, so the switch costs no
//! code here.

use std::process::{Command, Stdio};
use std::time::{Duration, Instant};
use std::{
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicU64, Ordering},
};
use tauri_plugin_dialog::DialogExt;
use thoughtd::discovery::{self, Daemon};

#[cfg(target_os = "macos")]
mod macos_secure_input;
mod pro_chat;
mod pro_provider;
mod provider_credentials;

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

#[derive(serde::Serialize)]
struct ImportedMarkdown {
    file_name: String,
    markdown: String,
}

#[derive(serde::Serialize)]
struct ExportedMarkdown {
    file_name: String,
}

fn file_name(path: &std::path::Path) -> String {
    path.file_name()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or("Untitled.md")
        .to_string()
}

fn local_path(selected: tauri_plugin_dialog::FilePath) -> Result<std::path::PathBuf, String> {
    selected
        .into_path()
        .map_err(|_| "the selected item is not a local file".to_string())
}

/// Pick and read one UTF-8 Markdown snapshot.
///
/// The path never crosses IPC. The native picker is the only way to choose it,
/// and the size cap stays below MCP's request-body limit after JSON encoding.
#[tauri::command]
async fn import_markdown(app: tauri::AppHandle) -> Result<Option<ImportedMarkdown>, String> {
    let selected = app
        .dialog()
        .file()
        .set_title("Import Markdown")
        .add_filter("Markdown", &["md", "markdown"])
        .blocking_pick_file();
    let Some(selected) = selected else {
        return Ok(None);
    };
    let path = local_path(selected)?;
    let limit = thoughtd::MAX_MARKDOWN_IMPORT_BYTES;
    let metadata =
        std::fs::metadata(&path).map_err(|error| format!("could not inspect file: {error}"))?;
    if metadata.len() > limit as u64 {
        return Err(format!(
            "Imported Markdown must be smaller than {} MiB",
            limit / 1024 / 1024
        ));
    }
    let mut bytes = Vec::with_capacity((metadata.len() as usize).min(limit));
    std::fs::File::open(&path)
        .map_err(|error| format!("could not open file: {error}"))?
        .take((limit + 1) as u64)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("could not read file: {error}"))?;
    if bytes.len() > limit {
        return Err(format!(
            "Imported Markdown must be smaller than {} MiB",
            limit / 1024 / 1024
        ));
    }
    let markdown = String::from_utf8(bytes)
        .map_err(|_| "Imported Markdown must use UTF-8 text".to_string())?
        .trim_start_matches('\u{feff}')
        .to_string();
    Ok(Some(ImportedMarkdown {
        file_name: file_name(&path),
        markdown,
    }))
}

fn serialize_document(document: thought_schema::Node) -> Result<String, String> {
    let document = thought_schema::normalize(&document);
    thought_schema::Schema::v0()
        .validate(&document)
        .map_err(|errors| {
            let details = errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            format!("document cannot be exported as Markdown: {details}")
        })?;
    Ok(thought_markdown::to_markdown(&document))
}

/// Bind a daemon response to the exact wording and formatting in this window.
#[tauri::command]
fn document_wording_revision(document: thought_schema::Node) -> Result<String, String> {
    let document = thought_schema::normalize(&document);
    thought_schema::Schema::v0()
        .validate(&document)
        .map_err(|errors| {
            let details = errors
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join("; ");
            format!("document proof cannot inspect the visible editor tree: {details}")
        })?;
    Ok(thought_markdown::current_wording_revision(&document))
}

fn safe_suggested_name(suggested_name: &str) -> String {
    let cleaned: String = suggested_name
        .chars()
        .map(|character| {
            if character.is_control()
                || matches!(
                    character,
                    '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*'
                )
            {
                '-'
            } else {
                character
            }
        })
        .take(120)
        .collect();
    let cleaned = cleaned
        .trim()
        .trim_matches(|character| matches!(character, '.' | ' ' | '-'));
    let base = if cleaned.is_empty() {
        "Untitled.md"
    } else {
        cleaned
    };
    if base.to_ascii_lowercase().ends_with(".md")
        || base.to_ascii_lowercase().ends_with(".markdown")
    {
        base.to_string()
    } else {
        format!("{base}.md")
    }
}

static EXPORT_TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// Replace a file only after its complete new contents are durable beside it.
/// A failed write leaves the previous Markdown snapshot untouched.
fn atomic_write(path: &std::path::Path, contents: &[u8]) -> std::io::Result<()> {
    let parent = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| std::path::Path::new("."));
    let name = path.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "export path has no file name",
        )
    })?;

    let (temporary, mut file) = loop {
        let nonce = EXPORT_TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temporary = parent.join(format!(
            ".{}.{}.{}.tmp",
            name.to_string_lossy(),
            std::process::id(),
            nonce
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => break (temporary, file),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    };

    let result = (|| {
        if let Ok(metadata) = std::fs::metadata(path) {
            file.set_permissions(metadata.permissions())?;
        }
        file.write_all(contents)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&temporary, path)?;
        std::fs::File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

/// Project the live editor tree and write it to a path chosen by the user.
#[tauri::command]
async fn export_markdown(
    app: tauri::AppHandle,
    document: thought_schema::Node,
    suggested_name: String,
) -> Result<Option<ExportedMarkdown>, String> {
    let markdown = serialize_document(document)?;
    let selected = app
        .dialog()
        .file()
        .set_title("Export Markdown Copy")
        .set_file_name(safe_suggested_name(&suggested_name))
        .add_filter("Markdown", &["md", "markdown"])
        .blocking_save_file();
    let Some(selected) = selected else {
        return Ok(None);
    };
    let mut path = local_path(selected)?;
    if path.extension().is_none() {
        path.set_extension("md");
    }
    atomic_write(&path, markdown.as_bytes())
        .map_err(|error| format!("could not export file: {error}"))?;
    Ok(Some(ExportedMarkdown {
        file_name: file_name(&path),
    }))
}

fn document_window_path(doc_id: Option<&str>) -> Result<Option<String>, String> {
    let Some(doc_id) = doc_id else {
        return Ok(None);
    };
    if doc_id.is_empty()
        || doc_id.len() > 128
        || !doc_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'))
    {
        return Err("invalid document id".to_string());
    }
    Ok(Some(format!("index.html?doc={doc_id}")))
}

const NEW_WINDOW_OFFSET: f64 = 28.0;
static WINDOW_COUNTER: AtomicU64 = AtomicU64::new(0);

fn cascade_axis(
    start: i32,
    extent: u32,
    screen_start: i32,
    screen_extent: u32,
    offset: i32,
) -> i32 {
    let start = i64::from(start);
    let extent = i64::from(extent);
    let screen_start = i64::from(screen_start);
    let screen_end = screen_start + i64::from(screen_extent);
    let offset = i64::from(offset);
    let maximum = (screen_end - extent).max(screen_start);
    let start = start.clamp(screen_start, maximum);
    let after = start.saturating_add(offset);
    let before = start.saturating_sub(offset);
    let chosen = if after <= maximum {
        after
    } else if before >= screen_start {
        before
    } else {
        start
    };
    i32::try_from(chosen).unwrap_or(start as i32)
}

fn cascaded_position(
    source: &tauri::WebviewWindow,
    target_size: tauri::PhysicalSize<u32>,
) -> Option<tauri::PhysicalPosition<i32>> {
    let scale = source.scale_factor().ok()?;
    let position = source.outer_position().ok()?;
    let offset = (NEW_WINDOW_OFFSET * scale).round() as i32;

    let (x, y) = if let Ok(Some(monitor)) = source.current_monitor() {
        let work_area = monitor.work_area();
        (
            cascade_axis(
                position.x,
                target_size.width,
                work_area.position.x,
                work_area.size.width,
                offset,
            ),
            cascade_axis(
                position.y,
                target_size.height,
                work_area.position.y,
                work_area.size.height,
                offset,
            ),
        )
    } else {
        (
            position.x.saturating_add(offset),
            position.y.saturating_add(offset),
        )
    };

    Some(tauri::PhysicalPosition::new(x, y))
}

/// Open another window on the same daemon, optionally pinned to one document.
///
/// Passing a document id is how New Document opens a genuinely separate
/// document without replacing the editor in the current window. Omitting it
/// preserves the ordinary peer-window behavior.
#[tauri::command]
fn new_window(
    app: tauri::AppHandle,
    window: tauri::WebviewWindow,
    doc_id: Option<String>,
) -> Result<String, String> {
    let label = format!(
        "window-{}-{}",
        std::process::id(),
        WINDOW_COUNTER.fetch_add(1, Ordering::Relaxed)
    );
    let url = match document_window_path(doc_id.as_deref())? {
        Some(path) => tauri::WebviewUrl::App(path.into()),
        None => tauri::WebviewUrl::default(),
    };
    // The overlay title bar and the hidden title are macOS-only: the builder
    // does not carry those methods at all on other platforms.
    #[allow(unused_mut)]
    let mut builder = tauri::WebviewWindowBuilder::new(&app, &label, url)
        .title("Proof of Thought")
        // Matches tauri.conf.json: the measure is 768px wide, and anything
        // narrower than ~860 pushes the provenance rails on top of the text.
        .inner_size(1040.0, 820.0)
        .min_inner_size(560.0, 400.0)
        // Position the fully constructed native window using its real outer
        // dimensions, then reveal it. This avoids a centered-window flash and
        // keeps a default-sized child on screen even when its parent is narrow.
        .visible(false);
    #[cfg(target_os = "macos")]
    {
        builder = builder
            .title_bar_style(tauri::TitleBarStyle::Overlay)
            .hidden_title(true);
    }
    let child = builder.build().map_err(|e| e.to_string())?;
    if let Ok(target_size) = child.outer_size()
        && let Some(position) = cascaded_position(&window, target_size)
    {
        // Positioning is a visual enhancement. If the window manager refuses
        // it, still show the new document instead of stranding it invisibly.
        let _ = child.set_position(position);
    }
    child.show().map_err(|e| e.to_string())?;
    child.set_focus().map_err(|e| e.to_string())?;
    Ok(label)
}

fn replaceable_prior_protocol(protocol_version: u32) -> bool {
    (1..discovery::PROTOCOL_VERSION).contains(&protocol_version)
}

fn replaceable_prior_daemon_at(
    published: &discovery::PublishedDaemon,
    expected_store: &Path,
) -> bool {
    replaceable_prior_protocol(published.protocol_version) && published.store == expected_store
}

fn replaceable_prior_daemon(published: &discovery::PublishedDaemon) -> bool {
    replaceable_prior_daemon_at(published, &discovery::default_db_path())
}

fn published_store_is_upgradeable(
    published: &discovery::PublishedDaemon,
    expected_store: &Path,
    discovery_path: &Path,
) -> Result<bool, String> {
    if published.store != expected_store {
        return Ok(false);
    }
    match published.store.try_exists() {
        Ok(true) => {}
        Ok(false) => {
            return Err(format!(
                "The published thought store is missing. Proof of Thought left the running daemon and {} untouched because stopping it could discard an unlinked store.",
                discovery_path.display(),
            ));
        }
        Err(error) => {
            return Err(format!(
                "Proof of Thought could not verify the published store path without changing it, so it left the daemon and {} untouched: {error}",
                discovery_path.display(),
            ));
        }
    }
    match thought_store::inspect_compatibility(&published.store) {
        Ok(thought_store::StoreCompatibility::Current) => Ok(true),
        Ok(thought_store::StoreCompatibility::Missing) => Err(format!(
            "The published thought store disappeared while Proof of Thought was checking it. The daemon and {} were left untouched.",
            discovery_path.display(),
        )),
        Ok(thought_store::StoreCompatibility::Unsupported) => Err(format!(
            "The published thought store uses a format this build cannot safely upgrade. Proof of Thought left the daemon and both files untouched. Do not remove {} by itself. Install a build with a supported migration, or back up and remove both {} and {} only if you intend to discard this test data.",
            discovery_path.display(),
            published.store.display(),
            discovery_path.display(),
        )),
        Err(error) => Err(format!(
            "Proof of Thought could not inspect the published store without changing it, so it left the daemon and both files untouched: {error}. Keep {} and {} together until the store can be inspected safely.",
            published.store.display(),
            discovery_path.display(),
        )),
    }
}

#[cfg(unix)]
fn interrupt_process(pid: u32) -> std::io::Result<()> {
    let pid = libc::pid_t::try_from(pid)
        .map_err(|_| std::io::Error::new(std::io::ErrorKind::InvalidInput, "invalid daemon PID"))?;
    if unsafe { libc::kill(pid, libc::SIGINT) } == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(not(unix))]
fn interrupt_process(_: u32) -> std::io::Result<()> {
    Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "verified daemon replacement is unavailable on this platform",
    ))
}

#[cfg(unix)]
fn process_already_gone(error: &std::io::Error) -> bool {
    error.raw_os_error() == Some(libc::ESRCH)
}

#[cfg(not(unix))]
fn process_already_gone(_: &std::io::Error) -> bool {
    false
}

/// Stop a known predecessor only after the unchanged record, exact public
/// identity, listener owner, and bundled executable all agree twice. A stale
/// or reused PID alone is never authority to signal a process.
fn retire_verified_prior_daemon(
    published: &discovery::PublishedDaemon,
    expected_executable: &Path,
) -> Result<bool, String> {
    retire_verified_prior_daemon_at(
        published,
        expected_executable,
        &discovery::discovery_path(),
        &discovery::default_db_path(),
    )
}

fn retire_verified_prior_daemon_at(
    published: &discovery::PublishedDaemon,
    expected_executable: &Path,
    discovery_path: &Path,
    expected_store: &Path,
) -> Result<bool, String> {
    if !replaceable_prior_daemon_at(published, expected_store) {
        return Ok(false);
    }

    let verified = || -> Result<bool, String> {
        Ok(
            published_store_is_upgradeable(published, expected_store, discovery_path)?
                && discovery::published_record_unchanged_at(discovery_path, published)
                && discovery::published_identity_reachable(published)
                && discovery::process_owns_published_listener(published.pid, &published.url)
                && discovery::process_is_expected_daemon(published.pid, expected_executable),
        )
    };
    if !verified()? {
        return Ok(false);
    }

    // Repeat every observation at the signal boundary to narrow PID reuse and
    // discovery replacement races. Any disagreement leaves the process alone.
    if !verified()? {
        return Ok(false);
    }
    if let Err(error) = interrupt_process(published.pid)
        && !process_already_gone(&error)
    {
        return Err(format!(
            "could not stop the verified prior thought daemon: {error}"
        ));
    }

    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let endpoint_gone = !discovery::published_identity_reachable(published);
        let process_gone = discovery::published_process_definitively_absent(published.pid);
        if endpoint_gone && process_gone {
            if !discovery_path.exists()
                || !discovery::published_record_unchanged_at(discovery_path, published)
            {
                return Ok(true);
            }
            if discovery::remove_published_if_definitively_stale_at(discovery_path, published)
                .map_err(|error| format!("could not remove retired daemon discovery: {error}"))?
            {
                return Ok(true);
            }
        }
        std::thread::sleep(Duration::from_millis(25));
    }

    Err("the verified prior thought daemon did not stop within 5 seconds".into())
}

/// Reuse a current daemon, clean a conclusively dead known record, or retire a
/// fully verified predecessor during an app upgrade. Malformed, unknown, and
/// future publishers remain untouched and fail closed.
fn ensure_daemon() -> Result<Daemon, String> {
    let thoughtd =
        find_binary("thoughtd").ok_or_else(|| "could not find the thoughtd binary".to_string())?;
    let discovery_path = discovery::discovery_path();
    let default_store = discovery::default_db_path();

    // Store compatibility is part of discovery authority. Check it before a
    // dead-row cleanup as well as at both live-process signal boundaries, so
    // removing discovery can never bypass a fail-closed schema decision.
    if let Some(published) = discovery::read_published()
        && (1..=discovery::PROTOCOL_VERSION).contains(&published.protocol_version)
        && published.store == default_store
    {
        published_store_is_upgradeable(&published, &default_store, &discovery_path)?;
    }

    if discovery::discovery_path().exists()
        && discovery::remove_definitively_stale_discovery()
            .map_err(|error| format!("could not remove stale daemon discovery: {error}"))?
    {
        return ensure_daemon();
    }

    if discovery::read().is_some() {
        let deadline = Instant::now() + Duration::from_secs(2);
        while let Some(daemon) = discovery::read() {
            if discovery::authenticated_reachable(&daemon) {
                return Ok(daemon);
            }
            if Instant::now() >= deadline {
                return Err(format!(
                    "A thought daemon is already published but did not accept its discovery capability. Quit any running Proof of Thought or thoughtd process, then remove {} if the problem persists.",
                    discovery_path.display()
                ));
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    if discovery_path.exists() {
        let mut retirement_error = None;
        if let Some(published) = discovery::read_published()
            && replaceable_prior_daemon(&published)
        {
            match retire_verified_prior_daemon(&published, &thoughtd) {
                Ok(true) => return ensure_daemon(),
                Ok(false) => {}
                Err(error) => retirement_error = Some(error),
            }
        }

        // Another app may have completed retirement and atomically published
        // the current daemon while this launch was classifying the old row.
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            if let Some(daemon) = discovery::read()
                && discovery::authenticated_reachable(&daemon)
            {
                return Ok(daemon);
            }
            if !discovery_path.exists() {
                return ensure_daemon();
            }
            if Instant::now() >= deadline {
                break;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
        if let Some(error) = retirement_error {
            return Err(error);
        }
        return Err(format!(
            "The thought daemon discovery record is malformed, unknown, or could not be safely verified for automatic upgrade. Quit any running Proof of Thought or thoughtd process, then reopen the current app. If the problem persists, keep {} and its database together and use a backed-up app-data reset. Do not remove the discovery record by itself.",
            discovery_path.display()
        ));
    }

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
    Err(
        "thoughtd did not become reachable within 10 seconds; another process may own the workspace"
            .into(),
    )
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
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(daemon)
        .invoke_handler(tauri::generate_handler![
            connection,
            new_window,
            import_markdown,
            export_markdown,
            document_wording_revision,
            pro_chat::provider_models,
            pro_chat::send_provider_chat,
            pro_provider::provider_configurations,
            pro_provider::configure_provider_key,
            pro_provider::remove_provider_key
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}

#[cfg(test)]
mod tests {
    use super::{
        atomic_write, cascade_axis, document_window_path, document_wording_revision,
        replaceable_prior_daemon, replaceable_prior_protocol, retire_verified_prior_daemon_at,
        safe_suggested_name, serialize_document,
    };
    use std::io::{Read as _, Write as _};
    use thought_schema::{Mark, Node};

    const PRIOR_DAEMON_CHILD: &str = "THOUGHT_PRIOR_DAEMON_CHILD";
    const PRIOR_DISCOVERY_PATH: &str = "THOUGHT_PRIOR_DISCOVERY_PATH";

    #[cfg(all(unix, any(target_os = "macos", target_os = "linux")))]
    struct PriorDaemonGuard {
        child: Option<std::process::Child>,
    }

    #[cfg(all(unix, any(target_os = "macos", target_os = "linux")))]
    impl PriorDaemonGuard {
        fn stop(mut self) {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    #[cfg(all(unix, any(target_os = "macos", target_os = "linux")))]
    impl Drop for PriorDaemonGuard {
        fn drop(&mut self) {
            if let Some(mut child) = self.child.take() {
                let _ = child.kill();
                let _ = child.wait();
            }
        }
    }

    #[cfg(all(unix, any(target_os = "macos", target_os = "linux")))]
    fn spawn_prior_daemon(
        directory: &std::path::Path,
        discovery_path: &std::path::Path,
    ) -> (
        PriorDaemonGuard,
        thoughtd::discovery::PublishedDaemon,
        std::path::PathBuf,
    ) {
        let executable = std::env::current_exe().unwrap();
        let child = std::process::Command::new(&executable)
            .args(["--exact", "tests::prior_daemon_child", "--nocapture"])
            .env(PRIOR_DAEMON_CHILD, "1")
            .env(PRIOR_DISCOVERY_PATH, discovery_path)
            .env("THOUGHT_HOME", directory)
            .spawn()
            .unwrap();
        let pid = child.id();
        let guard = PriorDaemonGuard { child: Some(child) };

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let published = loop {
            if let Some(published) = thoughtd::discovery::read_published_at(discovery_path) {
                break published;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "prior daemon did not publish discovery"
            );
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(published.pid, pid);
        assert_eq!(published.protocol_version, 9);
        (guard, published, executable)
    }

    #[cfg(all(unix, any(target_os = "macos", target_os = "linux")))]
    fn create_version_six_store(path: &std::path::Path) {
        drop(thought_store::Store::open(path).unwrap());
        let mut database = std::fs::read(path).unwrap();
        assert!(database.starts_with(b"SQLite format 3\0"));
        database[60..64].copy_from_slice(&6_u32.to_be_bytes());
        std::fs::write(path, database).unwrap();
        assert_eq!(
            thought_store::inspect_compatibility(path).unwrap(),
            thought_store::StoreCompatibility::Unsupported
        );
    }

    #[test]
    fn prior_daemon_child() {
        if std::env::var_os(PRIOR_DAEMON_CHILD).is_none() {
            return;
        }
        let discovery = std::path::PathBuf::from(
            std::env::var_os(PRIOR_DISCOVERY_PATH).expect("prior discovery path"),
        );
        let store = discovery.with_file_name("thought.db");
        let _home_lock = thoughtd::discovery::try_lock_home()
            .unwrap()
            .expect("prior daemon owns its home");
        let _store_lock = thoughtd::discovery::try_lock_store(&store)
            .unwrap()
            .expect("prior daemon owns its store");
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let body = serde_json::json!({
            "protocol_version": 9,
            "pid": std::process::id(),
            "port": port,
            "url": format!("http://127.0.0.1:{port}/mcp"),
            "store": store,
            "token": "old-mcp",
            "editor_token": "old-editor",
            "provider_token": "old-provider"
        });
        std::fs::write(&discovery, serde_json::to_vec(&body).unwrap()).unwrap();

        loop {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = Vec::new();
            let mut chunk = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut chunk).unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&chunk[..read]);
            }
            let response = serde_json::json!({
                "service": "ai.exalto.thoughtd",
                "protocol_version": 9
            })
            .to_string();
            write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
                response.len(),
                response
            )
            .unwrap();
        }
    }

    #[test]
    fn export_projection_preserves_title_and_font_size_metadata() {
        let document = Node::element(
            "doc",
            vec![
                Node::element(
                    "heading",
                    vec![Node::text(
                        "Plan",
                        vec![Mark::new("fontSize").with_attr("size", "24px".into())],
                    )],
                )
                .with_attr("level", 1.into())
                .with_attr("variant", "title".into()),
            ],
        );

        assert_eq!(
            serialize_document(document).unwrap(),
            "<!--thought:title-->\n# <span style=\"font-size: 24px\">Plan</span>"
        );
    }

    #[test]
    fn export_refuses_an_invalid_editor_tree() {
        let invalid = Node::element("doc", vec![Node::element("mystery", vec![])]);
        assert!(serialize_document(invalid.clone()).is_err());
        assert!(document_wording_revision(invalid).is_err());
    }

    #[test]
    fn proof_wording_revision_uses_the_shared_markdown_projection() {
        let document = Node::element(
            "doc",
            vec![Node::element(
                "paragraph",
                vec![Node::text("One careful sentence.", vec![])],
            )],
        );

        assert_eq!(
            document_wording_revision(document.clone()).unwrap(),
            thought_markdown::current_wording_revision(&document)
        );
    }

    #[test]
    fn suggested_names_are_local_and_keep_a_markdown_extension() {
        assert_eq!(safe_suggested_name("Plan: Q4"), "Plan- Q4.md");
        assert_eq!(safe_suggested_name("notes.markdown"), "notes.markdown");
        assert_eq!(safe_suggested_name("../../"), "Untitled.md");
    }

    #[test]
    fn document_windows_receive_only_safe_document_ids() {
        assert_eq!(
            document_window_path(Some("916e52c1-56b9-4fc3-a76f-47a72521e458")).unwrap(),
            Some("index.html?doc=916e52c1-56b9-4fc3-a76f-47a72521e458".to_string())
        );
        assert_eq!(document_window_path(None).unwrap(), None);
        assert!(document_window_path(Some("../../settings")).is_err());
        assert!(document_window_path(Some("doc?id=other")).is_err());
    }

    #[test]
    fn new_windows_cascade_but_remain_on_screen() {
        assert_eq!(cascade_axis(100, 500, 0, 1200, 28), 128);
        assert_eq!(cascade_axis(690, 500, 0, 1200, 28), 662);
        assert_eq!(cascade_axis(-1400, 900, -1440, 1440, 28), -1372);
        assert_eq!(cascade_axis(0, 1400, 0, 1200, 28), 0);
        assert_eq!(cascade_axis(1100, 500, 0, 1200, 28), 672);
        assert_eq!(cascade_axis(-300, 500, 0, 1200, 28), 28);
    }

    #[test]
    fn markdown_exports_replace_existing_files_atomically() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("Notes.md");
        std::fs::write(&path, "previous contents").unwrap();

        atomic_write(&path, b"complete new contents").unwrap();

        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "complete new contents"
        );
        assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 1);
    }

    #[test]
    fn only_known_predecessor_protocols_are_replaceable() {
        assert!(replaceable_prior_protocol(1));
        assert!(replaceable_prior_protocol(2));
        assert!(replaceable_prior_protocol(9));
        assert!(!replaceable_prior_protocol(0));
        assert!(!replaceable_prior_protocol(
            thoughtd::discovery::PROTOCOL_VERSION
        ));
        assert!(!replaceable_prior_protocol(
            thoughtd::discovery::PROTOCOL_VERSION + 1
        ));
    }

    #[test]
    fn custom_store_predecessor_is_not_automatically_replaced() {
        let directory = tempfile::tempdir().unwrap();
        let discovery_path = directory.path().join("daemon.json");
        let custom_store = directory.path().join("custom.db");
        let body = serde_json::json!({
            "protocol_version": 9,
            "pid": std::process::id(),
            "port": 4567,
            "url": "http://127.0.0.1:4567/mcp",
            "store": custom_store,
            "token": "old-mcp",
            "editor_token": "old-editor",
            "provider_token": "old-provider"
        });
        std::fs::write(&discovery_path, serde_json::to_vec(&body).unwrap()).unwrap();
        let published = thoughtd::discovery::read_published_at(&discovery_path).unwrap();

        assert!(!replaceable_prior_daemon(&published));
    }

    #[cfg(all(unix, any(target_os = "macos", target_os = "linux")))]
    #[test]
    fn protocol_nine_with_version_six_store_is_not_signalled_or_modified() {
        let directory = tempfile::tempdir().unwrap();
        let discovery_path = directory.path().join("daemon.json");
        let store_path = directory.path().join("thought.db");
        create_version_six_store(&store_path);
        let store_before = std::fs::read(&store_path).unwrap();
        let (child, published, executable) = spawn_prior_daemon(directory.path(), &discovery_path);
        let discovery_before = std::fs::read(&discovery_path).unwrap();

        let result =
            retire_verified_prior_daemon_at(&published, &executable, &discovery_path, &store_path);

        assert!(
            result.is_err(),
            "unsupported store unexpectedly retired: {result:?}"
        );
        assert!(thoughtd::discovery::process_is_expected_daemon(
            published.pid,
            &executable
        ));
        assert!(thoughtd::discovery::published_identity_reachable(
            &published
        ));
        assert_eq!(std::fs::read(&discovery_path).unwrap(), discovery_before);
        assert_eq!(std::fs::read(&store_path).unwrap(), store_before);

        child.stop();
    }

    #[cfg(all(unix, any(target_os = "macos", target_os = "linux")))]
    #[test]
    fn protocol_nine_with_missing_store_is_not_signalled_or_recreated() {
        let directory = tempfile::tempdir().unwrap();
        let discovery_path = directory.path().join("daemon.json");
        let store_path = directory.path().join("thought.db");
        let (child, published, executable) = spawn_prior_daemon(directory.path(), &discovery_path);
        assert!(!store_path.exists());
        let discovery_before = std::fs::read(&discovery_path).unwrap();

        let result =
            retire_verified_prior_daemon_at(&published, &executable, &discovery_path, &store_path);

        assert!(
            result.is_err(),
            "missing store unexpectedly retired: {result:?}"
        );
        assert!(thoughtd::discovery::process_is_expected_daemon(
            published.pid,
            &executable
        ));
        assert!(thoughtd::discovery::published_identity_reachable(
            &published
        ));
        assert_eq!(std::fs::read(&discovery_path).unwrap(), discovery_before);
        assert!(!store_path.exists());

        child.stop();
    }

    #[cfg(all(unix, any(target_os = "macos", target_os = "linux")))]
    #[test]
    fn verified_protocol_nine_daemon_is_retired_without_touching_its_store() {
        let directory = tempfile::tempdir().unwrap();
        let discovery_path = directory.path().join("daemon.json");
        let store_path = directory.path().join("thought.db");
        drop(thought_store::Store::open(&store_path).unwrap());
        let store_before = std::fs::read(&store_path).unwrap();
        let executable = std::env::current_exe().unwrap();
        let mut child = std::process::Command::new(&executable)
            .args(["--exact", "tests::prior_daemon_child", "--nocapture"])
            .env(PRIOR_DAEMON_CHILD, "1")
            .env(PRIOR_DISCOVERY_PATH, &discovery_path)
            .env("THOUGHT_HOME", directory.path())
            .spawn()
            .unwrap();
        let pid = child.id();

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        let published = loop {
            if let Some(published) = thoughtd::discovery::read_published_at(&discovery_path) {
                break published;
            }
            if std::time::Instant::now() >= deadline {
                let _ = child.kill();
                let _ = child.wait();
                panic!("prior daemon did not publish discovery");
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        };
        assert_eq!(published.pid, pid);
        assert_eq!(published.protocol_version, 9);

        let wrong_executable = tempfile::NamedTempFile::new().unwrap();
        assert_eq!(
            retire_verified_prior_daemon_at(
                &published,
                wrong_executable.path(),
                &discovery_path,
                &store_path,
            ),
            Ok(false)
        );
        assert!(thoughtd::discovery::process_is_expected_daemon(
            pid,
            &executable
        ));

        let original_discovery = std::fs::read(&discovery_path).unwrap();
        let mut changed_secret: serde_json::Value =
            serde_json::from_slice(&original_discovery).unwrap();
        changed_secret["token"] = "different-secret-same-public-metadata".into();
        std::fs::write(
            &discovery_path,
            serde_json::to_vec(&changed_secret).unwrap(),
        )
        .unwrap();
        assert!(!thoughtd::discovery::published_record_unchanged_at(
            &discovery_path,
            &published
        ));
        assert_eq!(
            retire_verified_prior_daemon_at(&published, &executable, &discovery_path, &store_path,),
            Ok(false)
        );
        assert!(thoughtd::discovery::process_is_expected_daemon(
            pid,
            &executable
        ));
        std::fs::write(&discovery_path, original_discovery).unwrap();

        let waiter = std::thread::spawn(move || child.wait());
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(3));
        let retire = |barrier: std::sync::Arc<std::sync::Barrier>| {
            let published = published.clone();
            let executable = executable.clone();
            let discovery_path = discovery_path.clone();
            let store_path = store_path.clone();
            std::thread::spawn(move || {
                barrier.wait();
                retire_verified_prior_daemon_at(
                    &published,
                    &executable,
                    &discovery_path,
                    &store_path,
                )
            })
        };
        let first = retire(barrier.clone());
        let second = retire(barrier.clone());
        barrier.wait();
        let first = first.join().unwrap();
        let second = second.join().unwrap();
        if first.is_err()
            || second.is_err()
            || !matches!((&first, &second), (Ok(true), _) | (_, Ok(true)))
        {
            unsafe {
                libc::kill(libc::pid_t::try_from(pid).unwrap(), libc::SIGKILL);
            }
        }
        let _ = waiter.join().unwrap();

        assert!(first.is_ok(), "first upgrader failed: {first:?}");
        assert!(second.is_ok(), "second upgrader failed: {second:?}");
        assert!(
            matches!((&first, &second), (Ok(true), _) | (_, Ok(true))),
            "one upgrader must retire the predecessor: {first:?}, {second:?}"
        );
        assert!(!discovery_path.exists());
        assert_eq!(std::fs::read(&store_path).unwrap(), store_before);
    }
}
