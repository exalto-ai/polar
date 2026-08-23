//! Drives the daemon over real MCP: a spawned process, an HTTP transport, and
//! the JSON-RPC handshake. The tool layer is tested in `polar-mcp` without any
//! of that; what is under test here is specifically the wiring — discovery,
//! authentication, and whether an agent can actually reach the tools.

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

struct Daemon {
    child: Child,
    url: String,
    token: String,
    _home: tempfile::TempDir,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn start() -> Daemon {
    let home = tempfile::tempdir().expect("temp dir");
    let mut child = Command::new(env!("CARGO_BIN_EXE_polard"))
        .env("POLAR_HOME", home.path())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn polard");

    // Wait for the readiness line rather than sleeping: a fixed sleep is either
    // flaky or slow, and usually both.
    let stderr = child.stderr.take().expect("piped stderr");
    let mut reader = BufReader::new(stderr);
    let mut line = String::new();
    reader
        .read_line(&mut line)
        .expect("daemon printed a startup line");
    assert!(
        line.contains("listening"),
        "unexpected startup output: {line}"
    );

    let config: serde_json::Value = {
        let path = home.path().join("daemon.json");
        let body = std::fs::read_to_string(&path).expect("discovery file");
        serde_json::from_str(&body).expect("valid discovery json")
    };

    Daemon {
        child,
        url: config["url"].as_str().expect("url").to_string(),
        token: config["token"].as_str().expect("token").to_string(),
        _home: home,
    }
}

struct Client {
    url: String,
    token: String,
    session: Option<String>,
    id: u32,
}

impl Client {
    fn rpc(&mut self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.id += 1;
        let mut body = serde_json::json!({
            "jsonrpc": "2.0", "method": method, "params": params
        });
        let notification = method.starts_with("notifications/");
        if !notification {
            body["id"] = self.id.into();
        }

        let mut req = ureq::post(&self.url)
            .header("Authorization", &format!("Bearer {}", self.token))
            .header("Accept", "application/json, text/event-stream");
        if let Some(session) = &self.session {
            req = req.header("Mcp-Session-Id", session);
        }

        let mut response = req.send_json(&body).expect("request succeeded");
        if self.session.is_none()
            && let Some(id) = response.headers().get("mcp-session-id")
        {
            self.session = Some(id.to_str().expect("ascii session id").to_string());
        }
        let raw = response.body_mut().read_to_string().expect("body");

        // The streamable-HTTP transport answers as SSE.
        for line in raw.lines() {
            if let Some(payload) = line.strip_prefix("data: ")
                && !payload.trim().is_empty()
            {
                let msg: serde_json::Value = serde_json::from_str(payload).expect("valid json-rpc");
                if let Some(err) = msg.get("error") {
                    panic!("{method} failed: {err}");
                }
                if let Some(result) = msg.get("result") {
                    return result.clone();
                }
            }
        }
        serde_json::Value::Null
    }

    /// Tool results arrive as text content holding JSON.
    fn call(&mut self, name: &str, args: serde_json::Value) -> serde_json::Value {
        let result = self.rpc(
            "tools/call",
            serde_json::json!({
                "name": name, "arguments": args
            }),
        );
        let text = result["content"][0]["text"].as_str().expect("text content");
        serde_json::from_str(text).expect("tool returned json")
    }

    fn connect(daemon: &Daemon) -> Client {
        let mut client = Client {
            url: daemon.url.clone(),
            token: daemon.token.clone(),
            session: None,
            id: 0,
        };
        client.rpc(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "polar-test", "version": "1"}
            }),
        );
        client.rpc("notifications/initialized", serde_json::json!({}));
        client
    }
}

#[test]
fn an_agent_drives_the_daemon_over_mcp() {
    let daemon = start();
    let mut client = Client::connect(&daemon);

    let tools: Vec<String> = client.rpc("tools/list", serde_json::json!({}))["tools"]
        .as_array()
        .expect("tool list")
        .iter()
        .map(|t| t["name"].as_str().unwrap_or_default().to_string())
        .collect();
    for expected in [
        "create_document",
        "read_document",
        "replace_block",
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
    let doc = client.call("create_document", create);
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
    client.call("replace_block", edit);

    let view = client.call("read_document", serde_json::json!({ "doc_id": doc_id }));
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

    let hits = client.call(
        "search",
        serde_json::json!({ "query": "Reached", "limit": 5 }),
    );
    assert_eq!(hits["hits"][0]["doc_id"], doc_id.as_str());
}

#[test]
fn the_endpoint_refuses_an_unauthenticated_client() {
    let daemon = start();
    let response = ureq::post(&daemon.url)
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
