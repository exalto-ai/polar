//! Drives the daemon over real MCP: a spawned process, an HTTP transport, and
//! the JSON-RPC handshake. The tool layer is tested in `thought-mcp` without any
//! of that; what is under test here is specifically the wiring — discovery,
//! authentication, and whether an agent can actually reach the tools.

mod harness;

use harness::Daemon;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use std::sync::Arc;
use thought_credentials::CredentialStore;
use thought_mcp::{ReviewerClient, ReviewerPermissions, Workspace};
use thoughtd::connections::ConnectionRegistry;
use thoughtd::discovery::{self, Daemon as PublishedDaemon};

fn editor_base(daemon: &Daemon) -> String {
    daemon.url.strip_suffix("/mcp").unwrap().to_string()
}

fn create_reviewer(
    daemon: &Daemon,
    label: &str,
    permissions: serde_json::Value,
) -> (serde_json::Value, String) {
    let mut response = ureq::post(format!(
        "{}/editor/reviewer-connections",
        editor_base(daemon)
    ))
    .header("Authorization", &format!("Bearer {}", daemon.editor_token))
    .send_json(serde_json::json!({
        "client": "claude-code",
        "display_label": label,
        "permissions": permissions
    }))
    .expect("create reviewer");
    let body: serde_json::Value = response.body_mut().read_json().unwrap();
    let id = body["connection"]["id"].as_str().unwrap();
    let credential = std::fs::read_to_string(
        daemon
            .home
            .path()
            .join("reviewer-credentials")
            .join(format!("{id}.credential")),
    )
    .unwrap();
    (body["connection"].clone(), credential)
}

#[test]
fn a_reviewer_reads_the_daemon_over_mcp_but_cannot_write() {
    let daemon = Daemon::start();
    daemon.connect();

    let tools: Vec<String> = daemon.rpc("tools/list", serde_json::json!({}))["tools"]
        .as_array()
        .expect("tool list")
        .iter()
        .map(|t| t["name"].as_str().unwrap_or_default().to_string())
        .collect();
    for expected in [
        "create_document",
        "read_document",
        "replace_block",
        "document_lineage",
        "search",
    ] {
        assert!(
            tools.contains(&expected.to_string()),
            "missing tool {expected}"
        );
    }

    let doc_id = daemon.create_document("Transport");
    let view = daemon.read_document(&doc_id);
    assert_eq!(view["title"], "Transport");

    // Anchors must point at lines that exist, or a follow-up edit misses.
    let markdown = view["markdown"].as_str().unwrap();
    let lines = markdown.lines().count();
    for block in view["blocks"].as_array().expect("blocks") {
        let end = block["line_end"].as_u64().expect("line_end") as usize;
        assert!(end <= lines, "anchor points past the end of the document");
    }

    let hits = daemon.call(
        "search",
        serde_json::json!({ "query": "Transport", "limit": 5 }),
    );
    assert_eq!(hits["hits"][0]["doc_id"], doc_id.as_str());

    let imported_markdown = "<!--thought:title-->\n# Imported\n\nFrom **Markdown**.";
    let imported_id = daemon.create_document_with_markdown("Imported.md", Some(imported_markdown));
    let imported_view = daemon.read_document(&imported_id);
    assert_eq!(imported_view["markdown"], imported_markdown);

    let mut session = daemon.session_id();
    let denied = daemon
        .raw_rpc_with_token(
            &daemon.reviewer_token,
            &mut session,
            "tools/call",
            serde_json::json!({
                "name": "create_document",
                "arguments": {"title": "Must not exist"}
            }),
        )
        .unwrap();
    assert!(denied.get("error").is_some(), "{denied}");
    assert!(denied.to_string().contains("authorization"), "{denied}");
}

#[test]
fn a_legacy_human_label_cannot_promote_a_read_only_route_to_write_access() {
    let daemon = Daemon::start();
    daemon.connect();

    let doc_id = daemon.create_document("Reported review");
    let view = daemon.read_document(&doc_id);
    let mut session = daemon.session_id();
    let denied = daemon
        .raw_rpc_with_token(
            &daemon.reviewer_token,
            &mut session,
            "tools/call",
            serde_json::json!({
                "name": "replace_block",
                "arguments": {
                    "doc_id": doc_id,
                    "block_id": view["blocks"][0]["block_id"],
                    "markdown": "Spoofed",
                    "kind": "human"
                }
            }),
        )
        .unwrap();
    assert!(denied.get("error").is_some(), "{denied}");
    assert!(denied.to_string().contains("authorization"), "{denied}");
}

