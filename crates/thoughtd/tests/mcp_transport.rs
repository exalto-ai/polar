//! Drives the daemon over real MCP: a spawned process, an HTTP transport, and
//! the JSON-RPC handshake. The tool layer is tested in `thought-mcp` without any
//! of that; what is under test here is specifically the wiring — discovery,
//! authentication, and whether an agent can actually reach the tools.

mod harness;

use harness::Daemon;
use std::io::{BufRead, BufReader};
use std::process::{Command, Stdio};
use thoughtd::discovery::{self, Daemon as PublishedDaemon};

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
        "document_lineage",
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
fn an_mcp_caller_cannot_spoof_a_local_actor_kind() {
    let daemon = Daemon::start();
    daemon.connect();

    for legacy_kind in ["human", "editor"] {
        let doc = daemon.call(
            "create_document",
            serde_json::json!({
                "title": format!("Reported {legacy_kind} review"),
                "agent": "Claude",
                "model": "claimed-model",
                "session": format!("spoof-{legacy_kind}"),
                "kind": legacy_kind
            }),
        );
        let doc_id = doc["doc_id"].as_str().expect("doc_id");

        // Public MCP is AI ingress even when an older caller sends a local
        // actor label. It must remain visible in the app's agent activity list.
        let actors = daemon.call("document_actors", serde_json::json!({ "doc_id": doc_id }));
        assert_eq!(actors["actors"][0]["kind"], "agent");

        // Provenance is owned by the MCP transport, not by caller arguments.
        let lineage = daemon.call("document_lineage", serde_json::json!({ "doc_id": doc_id }));
        let source = &lineage["summary"]["contributions"][0]["source"];
        assert_eq!(source["label"], "Claude (reported)");
        assert_eq!(source["ingress"], "mcp");
        assert_eq!(source["assurance"], "reported");
    }
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
fn discovery_probe_verifies_the_published_capabilities() {
    let daemon = Daemon::start();
    let published = PublishedDaemon {
        url: daemon.url.clone(),
        protocol_version: daemon.protocol_version,
        token: daemon.token.clone(),
        editor_token: daemon.editor_token.clone(),
    };
    assert!(discovery::authenticated_reachable(&published));
    assert!(discovery::editor_authenticated_reachable(&published));

    let mut wrong_token = published;
    wrong_token.token.push_str("-wrong");
    assert!(
        !discovery::authenticated_reachable(&wrong_token),
        "an unrelated or stale bearer credential must not validate the endpoint"
    );
    assert!(
        discovery::editor_authenticated_reachable(&wrong_token),
        "the editor capability remains independently valid"
    );

    wrong_token.token = daemon.token.clone();
    wrong_token.editor_token.push_str("-wrong");
    assert!(
        !discovery::editor_authenticated_reachable(&wrong_token),
        "an unrelated editor capability must not validate the sync endpoint"
    );
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

    let mut shim = Command::new(env!("CARGO_BIN_EXE_thought-mcp-stdio"))
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
