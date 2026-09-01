//! Drives the daemon over real MCP: a spawned process, an HTTP transport, and
//! the JSON-RPC handshake. The tool layer is tested in `thought-mcp` without any
//! of that; what is under test here is specifically the wiring — discovery,
//! authentication, and whether an agent can actually reach the tools.

mod harness;

use harness::Daemon;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};
use std::sync::Arc;
use thought_mcp::{ReviewerAccess, ReviewerClient, Workspace};
use thoughtd::connections::{ConnectionRegistry, CredentialFiles};
use thoughtd::discovery::{self, Daemon as PublishedDaemon};

struct ReviewerFixture {
    connection_id: String,
    doc_id: String,
    block_id: String,
    version: String,
}

fn configure_reviewer(home: &std::path::Path) -> ReviewerFixture {
    let workspace = Arc::new(Workspace::open(home.join("thought.db")).unwrap());
    let document = workspace
        .create_document("Direct edit", &thought_mcp::ActorRef::editor())
        .unwrap();
    let registry = ConnectionRegistry::new(
        workspace.clone(),
        CredentialFiles::new(home.join("reviewer-credentials")),
    );
    let connection_id = registry
        .create(
            ReviewerClient::Codex,
            "Transport reviewer".into(),
            ReviewerAccess::all(),
            10,
        )
        .unwrap()
        .id;
    ReviewerFixture {
        connection_id,
        doc_id: document.doc_id,
        block_id: document.blocks[0].block_id.clone(),
        version: document.version,
    }
}

fn read_stdio_response(stdout: &mut BufReader<std::process::ChildStdout>) -> serde_json::Value {
    let mut line = String::new();
    stdout.read_line(&mut line).expect("shim replied");
    serde_json::from_str(&line).expect("json-rpc line")
}

fn send_stdio_message(stdin: &mut std::process::ChildStdin, body: serde_json::Value) {
    writeln!(stdin, "{body}").expect("write to shim");
    stdin.flush().expect("flush shim input");
}

fn tool_json(response: &serde_json::Value) -> serde_json::Value {
    let text = response["result"]["content"][0]["text"]
        .as_str()
        .expect("tool text");
    serde_json::from_str(text).expect("tool json")
}

#[test]
fn an_agent_drives_the_daemon_over_mcp() {
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
        "insert_blocks",
        "replace_text",
        "set_document_deleted",
        "delete_block",
        "search",
    ] {
        assert!(
            tools.contains(&expected.to_string()),
            "missing tool {expected}"
        );
    }

    let caller = serde_json::json!({
        "agent": "opus", "model": "claude-opus-5", "session": "test-run"
    });
    let mut create = caller.clone();
    create["title"] = "Transport".into();
    let doc = daemon.call("create_document", create);
    let doc_id = doc["doc_id"].as_str().expect("doc_id").to_string();
    let block = doc["blocks"][0]["block_id"]
        .as_str()
        .expect("block_id")
        .to_string();

    let mut edit = caller.clone();
    edit["doc_id"] = doc_id.clone().into();
    edit["block_id"] = block.into();
    edit["markdown"] = "# Transport\n\nReached the tools.".into();
    edit["version"] = doc["version"].clone();
    daemon.call("replace_block", edit);

    let view = daemon.read_document(&doc_id);
    assert_eq!(view["title"], "Transport");
    assert!(
        view["markdown"]
            .as_str()
            .unwrap()
            .contains("Reached the tools.")
    );

    // Anchors must point at lines that exist, or a follow-up edit misses.
    let markdown = view["markdown"].as_str().unwrap();
    let lines = markdown.lines().count();
    for block in view["blocks"].as_array().expect("blocks") {
        let end = block["line_end"].as_u64().expect("line_end") as usize;
        assert!(end <= lines, "anchor points past the end of the document");
    }

    let hits = daemon.call(
        "search",
        serde_json::json!({ "query": "Reached", "limit": 5 }),
    );
    assert_eq!(hits["hits"][0]["doc_id"], doc_id.as_str());

    let imported_markdown = "<!--thought:title-->\n# Imported\n\nFrom **Markdown**.";
    let mut import = caller;
    import["title"] = "Imported.md".into();
    import["initial_markdown"] = imported_markdown.into();
    let imported = daemon.call("create_document", import);
    let imported_view = daemon.read_document(imported["doc_id"].as_str().unwrap());
    assert_eq!(imported_view["markdown"], imported_markdown);
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
fn discovery_probe_verifies_the_published_bearer_token() {
    let daemon = Daemon::start();
    let published = PublishedDaemon {
        url: daemon.url.clone(),
        protocol_version: discovery::PROTOCOL_VERSION,
        instance_id: daemon.instance_id.clone(),
        token: daemon.token.clone(),
    };
    assert!(discovery::authenticated_reachable(&published));

    let mut wrong_instance = published.clone();
    wrong_instance.instance_id = discovery::random_token().unwrap();
    assert!(
        !discovery::authenticated_reachable(&wrong_instance),
        "a stale port must not receive the published bearer"
    );

    let mut wrong_token = published;
    wrong_token.token.push_str("-wrong");
    assert!(
        !discovery::authenticated_reachable(&wrong_token),
        "an unrelated or stale bearer credential must not validate the endpoint"
    );
}

