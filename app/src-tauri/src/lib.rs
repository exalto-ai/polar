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
    sync::atomic::{AtomicU64, Ordering},
};
use tauri::Manager;
use tauri_plugin_dialog::DialogExt;
use thoughtd::discovery::{self, Daemon};

#[cfg(target_os = "macos")]
mod macos_secure_input;
mod pro_chat;
mod pro_provider;

#[derive(serde::Serialize)]
struct Connection {
    protocol_version: u32,
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

/// Reuse the published instance when its identity and bearer both work.
/// Otherwise start a candidate: lifetime home and store locks decide whether
/// that child may replace stale discovery, so clients never infer ownership
/// from a PID or the presence of a file.
fn ensure_daemon() -> Result<Daemon, String> {
    if let Some(daemon) = discovery::read() {
        if discovery::authenticated_reachable(&daemon) {
            return Ok(daemon);
        }
    } else if discovery::discovery_path().exists() {
        return Err(format!(
            "The thought daemon discovery record uses an incompatible or invalid format. Quit any older Proof of Thought or thoughtd process, then remove {}.",
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
    let provider_state = pro_provider::ProviderState::platform(discovery::home());
    let chat_state = pro_chat::ChatState::platform(discovery::home());
    let app = tauri::Builder::default()
        .plugin(tauri_plugin_dialog::init())
        .plugin(tauri_plugin_opener::init())
        .manage(daemon)
        .manage(provider_state)
        .manage(chat_state)
        .invoke_handler(tauri::generate_handler![
            connection,
            new_window,
            import_markdown,
            export_markdown,
            document_wording_revision,
            pro_provider::provider_configurations,
            pro_provider::configure_provider_key,
            pro_provider::revalidate_provider_key,
            pro_provider::remove_provider_key,
            pro_chat::pro_chat_capabilities,
            pro_chat::pro_chat_history,
            pro_chat::start_pro_chat,
            pro_chat::stop_pro_chat,
            pro_chat::clear_pro_chat
        ])
        .build(tauri::generate_context!())
        .expect("error while building tauri application");

    app.run(|handle, event| {
        if let tauri::RunEvent::WindowEvent {
            label,
            event: tauri::WindowEvent::Destroyed,
            ..
        } = event
            && let Some(state) = handle.try_state::<pro_chat::ChatState>()
        {
            state.cancel_window(&label);
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{
        atomic_write, cascade_axis, connection_payload, document_window_path,
        document_wording_revision, safe_suggested_name, serialize_document,
    };
    use thought_schema::{Mark, Node};

    #[test]
    fn connection_payload_uses_one_private_platform_capability() {
        let daemon = thoughtd::discovery::Daemon {
            url: "http://127.0.0.1:1234/mcp".into(),
            protocol_version: thoughtd::discovery::PROTOCOL_VERSION,
            instance_id: "c".repeat(64),
            token: "platform-only".into(),
        };

        let payload = connection_payload(&daemon);
        assert_eq!(
            payload.protocol_version,
            thoughtd::discovery::PROTOCOL_VERSION
        );
        assert_eq!(payload.token, "platform-only");
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
}
