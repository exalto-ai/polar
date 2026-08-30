mod harness;

use harness::Daemon;

#[test]
fn window_lifecycle_operations_do_not_self_assert_over_mcp() {
    let daemon = Daemon::start();
    let created = daemon.editor_post(
        "/editor/documents",
        serde_json::json!({ "title": "Import", "markdown": "Imported text" }),
    );
    let doc_id = created["doc_id"].as_str().unwrap();

    let lineage = daemon
        .connect()
        .call("document_lineage", serde_json::json!({ "doc_id": doc_id }));
    let source = &lineage["summary"]["contributions"][0]["source"];
    assert_eq!(source["ingress"], "imported");
    assert_eq!(source["assurance"], "observed");
    assert_eq!(source["alignment"], "exact");

    daemon.editor_post(
        &format!("/editor/documents/{doc_id}/deletion"),
        serde_json::json!({ "deleted": true }),
    );
    let trashed = daemon.call(
        "list_documents",
        serde_json::json!({ "trashed": true, "limit": 10 }),
    );
    assert_eq!(trashed["documents"][0]["doc_id"], doc_id);
}

#[test]
fn provider_chat_can_create_only_a_pending_reported_suggestion() {
    let daemon = Daemon::start();
    let created = daemon.editor_post(
        "/editor/documents",
        serde_json::json!({ "title": "Draft", "markdown": "Original text" }),
    );
    let doc_id = created["doc_id"].as_str().unwrap();
    let lineage = daemon
        .connect()
        .call("document_lineage", serde_json::json!({ "doc_id": doc_id }));
    let wording_revision = lineage["current_wording_revision"].as_str().unwrap();

    let outcome = daemon.editor_post(
        &format!("/editor/documents/{doc_id}/suggestions/pro-chat"),
        serde_json::json!({
            "request_id": "chat-request-1",
            "provider": "openai",
            "requested_model": "gpt-test",
            "reported_model": "gpt-test-2026",
            "assistant_text": "Suggested ending",
            "wording_revision": wording_revision,
            "after": { "kind": "end" }
        }),
    );

    assert_eq!(outcome["suggestion"]["state"], "pending");
    assert_eq!(outcome["suggestion"]["patch"]["kind"], "insert_blocks");
    assert_eq!(
        outcome["suggestion"]["proposer"]["label"],
        "OpenAI chat (reported)"
    );
    let unchanged = daemon.read_document(doc_id);
    assert_eq!(unchanged["markdown"], "Original text");
}
