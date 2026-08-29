//! Starting a real daemon and talking to it, shared by the transport and sync
//! tests. Both exercise the wiring, so both need the same setup.

// Each integration test compiles its own copy of this module, so anything only
// one of them uses reads as dead here.
#![allow(dead_code, unused_imports)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};
#[cfg(unix)]
use std::time::{Duration, Instant};

pub use thoughtd::sync::Frame;

pub struct Daemon {
    child: Child,
    pub url: String,
    pub protocol_version: u32,
    pub pid: u32,
    /// Platform bearer, matching the discovery field name.
    pub token: String,
    pub instance_id: String,
    pub reviewer_token: String,
    pub connection_id: String,
    pub home: tempfile::TempDir,
    agent: ureq::Agent,
    session: std::cell::RefCell<Option<String>>,
    id: std::cell::Cell<u32>,
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Daemon {
    pub fn start() -> Daemon {
        let home = tempfile::tempdir().expect("temp dir");
        let mut child = Command::new(env!("CARGO_BIN_EXE_thoughtd"))
            .env("THOUGHT_HOME", home.path())
            .env("THOUGHT_CREDENTIAL_BACKEND", "file")
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn thoughtd");

        // Wait for the readiness line rather than sleeping: a fixed sleep is
        // either flaky or slow, and usually both.
        // Then keep draining. The daemon prints three startup lines, and dropping
        // the reader after the first closes the pipe — its next write gets EPIPE
        // and the process dies, which surfaced much later and somewhere else as
        // a connection refused on an unrelated request.
        let stderr = child.stderr.take().expect("piped stderr");
        let mut reader = BufReader::new(stderr);

        // Scan for the readiness line rather than assuming it comes first: the
        // daemon also logs to stderr, so anything else it has to say at startup
        // would otherwise be mistaken for a failure to start.
        let mut line = String::new();
        let mut startup = String::new();
        loop {
            line.clear();
            let read = reader
                .read_line(&mut line)
                .expect("daemon stderr is readable");
            assert!(
                read > 0,
                "daemon exited before reporting a port:\n{startup}"
            );
            startup.push_str(&line);
            if line.contains("listening on") {
                break;
            }
        }

        std::thread::spawn(move || {
            let mut sink = String::new();
            while reader.read_line(&mut sink).unwrap_or(0) > 0 {
                sink.clear();
            }
        });

        let config: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(home.path().join("daemon.json")).expect("discovery file"),
        )
        .expect("valid discovery json");
        let token = config["token"].as_str().expect("token").to_string();
        let protocol_version = config["protocol_version"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .expect("protocol_version");
        assert_eq!(protocol_version, thoughtd::discovery::PROTOCOL_VERSION);
        let pid = config["pid"]
            .as_u64()
            .and_then(|value| u32::try_from(value).ok())
            .expect("pid");

        let agent = ureq::Agent::new_with_config(
            ureq::Agent::config_builder()
                .max_idle_connections(0)
                .build(),
        );
        let editor_base = config["url"]
            .as_str()
            .expect("url")
            .strip_suffix("/mcp")
            .expect("MCP suffix");
        let mut response = agent
            .post(format!("{editor_base}/editor/reviewer-connections"))
            .header("Authorization", &format!("Bearer {token}"))
            .send_json(serde_json::json!({
                "client": "codex",
                "display_label": "Test reviewer",
                "permissions": {
                    "document_scope": "all",
                    "can_read": true,
                    "can_edit": false,
                    "can_create": false,
                    "can_trash": false,
                    "document_ids": []
                }
            }))
            .expect("create test reviewer");
        let created: serde_json::Value = response.body_mut().read_json().expect("reviewer JSON");
        let connection_id = created["connection"]["id"]
            .as_str()
            .expect("connection id")
            .to_string();
        let reviewer_token = std::fs::read_to_string(
            home.path()
                .join("reviewer-credentials")
                .join(format!("{connection_id}.credential")),
        )
        .expect("test reviewer credential");

        Daemon {
            child,
            url: config["url"].as_str().expect("url").to_string(),
            protocol_version,
            pid,
            token,
            instance_id: config["instance_id"]
                .as_str()
                .expect("instance_id")
                .to_string(),
            reviewer_token,
            connection_id,
            home,
            // No idle pooling: a keep-alive socket the server has since closed
            // fails on write with ECONNRESET, which would read as a product bug
            // rather than the transport artefact it is.
            agent,
            session: std::cell::RefCell::new(None),
            id: std::cell::Cell::new(0),
        }
    }

    pub fn stop_abruptly(&mut self) {
        self.child.kill().expect("kill daemon");
        self.child.wait().expect("wait for daemon");
    }

    pub fn sync_url(&self) -> String {
        self.url
            .replace("http://", "ws://")
            .replace("/mcp", "/sync")
    }

    pub fn editor_post(&self, path: &str, body: serde_json::Value) -> serde_json::Value {
        let url = self.url.replace("/mcp", path);
        let mut response = self
            .agent
            .post(&url)
            .header("Authorization", &format!("Bearer {}", self.token))
            .send_json(&body)
            .expect("editor request succeeded");
        response
            .body_mut()
            .read_json()
            .expect("editor response is json")
    }

    pub fn logs(&self) -> String {
        let mut paths = std::fs::read_dir(self.home.path())
            .expect("read daemon home")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().is_some_and(|extension| extension == "log"))
            .collect::<Vec<_>>();
        paths.sort();
        paths
            .into_iter()
            .map(|path| std::fs::read_to_string(path).expect("read daemon log"))
            .collect()
    }

