//! Starting a real daemon and talking to it, shared by the transport and sync
//! tests. Both exercise the wiring, so both need the same setup.

// Each integration test compiles its own copy of this module, so anything only
// one of them uses reads as dead here.
#![allow(dead_code, unused_imports)]

use std::io::{BufRead, BufReader};
use std::process::{Child, Command, Stdio};

pub use polard::sync::Frame;

pub struct Daemon {
    child: Child,
    pub url: String,
    pub token: String,
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
        let mut child = Command::new(env!("CARGO_BIN_EXE_polard"))
            .env("POLAR_HOME", home.path())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn polard");

        // Wait for the readiness line rather than sleeping: a fixed sleep is
        // either flaky or slow, and usually both.
        // Then keep draining. The daemon prints three startup lines, and dropping
        // the reader after the first closes the pipe — its next write gets EPIPE
        // and the process dies, which surfaced much later and somewhere else as
        // a connection refused on an unrelated request.
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

        Daemon {
            child,
            url: config["url"].as_str().expect("url").to_string(),
            token: config["token"].as_str().expect("token").to_string(),
            home,
            // No idle pooling: a keep-alive socket the server has since closed
            // fails on write with ECONNRESET, which would read as a product bug
            // rather than the transport artefact it is.
            agent: ureq::Agent::new_with_config(
                ureq::Agent::config_builder()
                    .max_idle_connections(0)
                    .build(),
            ),
            session: std::cell::RefCell::new(None),
            id: std::cell::Cell::new(0),
        }
    }

    pub fn sync_url(&self) -> String {
        self.url
            .replace("http://", "ws://")
            .replace("/mcp", "/sync")
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
                .header("Authorization", &format!("Bearer {}", self.token))
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

    pub fn connect(&self) -> &Daemon {
        self.rpc(
            "initialize",
            serde_json::json!({
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "polar-test", "version": "1"}
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
        self.connect();
        let doc = self.call(
            "create_document",
            serde_json::json!({
                "title": title, "agent": "test", "session": "harness"
            }),
        );
        doc["doc_id"].as_str().expect("doc_id").to_string()
    }

    pub fn read_document(&self, doc_id: &str) -> serde_json::Value {
        self.call("read_document", serde_json::json!({ "doc_id": doc_id }))
    }
}
