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