#[test]
fn the_endpoint_refuses_an_unauthenticated_client() {
    let daemon = Daemon::start();
    // A dedicated agent with no idle pooling. The global agent reuses
    // keep-alive sockets, and one the server has since closed fails on write
    // with ECONNRESET — which reads as "not a 401" and fails the test for a
    // reason that has nothing to do with authentication.
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .max_idle_connections(0)
            .build(),
    );
    let response = agent
        .post(&daemon.url)
        .header("Accept", "application/json, text/event-stream")
        .send_json(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                       "clientInfo": {"name": "intruder", "version": "1"}}
        }));

    // Any local process can reach a loopback port; possession of the 0600
    // discovery file is what grants access.
    match response {
        Err(ureq::Error::StatusCode(401)) => {}
        other => panic!("expected 401, got {other:?}"),
    }
}

#[test]
fn the_mcp_endpoint_rejects_the_editor_capability() {
    let daemon = Daemon::start();
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .max_idle_connections(0)
            .build(),
    );
    let response = agent
        .post(&daemon.url)
        .header("Authorization", &format!("Bearer {}", daemon.editor_token))
        .header("Accept", "application/json, text/event-stream")
        .send_json(serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                       "clientInfo": {"name": "wrong-capability", "version": "1"}}
        }));

    match response {
        Err(ureq::Error::StatusCode(401)) => {}
        other => panic!("expected editor capability to receive 401 from MCP, got {other:?}"),
    }
}

#[test]
fn the_internal_discovery_capability_is_read_only() {
    let daemon = Daemon::start();
    let mut session = None;
    let initialized = daemon
        .raw_rpc_with_token(
            &daemon.token,
            &mut session,
            "initialize",
            serde_json::json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "internal-test", "version": "1"}
            }),
        )
        .unwrap();
    assert!(initialized.get("result").is_some());

    let listed = daemon
        .raw_rpc_with_token(
            &daemon.token,
            &mut session,
            "tools/call",
            serde_json::json!({
                "name": "list_documents",
                "arguments": {"limit": 5}
            }),
        )
        .unwrap();
    assert!(listed.get("result").is_some(), "{listed}");

    let denied = daemon
        .raw_rpc_with_token(
            &daemon.token,
            &mut session,
            "tools/call",
            serde_json::json!({
                "name": "create_document",
                "arguments": {"title": "Must not exist"}
            }),
        )
        .unwrap();
    assert!(denied.get("error").is_some(), "{denied}");
    assert!(denied.to_string().contains("read-only"), "{denied}");
}

#[test]
fn one_reviewer_cannot_reuse_another_reviewers_session() {
    let daemon = Daemon::start();
    daemon.connect();
    let first_session = daemon.session_id().expect("default reviewer session");
    let (_connection, second_token) = create_reviewer(
        &daemon,
        "Second reviewer",
        serde_json::json!({
            "document_scope": "all",
            "can_read": true,
            "can_edit": false,
            "can_create": false,
            "can_trash": false,
            "document_ids": []
        }),
    );

    let mut stolen_session = Some(first_session);
    let result = daemon.raw_rpc_with_token(
        &second_token,
        &mut stolen_session,
        "tools/list",
        serde_json::json!({}),
    );
    assert!(matches!(result, Err(ureq::Error::StatusCode(401))));

    let mut own_session = None;
    let result = daemon
        .raw_rpc_with_token(
            &second_token,
            &mut own_session,
            "initialize",
            serde_json::json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "second-reviewer", "version": "1"}
            }),
        )
        .unwrap();
    assert!(result.get("result").is_some());
    assert_ne!(own_session, daemon.session_id());
}

#[test]
fn deleting_an_mcp_session_removes_its_authorization_binding() {
    let daemon = Daemon::start();
    daemon.connect();
    let session_id = daemon.session_id().expect("reviewer session");

    let response = ureq::delete(&daemon.url)
        .header(
            "Authorization",
            &format!("Bearer {}", daemon.reviewer_token),
        )
        .header("Mcp-Session-Id", &session_id)
        .header("Mcp-Protocol-Version", "2025-06-18")
        .call()
        .expect("delete MCP session");
    assert_eq!(response.status().as_u16(), 202);

    let mut deleted_session = Some(session_id);
    let reused = daemon.raw_rpc_with_token(
        &daemon.reviewer_token,
        &mut deleted_session,
        "tools/list",
        serde_json::json!({}),
    );
    assert!(matches!(reused, Err(ureq::Error::StatusCode(401))));

    let mut replacement_session = None;
    let initialized = daemon
        .raw_rpc_with_token(
            &daemon.reviewer_token,
            &mut replacement_session,
            "initialize",
            serde_json::json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "replacement", "version": "1"}
            }),
        )
        .expect("replacement session initializes");
    assert!(initialized.get("result").is_some(), "{initialized}");
    assert!(replacement_session.is_some());
}