    #[cfg(unix)]
    pub fn interrupt_and_wait(&mut self, timeout: Duration) -> std::process::ExitStatus {
        let status = Command::new("kill")
            .args(["-INT", &self.child.id().to_string()])
            .status()
            .expect("send SIGINT to thoughtd");
        assert!(status.success(), "kill could not deliver SIGINT");

        let deadline = Instant::now() + timeout;
        loop {
            if let Some(status) = self.child.try_wait().expect("poll thoughtd exit") {
                return status;
            }
            assert!(
                Instant::now() < deadline,
                "thoughtd did not exit within {timeout:?} after SIGINT"
            );
            std::thread::sleep(Duration::from_millis(10));
        }
    }

    pub fn rpc(&self, method: &str, params: serde_json::Value) -> serde_json::Value {
        self.id.set(self.id.get() + 1);
        let mut body = serde_json::json!({
            "jsonrpc": "2.0", "method": method, "params": params
        });
        if !method.starts_with("notifications/") {
            body["id"] = self.id.get().into();
        }

        // Built per attempt: a RequestBuilder is consumed by sending, and a
        // transport failure needs a fresh one.
        let build = || {
            let mut request = self
                .agent
                .post(&self.url)
                .header("Authorization", &format!("Bearer {}", self.reviewer_token))
                .header("Accept", "application/json, text/event-stream");
            if let Some(session) = self.session.borrow().as_ref() {
                request = request.header("Mcp-Session-Id", session);
            }
            request
        };

        // Retry once on a *transport* failure. Disabling idle pooling is not
        // enough on its own: a keep-alive socket the server has already closed
        // still fails on write with ECONNRESET. A connection that died before
        // delivering anything has nothing to duplicate, so retrying is safe —
        // the same reasoning as the stdio shim's retry.
        let mut response = match build().send_json(&body) {
            Err(ureq::Error::ConnectionFailed | ureq::Error::Io(_)) => build()
                .send_json(&body)
                .expect("request succeeded on retry"),
            other => other.expect("request succeeded"),
        };
        if self.session.borrow().is_none()
            && let Some(id) = response.headers().get("mcp-session-id")
        {
            *self.session.borrow_mut() = Some(id.to_str().expect("ascii session id").to_string());
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

    pub fn session_id(&self) -> Option<String> {
        self.session.borrow().clone()
    }

    /// Send one RPC with an explicit capability and caller-owned session. The
    /// full JSON-RPC envelope is returned so denial tests can inspect errors.
    pub fn raw_rpc_with_token(
        &self,
        token: &str,
        session: &mut Option<String>,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, ureq::Error> {
        self.id.set(self.id.get() + 1);
        let body = serde_json::json!({
            "jsonrpc": "2.0",
            "id": self.id.get(),
            "method": method,
            "params": params
        });
        let mut request = self
            .agent
            .post(&self.url)
            .header("Authorization", &format!("Bearer {token}"))
            .header("Accept", "application/json, text/event-stream");
        if let Some(session_id) = session.as_ref() {
            request = request.header("Mcp-Session-Id", session_id);
        }
        let mut response = request.send_json(&body)?;
        if session.is_none()
            && let Some(id) = response.headers().get("mcp-session-id")
        {
            *session = Some(id.to_str().expect("ascii session id").to_string());
        }
        let raw = response.body_mut().read_to_string()?;
        for line in raw.lines() {
            if let Some(payload) = line.strip_prefix("data: ")
                && !payload.trim().is_empty()
            {
                return Ok(serde_json::from_str(payload).expect("valid json-rpc"));
            }
        }
        Ok(serde_json::Value::Null)
    }

    pub fn connect(&self) -> &Daemon {
        self.rpc(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "thought-test", "version": "1"}
            }),
        );
        self.rpc("notifications/initialized", serde_json::json!({}));
        self
    }

    pub fn call(&self, name: &str, args: serde_json::Value) -> serde_json::Value {
        let result = self.rpc(
            "tools/call",
            serde_json::json!({
                "name": name, "arguments": args
            }),
        );
        let text = result["content"][0]["text"].as_str().expect("text content");
        serde_json::from_str(text).expect("tool returned json")
    }

    pub fn create_document(&self, title: &str) -> String {
        self.create_document_with_markdown(title, None)
    }

    pub fn create_document_with_markdown(
        &self,
        title: &str,
        initial_markdown: Option<&str>,
    ) -> String {
        self.connect();
        let base = self.url.strip_suffix("/mcp").expect("MCP URL suffix");
        let body = match (title.trim(), initial_markdown) {
            ("", markdown) => {
                serde_json::json!({ "title": "Untitled", "markdown": markdown.unwrap_or("") })
            }
            (title, Some(markdown)) => {
                serde_json::json!({ "title": title, "markdown": markdown })
            }
            (title, None) => serde_json::json!({ "title": title }),
        };
        let mut response = self
            .agent
            .post(format!("{base}/editor/documents"))
            .header("Authorization", &format!("Bearer {}", self.token))
            .send_json(body)
            .expect("editor creates test document");
        let doc: serde_json::Value = response.body_mut().read_json().expect("document JSON");
        doc["doc_id"].as_str().expect("doc_id").to_string()
    }

    pub fn read_document(&self, doc_id: &str) -> serde_json::Value {
        self.call("read_document", serde_json::json!({ "doc_id": doc_id }))
    }
}
