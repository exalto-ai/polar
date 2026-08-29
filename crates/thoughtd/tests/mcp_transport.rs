//! Real HTTP transport coverage for platform and reviewer credentials.

mod harness;

use harness::Daemon;
use thoughtd::discovery::{self, Daemon as PublishedDaemon};

#[test]
fn a_scoped_reviewer_reads_over_mcp() {
    let daemon = Daemon::start();
    let doc_id = daemon.create_document_with_markdown("Transport", Some("Reached the tools."));
    daemon.connect();

    let tools: Vec<String> = daemon.rpc("tools/list", serde_json::json!({}))["tools"]
        .as_array()
        .expect("tool list")
        .iter()
        .filter_map(|tool| tool["name"].as_str().map(str::to_string))
        .collect();
    assert!(tools.contains(&"read_document".to_string()));

    let view = daemon.read_document(&doc_id);
    assert_eq!(view["title"], "Reached the tools.");
    assert_eq!(view["markdown"], "Reached the tools.");

    let hits = daemon.call(
        "search",
        serde_json::json!({ "query": "Reached", "limit": 5 }),
    );
    assert_eq!(hits["hits"][0]["doc_id"], doc_id);
}

#[test]
fn the_endpoint_refuses_an_unauthenticated_client() {
    let daemon = Daemon::start();
    let agent = ureq::Agent::new_with_config(
        ureq::Agent::config_builder()
            .max_idle_connections(0)
            .build(),
    );
    let response = agent
        .post(&daemon.url)
        .header("Accept", "application/json, text/event-stream")
        .send_json(serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-06-18",
                "capabilities": {},
                "clientInfo": {"name": "intruder", "version": "1"}
            }
        }));

    assert!(matches!(response, Err(ureq::Error::StatusCode(401))));
}

#[test]
fn discovery_probe_verifies_the_platform_bearer() {
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
    assert!(!discovery::authenticated_reachable(&wrong_instance));

    let mut wrong_token = published;
    wrong_token.token.push_str("-wrong");
    assert!(!discovery::authenticated_reachable(&wrong_token));
}