#[cfg(unix)]
#[test]
fn sigint_drains_a_live_sse_session_before_exit() {
    use std::sync::mpsc;
    use std::time::Duration;

    let mut daemon = Daemon::start();
    daemon.connect();
    let session_id = daemon.session_id().expect("reviewer session");
    let url = daemon.url.clone();
    let credential = daemon.reviewer_token.clone();
    let (ready_tx, ready_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();

    let live_stream = std::thread::spawn(move || {
        let response = ureq::get(&url)
            .header("Authorization", &format!("Bearer {credential}"))
            .header("Accept", "text/event-stream")
            .header("Mcp-Session-Id", &session_id)
            .header("Mcp-Protocol-Version", "2025-06-18")
            .call()
            .expect("open standalone MCP event stream");
        ready_tx.send(()).unwrap();
        let _ = release_rx.recv_timeout(Duration::from_secs(8));
        drop(response);
    });
    ready_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("standalone event stream becomes live");

    let status = daemon.interrupt_and_wait(Duration::from_secs(6));
    assert!(status.success(), "thoughtd exited with {status}");
    assert!(
        !daemon.home.path().join("daemon.json").exists(),
        "graceful shutdown removes discovery"
    );
    let logs = daemon.logs();
    assert!(
        logs.contains("drained MCP sessions") && logs.contains("closed=1"),
        "SIGINT must drain the live MCP session before the forced server timeout:\n{logs}"
    );

    let _ = release_tx.send(());
    live_stream.join().unwrap();
}

#[test]
fn current_document_permissions_filter_reads_and_deny_every_ungranted_write_class() {
    let daemon = Daemon::start();
    daemon.connect();
    let allowed_id = daemon.create_document("Allowed");
    let denied_id = daemon.create_document("Private");
    let allowed_view = daemon.read_document(&allowed_id);
    let allowed_block = allowed_view["blocks"][0]["block_id"].as_str().unwrap();

    let (_connection, token) = create_reviewer(
        &daemon,
        "Current document reader",
        serde_json::json!({
            "document_scope": "current",
            "can_read": true,
            "can_edit": false,
            "can_create": false,
            "can_trash": false,
            "document_ids": [&allowed_id]
        }),
    );
    let mut session = None;
    daemon
        .raw_rpc_with_token(
            &token,
            &mut session,
            "initialize",
            serde_json::json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "scoped-reader", "version": "1"}
            }),
        )
        .unwrap();

    let list = daemon
        .raw_rpc_with_token(
            &token,
            &mut session,
            "tools/call",
            serde_json::json!({
                "name": "list_documents",
                "arguments": {"limit": 50}
            }),
        )
        .unwrap();
    let listed: serde_json::Value =
        serde_json::from_str(list["result"]["content"][0]["text"].as_str().unwrap()).unwrap();
    assert_eq!(listed["documents"].as_array().unwrap().len(), 1);
    assert_eq!(listed["documents"][0]["doc_id"], allowed_id);

    let read_allowed = daemon
        .raw_rpc_with_token(
            &token,
            &mut session,
            "tools/call",
            serde_json::json!({
                "name": "read_document",
                "arguments": {"doc_id": &allowed_id}
            }),
        )
        .unwrap();
    assert!(read_allowed.get("result").is_some(), "{read_allowed}");

    for (name, arguments) in [
        ("read_document", serde_json::json!({"doc_id": &denied_id})),
        (
            "replace_block",
            serde_json::json!({
                "doc_id": &allowed_id,
                "block_id": allowed_block,
                "markdown": "Not allowed"
            }),
        ),
        (
            "create_document",
            serde_json::json!({"title": "Not allowed"}),
        ),
        (
            "set_document_deleted",
            serde_json::json!({"doc_id": &allowed_id, "deleted": true}),
        ),
    ] {
        let response = daemon
            .raw_rpc_with_token(
                &token,
                &mut session,
                "tools/call",
                serde_json::json!({"name": name, "arguments": arguments}),
            )
            .unwrap();
        assert!(
            response.get("error").is_some(),
            "{name} unexpectedly succeeded: {response}"
        );
        assert!(response.to_string().contains("authorization"), "{response}");
    }
}

