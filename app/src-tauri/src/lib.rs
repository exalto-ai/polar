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
    sync::{
        Mutex, MutexGuard,
        atomic::{AtomicU64, Ordering},
    },
};
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;
use thoughtd::discovery::{self, Daemon};

#[cfg(target_os = "macos")]
mod macos_termination;

#[derive(serde::Serialize)]
struct Connection {
    protocol_version: u32,
    sync_url: String,
    mcp_url: String,
    editor_token: String,
    mcp_token: String,
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
    connection_payload(state.inner())
}

fn connection_payload(state: &Daemon) -> Connection {
    Connection {
        protocol_version: state.protocol_version,
        // Same origin, different path: the editor is a sync peer, agents come
        // in over MCP.
        sync_url: state
            .url
            .replace("http://", "ws://")
            .replace("/mcp", "/sync"),
        mcp_url: state.url.clone(),
        editor_token: state.editor_token.clone(),
        mcp_token: state.token.clone(),
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

#[derive(Default)]
struct QuitAttempt {
    requested: bool,
    native_reply_pending: bool,
    native_reply_scheduled: bool,
    active_window: Option<String>,
}

#[derive(Default)]
struct QuitState(Mutex<QuitAttempt>);

/// Heuristic pause between closing one document window and presenting the
/// next one's guard. Tauri does not expose the originating guard-dismissal
/// event or mouse-button state here, so this lets the triggering mouse-up or
/// Return finish before another default button exists to receive it.
const QUIT_INPUT_SETTLE_MS: u64 = 150;

impl QuitState {
    fn attempt(&self) -> MutexGuard<'_, QuitAttempt> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Begin one quit attempt. Repeated menu, Dock, or system requests join
    /// the in-flight attempt instead of opening another set of close sheets.
    fn begin(&self, native: bool) {
        let mut attempt = self.attempt();
        if native {
            attempt.native_reply_pending = true;
        }
        if !attempt.requested {
            attempt.requested = true;
            attempt.active_window = None;
        }
    }

    fn is_requested(&self) -> bool {
        self.attempt().requested
    }

    fn native_reply_pending(&self) -> bool {
        self.attempt().native_reply_pending
    }

    fn activate(&self, label: &str) -> bool {
        let mut attempt = self.attempt();
        if !attempt.requested || attempt.active_window.is_some() {
            return false;
        }
        attempt.active_window = Some(label.to_string());
        true
    }

    fn window_destroyed(&self, label: &str) -> bool {
        let mut attempt = self.attempt();
        if attempt.active_window.as_deref() == Some(label) {
            attempt.active_window = None;
            true
        } else {
            false
        }
    }

    /// Cancel the current attempt and report whether AppKit is waiting for a
    /// reply to `applicationShouldTerminate:`.
    fn cancel(&self) -> bool {
        let mut attempt = self.attempt();
        attempt.requested = false;
        attempt.active_window = None;
        if attempt.native_reply_pending && !attempt.native_reply_scheduled {
            attempt.native_reply_scheduled = true;
            true
        } else {
            false
        }
    }

    fn finish(&self) -> bool {
        let mut attempt = self.attempt();
        attempt.requested = false;
        attempt.active_window = None;
        if attempt.native_reply_pending && !attempt.native_reply_scheduled {
            attempt.native_reply_scheduled = true;
            true
        } else {
            false
        }
    }

    fn native_reply_sent(&self) {
        let mut attempt = self.attempt();
        attempt.native_reply_pending = false;
        attempt.native_reply_scheduled = false;
        if !attempt.requested {
            *attempt = QuitAttempt::default();
        }
    }

    fn abort(&self) {
        *self.attempt() = QuitAttempt::default();
    }
}

#[tauri::command]
fn cancel_quit(app: tauri::AppHandle, state: tauri::State<'_, QuitState>) {
    if state.cancel() {
        #[cfg(target_os = "macos")]
        macos_termination::reply(&app, false);
    }
}

/// Start or join a quit attempt. Returns false when there are no windows or
/// application state is unavailable, in which case AppKit may terminate now.
fn request_guarded_quit(handle: &tauri::AppHandle, native: bool) -> bool {
    let windows = handle.webview_windows();
    if windows.is_empty() {
        return false;
    }
    let Some(state) = handle.try_state::<QuitState>() else {
        return false;
    };

    state.begin(native);
    advance_guarded_quit(handle);
    true
}

/// Close one window at a time so a multi-document quit never presents several
/// independent export prompts or lets one cancellation race another window.
fn advance_guarded_quit(handle: &tauri::AppHandle) {
    let Some(state) = handle.try_state::<QuitState>() else {
        return;
    };
    if !state.is_requested() {
        return;
    }

    let mut windows = handle
        .webview_windows()
        .into_iter()
        .map(|(label, window)| {
            let focused = window.is_focused().unwrap_or(false);
            (focused, label, window)
        })
        .collect::<Vec<_>>();
    if windows.is_empty() {
        let native_reply_pending = state.finish();
        #[cfg(target_os = "macos")]
        if native_reply_pending {
            macos_termination::reply(handle, true);
            return;
        }
        #[cfg(not(target_os = "macos"))]
        let _ = native_reply_pending;
        handle.exit(0);
        return;
    }

    windows.sort_by(
        |(left_focus, left_label, _), (right_focus, right_label, _)| {
            right_focus
                .cmp(left_focus)
                .then_with(|| left_label.cmp(right_label))
        },
    );
    let (_, label, window) = &windows[0];
    if !state.activate(label) {
        return;
    }
    let _ = window.set_focus();
    if let Err(error) = window.close() {
        eprintln!("could not request window close during quit: {error}");
        if state.cancel() {
            #[cfg(target_os = "macos")]
            macos_termination::reply(handle, false);
        }
    }
}

/// Let the input event that dismissed one close sheet finish before presenting
/// the next document's sheet. Without this turn boundary, a mouse-up or Return
/// can activate the default button in the newly focused window.
fn schedule_guarded_quit_advance(handle: &tauri::AppHandle) {
    let queued_handle = handle.clone();
    let spawned = std::thread::Builder::new()
        .name("thought-quit-advance".into())
        .spawn(move || {
            std::thread::sleep(Duration::from_millis(QUIT_INPUT_SETTLE_MS));
            let main_handle = queued_handle.clone();
            if let Err(error) =
                queued_handle.run_on_main_thread(move || advance_guarded_quit(&main_handle))
            {
                eprintln!("could not advance guarded quit on the main thread: {error}");
                if queued_handle
                    .try_state::<QuitState>()
                    .is_some_and(|state| state.cancel())
                {
                    #[cfg(target_os = "macos")]
                    macos_termination::reply(&queued_handle, false);
                }
            }
        });
    if let Err(error) = spawned {
        eprintln!("could not schedule the next guarded window close: {error}");
        advance_guarded_quit(handle);
    }
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

/// Reuse a running daemon, or start one only when none is published. A stale
/// or unauthenticated record is surfaced for the developer to resolve; this
/// process never signals or silently replaces another possible store owner.
fn ensure_daemon() -> Result<Daemon, String> {
    let thoughtd =
        find_binary("thoughtd").ok_or_else(|| "could not find the thoughtd binary".to_string())?;
    if let Some(daemon) = discovery::read() {
        if discovery::authenticated_reachable(&daemon)
            && discovery::editor_authenticated_reachable(&daemon, &thoughtd)
        {
            return Ok(daemon);
        }
        return Err(format!(
            "A thought daemon is already published but did not accept its MCP and editor capabilities. Quit any running Proof of Thought or thoughtd process, then remove {} if the problem persists.",
            discovery::discovery_path().display()
        ));
    }

    if discovery::discovery_path().exists() {
        return Err(format!(
            "A thought daemon has legacy or incompatible discovery protocol/capabilities. Quit any running Proof of Thought or thoughtd process, then remove {} if the problem persists.",
            discovery::discovery_path().display()
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
            && discovery::editor_authenticated_reachable(&daemon, &thoughtd)
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
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            #[cfg(target_os = "macos")]
            macos_termination::install(app.handle().clone())?;
            Ok(())
        })
        .manage(daemon)
        .manage(QuitState::default())
        .invoke_handler(tauri::generate_handler![
            connection,
            new_window,
            import_markdown,
            export_markdown,
            cancel_quit
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|handle, event| match event {
        tauri::RunEvent::ExitRequested {
            code: None, api, ..
        } => {
            if handle
                .try_state::<QuitState>()
                .is_some_and(|state| state.native_reply_pending())
            {
                // The last window may disappear before the queued AppKit reply
                // runs. Keep Tauri's event loop alive until that reply resolves
                // the outstanding NSTerminateLater decision.
                api.prevent_exit();
                if handle.webview_windows().is_empty()
                    && let Some(state) = handle.try_state::<QuitState>()
                    && state.finish()
                {
                    #[cfg(target_os = "macos")]
                    macos_termination::reply(handle, true);
                }
                return;
            }
            let windows = handle.webview_windows();
            if windows.is_empty() {
                return;
            }

            // Tauri-managed exit requests do not emit per-window close
            // requests by default. Turn them into those requests so every
            // document gets the same export prompt and durable autosave
            // barrier as Command-W.
            api.prevent_exit();
            request_guarded_quit(handle, false);
        }
        tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::Destroyed,
            ..
        } if handle
            .try_state::<QuitState>()
            .is_some_and(|state| state.window_destroyed(&label)) =>
        {
            schedule_guarded_quit_advance(handle);
        }
        _ => {}
    });
}

#[cfg(test)]
mod tests {
    use super::{
        QuitState, atomic_write, cascade_axis, connection_payload, document_window_path,
        safe_suggested_name, serialize_document,
    };
    use thought_schema::{Mark, Node};

    #[test]
    fn connection_payload_keeps_mcp_and_editor_capabilities_separate() {
        let daemon = thoughtd::discovery::Daemon {
            url: "http://127.0.0.1:1234/mcp".into(),
            protocol_version: thoughtd::discovery::PROTOCOL_VERSION,
            pid: 4321,
            token: "mcp-only".into(),
            editor_token: "editor-only".into(),
        };

        let payload = connection_payload(&daemon);
        assert_eq!(
            payload.protocol_version,
            thoughtd::discovery::PROTOCOL_VERSION
        );
        assert_eq!(payload.mcp_token, "mcp-only");
        assert_eq!(payload.editor_token, "editor-only");
        assert_eq!(payload.sync_url, "ws://127.0.0.1:1234/sync");
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
        assert!(serialize_document(invalid).is_err());
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
    fn quit_attempts_serialize_windows_and_preserve_native_reply_state() {
        let state = QuitState::default();
        state.begin(false);
        assert!(state.activate("window-a"));
        assert!(!state.activate("window-b"));

        // A Dock or system request can join an application-menu attempt. The
        // currently active close sheet stays in place and AppKit gets a reply
        // only after that same attempt finishes or is cancelled.
        state.begin(true);
        assert!(!state.window_destroyed("window-b"));
        assert!(!state.activate("window-b"));
        assert!(state.window_destroyed("window-a"));
        assert!(state.activate("window-b"));
        assert!(state.finish());
        assert!(!state.finish());
        assert!(!state.is_requested());
        assert!(state.native_reply_pending());
        state.native_reply_sent();
        assert!(!state.native_reply_pending());

        state.begin(true);
        assert!(state.cancel());
        assert!(!state.cancel());
        assert!(state.native_reply_pending());
        state.native_reply_sent();
        assert!(!state.cancel());
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
}
