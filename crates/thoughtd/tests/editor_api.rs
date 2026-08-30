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
fn completed_chat_output_enters_through_the_existing_suggestion_path() {
    let daemon = Daemon::start();
    let created = daemon.editor_post(
        "/editor/documents",
        serde_json::json!({ "title": "Draft", "markdown": "Original wording" }),
    );
    let doc_id = created["doc_id"].as_str().unwrap();
    let wording_revision = daemon
        .connect()
        .call("document_lineage", serde_json::json!({ "doc_id": doc_id }))["current_wording_revision"]
        .as_str()
        .unwrap()
        .to_string();

    let proposed = daemon.editor_post(
        &format!("/editor/documents/{doc_id}/suggestions/pro-chat"),
        serde_json::json!({
            "request_id": "chat-suggestion-1",
            "turn_id": "turn-1",
            "provider": "openai",
            "requested_model": "gpt-requested",
            "reported_model": "gpt-reported",
            "assistant_text": "## Suggested\n\nNew paragraph.",
            "wording_revision": wording_revision,
            "after": { "kind": "end" }
        }),
    );
    assert_eq!(proposed["suggestion"]["state"], "pending");
    assert_eq!(
        proposed["suggestion"]["proposer"]["connection_id"],
        "pro-chat:openai"
    );
    assert_eq!(
        proposed["suggestion"]["proposer"]["source_label"],
        "OpenAI chat (reported)"
    );

    let document = daemon.call("read_document", serde_json::json!({ "doc_id": doc_id }));
    assert_eq!(document["markdown"], "Original wording");
}