#[test]
fn editor_manages_status_rotation_and_terminal_revocation_without_leaking_secrets() {
    let daemon = Daemon::start();
    daemon.connect();
    let base = editor_base(&daemon);

    let mut response = ureq::get(format!("{base}/editor/reviewer-connections"))
        .header("Authorization", &format!("Bearer {}", daemon.editor_token))
        .call()
        .unwrap();
    let listed: serde_json::Value = response.body_mut().read_json().unwrap();
    let connection = &listed["connections"][0];
    assert_eq!(connection["status"], "connected");
    assert_eq!(connection["client"], "codex");
    assert_eq!(connection["permissions"]["document_scope"], "all");
    assert!(!listed.to_string().contains(&daemon.reviewer_token));

    let disconnect = ureq::delete(format!("{base}/reviewer/status"))
        .header(
            "Authorization",
            &format!("Bearer {}", daemon.reviewer_token),
        )
        .call()
        .unwrap();
    assert_eq!(disconnect.status().as_u16(), 204);

    let mut response = ureq::patch(format!(
        "{base}/editor/reviewer-connections/{}",
        daemon.connection_id
    ))
    .header("Authorization", &format!("Bearer {}", daemon.editor_token))
    .send_json(serde_json::json!({
        "expected_revision": 1,
        "display_label": "Renamed reviewer"
    }))
    .unwrap();
    let renamed: serde_json::Value = response.body_mut().read_json().unwrap();
    assert_eq!(renamed["connection"]["revision"], 2);
    assert_eq!(renamed["connection"]["status"], "disconnected");

    let old_token = daemon.reviewer_token.clone();
    let mut response = ureq::post(format!(
        "{base}/editor/reviewer-connections/{}/reset",
        daemon.connection_id
    ))
    .header("Authorization", &format!("Bearer {}", daemon.editor_token))
    .send_json(serde_json::json!({"expected_revision": 2}))
    .unwrap();
    let reset: serde_json::Value = response.body_mut().read_json().unwrap();
    assert_eq!(reset["connection"]["revision"], 3);
    assert!(!reset.to_string().contains(&old_token));
    let new_token = std::fs::read_to_string(
        daemon
            .home
            .path()
            .join("reviewer-credentials")
            .join(format!("{}.credential", daemon.connection_id)),
    )
    .unwrap();
    assert_ne!(new_token, old_token);

    let health = format!("{base}{}", discovery::MCP_HEALTH_PATH);
    assert!(matches!(
        ureq::get(&health)
            .header("Authorization", &format!("Bearer {old_token}"))
            .call(),
        Err(ureq::Error::StatusCode(401))
    ));
    assert!(
        ureq::get(&health)
            .header("Authorization", &format!("Bearer {new_token}"))
            .call()
            .is_ok()
    );

    let mut response = ureq::delete(format!(
        "{base}/editor/reviewer-connections/{}",
        daemon.connection_id
    ))
    .header("Authorization", &format!("Bearer {}", daemon.editor_token))
    .force_send_body()
    .send_json(serde_json::json!({"expected_revision": 3}))
    .unwrap();
    let revoked: serde_json::Value = response.body_mut().read_json().unwrap();
    assert_eq!(revoked["connection"]["status"], "revoked");
    assert_eq!(revoked["connection"]["revision"], 4);
    assert!(matches!(
        ureq::get(&health)
            .header("Authorization", &format!("Bearer {new_token}"))
            .call(),
        Err(ureq::Error::StatusCode(401))
    ));

    let discovery_bytes = std::fs::read(daemon.home.path().join("daemon.json")).unwrap();
    let database_bytes = std::fs::read(daemon.home.path().join("thought.db")).unwrap();
    assert!(
        !discovery_bytes
            .windows(new_token.len())
            .any(|window| window == new_token.as_bytes())
    );
    assert!(
        !database_bytes
            .windows(new_token.len())
            .any(|window| window == new_token.as_bytes())
    );
}