#[test]
fn the_stdio_shim_refuses_to_replace_a_daemon_that_rejects_its_token() {
    let daemon = Daemon::start();
    let connection_id = daemon.create_reviewer();
    let published = daemon.home.path().join("daemon.json");
    let mut wrong: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&published).expect("discovery is readable"))
            .expect("discovery is json");
    wrong["token"] = "not-the-daemon-token".into();
    std::fs::write(&published, serde_json::to_vec_pretty(&wrong).unwrap()).unwrap();

    let output = Command::new(env!("CARGO_BIN_EXE_thought-mcp-stdio"))
        .args(["--connection", &connection_id])
        .env("THOUGHT_HOME", daemon.home.path())
        .stdin(Stdio::null())
        .output()
        .expect("spawn stdio shim");
    assert!(
        !output.status.success(),
        "the shim must not replace a published daemon"
    );
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("discovery record is invalid"),
        "the failure must explain how to resolve invalid discovery: {stderr}"
    );

    let original_daemon = PublishedDaemon {
        url: daemon.url.clone(),
        protocol_version: discovery::PROTOCOL_VERSION,
        instance_id: daemon.instance_id.clone(),
        token: daemon.token.clone(),
    };
    assert!(
        discovery::authenticated_reachable(&original_daemon),
        "the shim must leave the existing process running"
    );
}

#[test]
fn the_stdio_shim_replaces_stale_current_discovery() {
    let mut daemon = Daemon::start();
    let connection_id = daemon.create_reviewer();
    let published = daemon.home.path().join("daemon.json");
    let old_instance = daemon.instance_id.clone();
    daemon.stop_abruptly();
    assert!(published.exists(), "abrupt exit must leave stale discovery");

    let output = Command::new(env!("CARGO_BIN_EXE_thought-mcp-stdio"))
        .args(["--connection", &connection_id])
        .env("THOUGHT_HOME", daemon.home.path())
        .stdin(Stdio::null())
        .output()
        .expect("spawn stdio shim");
    assert!(
        output.status.success(),
        "shim did not recover stale discovery: {}",
        String::from_utf8_lossy(&output.stderr),
    );

    let current: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&published).unwrap()).unwrap();
    assert_ne!(
        current["instance_id"].as_str(),
        Some(old_instance.as_str()),
        "the new lock owner must publish a fresh instance"
    );
    if let Some(pid) = current["pid"].as_u64() {
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
}