#[test]
fn editor_lifecycle_routes_require_the_editor_capability_and_record_imports() {
    let daemon = Daemon::start();
    let endpoint = daemon.url.strip_suffix("/mcp").unwrap();
    let editor_url = format!("{endpoint}/editor/documents");
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .max_idle_connections(0)
            .build(),
    );

    let unauthorized = agent
        .post(&editor_url)
        .header("Authorization", &format!("Bearer {}", daemon.token))
        .send_json(serde_json::json!({
            "title": "Wrong capability",
            "initial_markdown": "should not exist"
        }));
    assert!(matches!(unauthorized, Err(ureq::Error::StatusCode(401))));

    let mut response = agent
        .post(&editor_url)
        .header("Authorization", &format!("Bearer {}", daemon.editor_token))
        .send_json(serde_json::json!({
            "title": "Imported notes",
            "initial_markdown": "# Imported notes\n\nExact local file."
        }))
        .expect("editor import succeeds");
    let created: serde_json::Value = response.body_mut().read_json().expect("document JSON");
    let doc_id = created["doc_id"].as_str().expect("document id");

    daemon.connect();
    let lineage = daemon.call("document_lineage", serde_json::json!({ "doc_id": doc_id }));
    assert_eq!(lineage["alignment"], "anchored");
    assert_eq!(lineage["consumer_eligible"], true);
    assert_eq!(
        lineage["summary"]["contributions"][0]["source"]["ingress"],
        "imported"
    );
    assert_eq!(
        lineage["summary"]["contributions"][0]["source"]["assurance"],
        "observed"
    );
}

#[test]
fn discovery_probe_verifies_the_published_capabilities() {
    let daemon = Daemon::start();
    let published = PublishedDaemon {
        url: daemon.url.clone(),
        protocol_version: daemon.protocol_version,
        pid: daemon.pid,
        token: daemon.token.clone(),
        editor_token: daemon.editor_token.clone(),
    };
    assert!(discovery::authenticated_reachable(&published));
    assert!(discovery::editor_authenticated_reachable(
        &published,
        std::path::Path::new(env!("CARGO_BIN_EXE_thoughtd"))
    ));

    let mut wrong_token = published;
    wrong_token.token.push_str("-wrong");
    assert!(
        !discovery::authenticated_reachable(&wrong_token),
        "an unrelated or stale bearer credential must not validate the endpoint"
    );
    assert!(
        discovery::editor_authenticated_reachable(
            &wrong_token,
            std::path::Path::new(env!("CARGO_BIN_EXE_thoughtd"))
        ),
        "the editor capability remains independently valid"
    );

    wrong_token.token = daemon.token.clone();
    wrong_token.editor_token.push_str("-wrong");
    assert!(
        !discovery::editor_authenticated_reachable(
            &wrong_token,
            std::path::Path::new(env!("CARGO_BIN_EXE_thoughtd"))
        ),
        "an unrelated editor capability must not validate the sync endpoint"
    );
}

#[test]
fn the_stdio_shim_requires_a_saved_connection_id() {
    let home = tempfile::tempdir().expect("temp dir");
    let output = Command::new(env!("CARGO_BIN_EXE_thought-mcp-stdio"))
        .env("THOUGHT_HOME", home.path())
        .env("THOUGHT_CREDENTIAL_BACKEND", "file")
        .stdin(Stdio::null())
        .output()
        .expect("spawn stdio shim");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("requires `--connection <id>`"), "{stderr}");
    assert!(!home.path().join("daemon.json").exists());
}

#[test]
fn the_stdio_shim_refuses_to_replace_a_daemon_that_rejects_its_token() {
    let daemon = Daemon::start();
    let published = daemon.home.path().join("daemon.json");
    let mut wrong: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&published).expect("discovery is readable"))
            .expect("discovery is json");
    wrong["token"] = "not-the-daemon-token".into();
    std::fs::write(&published, serde_json::to_vec_pretty(&wrong).unwrap()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_thought-mcp-stdio"))
        .env("THOUGHT_HOME", daemon.home.path())
        .env("THOUGHT_CREDENTIAL_BACKEND", "file")
        .arg("--connection")
        .arg(&daemon.connection_id)
        .stdin(Stdio::null())
        .output()
        .expect("spawn stdio shim");
    assert!(
        !output.status.success(),
        "the shim must not replace a published daemon"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("quit any running Proof of Thought or thoughtd process"),
        "the failure must explain how to resolve the published daemon: {stderr}"
    );

    let original_daemon = PublishedDaemon {
        url: daemon.url.clone(),
        protocol_version: daemon.protocol_version,
        pid: daemon.pid,
        token: daemon.token.clone(),
        editor_token: daemon.editor_token.clone(),
    };
    assert!(
        discovery::authenticated_reachable(&original_daemon),
        "the shim must leave the existing process running"
    );
}