/// The shim is what an MCP client actually spawns (AD-10). Its job is to make
/// "spawn a server on stdio" mean "reach the one daemon", so the case worth
/// testing is the one where no daemon is running yet.
#[test]
fn the_stdio_shim_starts_a_daemon_and_proxies_to_it() {
    let home = tempfile::tempdir().expect("temp dir");
    let fixture = configure_reviewer(home.path());
    assert!(
        !home.path().join("daemon.json").exists(),
        "test must begin with no daemon published"
    );

    let mut shim = Command::new(env!("CARGO_BIN_EXE_thought-mcp-stdio"))
        .args(["--connection", &fixture.connection_id])
        .env("THOUGHT_HOME", home.path())
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

    let response = read_stdio_response(&mut stdout);
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

    let response = read_stdio_response(&mut stdout);
    assert_eq!(response["id"], 2, "a notification leaked a response line");
    let tools: Vec<String> = response["result"]["tools"]
        .as_array()
        .expect("tool list")
        .iter()
        .map(|t| t["name"].as_str().unwrap_or_default().to_string())
        .collect();
    for expected in [
        "list_documents",
        "read_document",
        "list_suggestions",
        "suggest_change",
        "document_actors",
        "block_provenance",
        "document_lineage",
        "search",
        "request_direct_edit",
        "replace_block",
        "insert_blocks",
        "replace_text",
        "delete_block",
    ] {
        assert!(
            tools.contains(&expected.to_string()),
            "reviewer is missing {expected}: {tools:?}"
        );
    }
    for forbidden in ["create_document", "set_document_deleted"] {
        assert!(
            !tools.contains(&forbidden.to_string()),
            "reviewer was offered forbidden tool {forbidden}: {tools:?}"
        );
    }

    send(serde_json::json!({
        "jsonrpc": "2.0", "id": 3, "method": "tools/call",
        "params": {"name": "request_direct_edit", "arguments": {
            "doc_id": fixture.doc_id, "agent": "reviewer", "model": "reported-model"
        }}
    }));
    let requested = read_stdio_response(&mut stdout);
    let requested = tool_json(&requested);
    assert_eq!(requested["status"], "pending");
    assert_eq!(requested["request"]["document_title"], "Direct edit");
    let request_id = requested["request"]["request_id"].as_str().unwrap();

    send(serde_json::json!({
        "jsonrpc": "2.0", "id": 4, "method": "tools/call",
        "params": {"name": "replace_block", "arguments": {
            "doc_id": fixture.doc_id, "block_id": fixture.block_id,
            "markdown": "Not approved", "version": fixture.version,
            "agent": "reviewer", "model": "reported-model"
        }}
    }));
    let denied_edit = read_stdio_response(&mut stdout);
    assert!(
        denied_edit["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("active user-approved grant"),
        "unexpected pre-approval response: {denied_edit}"
    );

    let published = home.path().join("daemon.json");
    let discovery: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&published).unwrap()).unwrap();
    let editor_url = discovery["url"].as_str().unwrap().replace(
        "/mcp",
        &format!(
            "/editor/documents/{}/direct-edit-requests/{request_id}/approve",
            fixture.doc_id
        ),
    );
    ureq::post(&editor_url)
        .header(
            "Authorization",
            &format!("Bearer {}", discovery["token"].as_str().unwrap()),
        )
        .send_empty()
        .expect("editor approved direct edit");

    send(serde_json::json!({
        "jsonrpc": "2.0", "id": 5, "method": "tools/call",
        "params": {"name": "replace_block", "arguments": {
            "doc_id": fixture.doc_id, "block_id": fixture.block_id,
            "markdown": "Approved direct edit", "version": fixture.version,
            "agent": "reviewer", "model": "reported-model"
        }}
    }));
    let approved_edit = read_stdio_response(&mut stdout);
    assert!(
        approved_edit.get("result").is_some(),
        "approved edit failed: {approved_edit}"
    );

    send(serde_json::json!({
        "jsonrpc": "2.0", "id": 6, "method": "tools/call",
        "params": {"name": "read_document", "arguments": {"doc_id": fixture.doc_id}}
    }));
    let read = tool_json(&read_stdio_response(&mut stdout));
    assert_eq!(read["markdown"], "Approved direct edit");

    // The shim started a daemon that published itself under THOUGHT_HOME.
    assert!(published.exists(), "shim did not start a daemon");

    let access_url = discovery["url"]
        .as_str()
        .unwrap()
        .replace("/mcp", "/editor/direct-edit-access");
    let mut access = ureq::get(&access_url)
        .header(
            "Authorization",
            &format!("Bearer {}", discovery["token"].as_str().unwrap()),
        )
        .call()
        .expect("editor read direct edit access");
    let access: serde_json::Value = access.body_mut().read_json().unwrap();
    assert_eq!(access["grants"].as_array().unwrap().len(), 1);
    assert_eq!(access["grants"][0]["document_title"], "Direct edit");
    assert!(access["grants"][0].get("expires_at").is_none());

    // Normal client shutdown closes stdin. The shim translates EOF to an
    // authenticated MCP DELETE, and the daemon removes session authority
    // before the shim exits.
    drop(stdin);
    assert!(shim.wait().expect("wait for shim").success());

    let mut access = ureq::get(&access_url)
        .header(
            "Authorization",
            &format!("Bearer {}", discovery["token"].as_str().unwrap()),
        )
        .call()
        .expect("editor read direct edit access after EOF");
    let access: serde_json::Value = access.body_mut().read_json().unwrap();
    assert!(access["grants"].as_array().unwrap().is_empty());
    assert!(access["requests"].as_array().unwrap().is_empty());

    if let Ok(body) = std::fs::read_to_string(&published)
        && let Ok(json) = serde_json::from_str::<serde_json::Value>(&body)
        && let Some(pid) = json["pid"].as_u64()
    {
        // The shim's daemon outlives the shim by design; clean it up.
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
}

#[test]
fn the_stdio_shim_recovers_after_idle_session_cleanup_without_restoring_edit_access() {
    let home = tempfile::tempdir().expect("temp dir");
    let fixture = configure_reviewer(home.path());
    let mut shim = Command::new(env!("CARGO_BIN_EXE_thought-mcp-stdio"))
        .args(["--connection", &fixture.connection_id])
        .env("THOUGHT_HOME", home.path())
        .env("THOUGHT_TEST_MCP_IDLE_MS", "150")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn shim");
    let mut stdin = shim.stdin.take().expect("piped stdin");
    let mut stdout = BufReader::new(shim.stdout.take().expect("piped stdout"));

    send_stdio_message(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 1, "method": "initialize",
            "params": {"protocolVersion": "2025-06-18", "capabilities": {},
                       "clientInfo": {"name": "idle-recovery-test", "version": "1"}}
        }),
    );
    assert_eq!(read_stdio_response(&mut stdout)["id"], 1);
    send_stdio_message(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0", "method": "notifications/initialized", "params": {}
        }),
    );
    send_stdio_message(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 2, "method": "tools/call",
            "params": {"name": "request_direct_edit", "arguments": {
                "doc_id": fixture.doc_id, "agent": "reviewer", "model": "reported-model"
            }}
        }),
    );
    let requested = tool_json(&read_stdio_response(&mut stdout));
    let request_id = requested["request"]["request_id"].as_str().unwrap();

    let published = home.path().join("daemon.json");
    let discovery: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&published).unwrap()).unwrap();
    let editor_token = discovery["token"].as_str().unwrap();
    let approve_url = discovery["url"].as_str().unwrap().replace(
        "/mcp",
        &format!(
            "/editor/documents/{}/direct-edit-requests/{request_id}/approve",
            fixture.doc_id
        ),
    );
    ureq::post(&approve_url)
        .header("Authorization", &format!("Bearer {editor_token}"))
        .send_empty()
        .expect("editor approved direct edit");

    let access_url = discovery["url"]
        .as_str()
        .unwrap()
        .replace("/mcp", "/editor/direct-edit-access");
    let access_is_empty = || {
        let mut response = ureq::get(&access_url)
            .header("Authorization", &format!("Bearer {editor_token}"))
            .call()
            .expect("editor read direct edit access");
        let access: serde_json::Value = response.body_mut().read_json().unwrap();
        access["grants"].as_array().unwrap().is_empty()
    };
    assert!(!access_is_empty(), "approval did not create a grant");

    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(3);
    while !access_is_empty() && std::time::Instant::now() < deadline {
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(
        access_is_empty(),
        "idle transport cleanup did not revoke its grant"
    );

    // The shim still holds the terminated session ID. A read transparently
    // replays the handshake on a fresh session instead of failing forever.
    send_stdio_message(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 3, "method": "tools/call",
            "params": {"name": "read_document", "arguments": {"doc_id": fixture.doc_id}}
        }),
    );
    let read = tool_json(&read_stdio_response(&mut stdout));
    assert_eq!(read["doc_id"], fixture.doc_id);

    // Reinitialization gets a different daemon-issued session ID, so it must
    // not inherit direct-edit authority from the closed session.
    send_stdio_message(
        &mut stdin,
        serde_json::json!({
            "jsonrpc": "2.0", "id": 4, "method": "tools/call",
            "params": {"name": "replace_block", "arguments": {
                "doc_id": fixture.doc_id, "block_id": fixture.block_id,
                "markdown": "Must be approved again", "version": fixture.version,
                "agent": "reviewer", "model": "reported-model"
            }}
        }),
    );
    let denied = read_stdio_response(&mut stdout);
    assert!(
        denied["error"]["message"]
            .as_str()
            .unwrap_or_default()
            .contains("active user-approved grant"),
        "replacement session inherited edit access: {denied}"
    );

    drop(stdin);
    assert!(shim.wait().expect("wait for shim").success());
    if let Some(pid) = discovery["pid"].as_u64() {
        let _ = Command::new("kill").arg(pid.to_string()).status();
    }
}