#[test]
fn the_stdio_shim_refuses_legacy_single_token_discovery() {
    let daemon = Daemon::start();
    let published = daemon.home.path().join("daemon.json");
    let mut legacy: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&published).expect("discovery is readable"))
            .expect("discovery is json");
    legacy
        .as_object_mut()
        .expect("discovery object")
        .remove("editor_token");
    std::fs::write(&published, serde_json::to_vec_pretty(&legacy).unwrap()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_thought-mcp-stdio"))
        .env("THOUGHT_HOME", daemon.home.path())
        .env("THOUGHT_CREDENTIAL_BACKEND", "file")
        .arg("--connection")
        .arg(&daemon.connection_id)
        .stdin(Stdio::null())
        .output()
        .expect("spawn stdio shim");
    assert!(!output.status.success());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("legacy or incompatible discovery"),
        "the failure must identify incompatible discovery: {stderr}"
    );

    let original_daemon = PublishedDaemon {
        url: daemon.url.clone(),
        protocol_version: daemon.protocol_version,
        pid: daemon.pid,
        token: daemon.token.clone(),
        editor_token: daemon.editor_token.clone(),
    };
    assert!(
        discovery::authenticated_reachable(&original_daemon),
        "the shim must leave the existing process running"
    );
}

/// The shim is what an MCP client actually spawns (AD-10). Its job is to make
/// "spawn a server on stdio" mean "reach the one daemon", so the case worth
/// testing is the one where no daemon is running yet.
#[test]
fn the_stdio_shim_starts_a_daemon_and_proxies_to_it() {
    let home = tempfile::tempdir().expect("temp dir");
    assert!(
        !home.path().join("daemon.json").exists(),
        "test must begin with no daemon published"
    );

    // Provision the durable reviewer in the same store the shim-started
    // daemon will open. The setup command carries only this stable ID.
    let workspace = Arc::new(Workspace::open(home.path().join("thought.db")).unwrap());
    let registry = ConnectionRegistry::new(
        workspace,
        CredentialStore::files(home.path().join("reviewer-credentials")),
    );
    let connection = registry
        .create(
            ReviewerClient::Codex,
            "Shim reviewer".to_string(),
            ReviewerPermissions::all(true, true, true),
            10,
        )
        .unwrap();
    drop(registry);

    let mut shim = Command::new(env!("CARGO_BIN_EXE_thought-mcp-stdio"))
        .env("THOUGHT_HOME", home.path())
        .env("THOUGHT_CREDENTIAL_BACKEND", "file")
        .arg("--connection")
        .arg(&connection.id)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn shim");

    let mut stdin = shim.stdin.take().expect("piped stdin");
    let mut stdout = BufReader::new(shim.stdout.take().expect("piped stdout"));

    let mut send = |body: serde_json::Value| {
        use std::io::Write;
        writeln!(stdin, "{body}").expect("write to shim");
        stdin.flush().expect("flush");
    };

    send(serde_json::json!({
        "jsonrpc": "2.0", "id": 1, "method": "initialize",
        "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                   "clientInfo": {"name": "shim-test", "version": "1"}}
    }));

    let mut line = String::new();
    stdout.read_line(&mut line).expect("shim replied");
    let response: serde_json::Value = serde_json::from_str(&line).expect("json-rpc line");
    assert_eq!(response["id"], 1);
    assert!(
        response["result"]["capabilities"].get("tools").is_some(),
        "expected a tools capability, got {response}"
    );

    // Notifications must not produce a reply, or the client desynchronises.
    send(serde_json::json!({
        "jsonrpc": "2.0", "method": "notifications/initialized", "params": {}
    }));
    send(serde_json::json!({ "jsonrpc": "2.0", "id": 2, "method": "tools/list" }));

    line.clear();
    stdout.read_line(&mut line).expect("shim replied");
    let response: serde_json::Value = serde_json::from_str(&line).expect("json-rpc line");
    assert_eq!(response["id"], 2, "a notification leaked a response line");
    let tools: Vec<String> = response["result"]["tools"]
        .as_array()
        .expect("tool list")
        .iter()
        .map(|t| t["name"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(tools.contains(&"read_document".to_string()));

    // The shim started a daemon that published itself under THOUGHT_HOME.
    let published = home.path().join("daemon.json");
    assert!(published.exists(), "shim did not start a daemon");

    let _ = shim.kill();
    let _ = shim.wait();
    if let Ok(body) = std::fs::read_to_string(&published)
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&body)
        && let Some(pid) = json["pid"].as_u64()
    {
        // The shim's daemon outlives the shim by design; clean it up.
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
}
