//! The sync endpoint (M2.1), driven by real WebSocket peers.
//!
//! What matters here is that the *protocol* works, because M3's relay reuses
//! it. If the editor had a private channel this test would only prove the
//! editor works.

mod harness;

use futures_util::{SinkExt, StreamExt};
use harness::{Daemon, Frame};
use thought_core::Document;
use thought_mcp::lineage::ProseMirrorRangeHint;
use thought_schema::{Node, normalize};
use thoughtd::sync::{AnchoredMutation, LocalInputSource};
use tokio_tungstenite::tungstenite::Message;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(daemon: &Daemon) -> Socket {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = daemon.sync_url().into_client_request().expect("request");
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", daemon.editor_token)
            .parse()
            .expect("header"),
    );
    let (socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("sync endpoint accepted the connection");
    socket
}

async fn connect_as_browser(daemon: &Daemon) -> Socket {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = daemon.sync_url().into_client_request().expect("request");
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        format!("thought.v1, thought.token.{}", daemon.editor_token)
            .parse()
            .expect("header"),
    );
    let (socket, response) = tokio_tungstenite::connect_async(request)
        .await
        .expect("browser editor capability accepted");
    assert_eq!(
        response
            .headers()
            .get("Sec-WebSocket-Protocol")
            .and_then(|value| value.to_str().ok()),
        Some("thought.v1")
    );
    socket
}

async fn send(socket: &mut Socket, frame: Frame) {
    socket
        .send(Message::Binary(frame.encode().into()))
        .await
        .expect("send frame");
}

async fn recv(socket: &mut Socket) -> Frame {
    loop {
        let message = tokio::time::timeout(std::time::Duration::from_secs(5), socket.next())
            .await
            .expect("frame arrived within 5s")
            .expect("stream open")
            .expect("no websocket error");
        if let Message::Binary(bytes) = message {
            return Frame::decode(&bytes).expect("decodable frame");
        }
    }
}

async fn recv_ack(socket: &mut Socket, doc_id: &str) {
    loop {
        match recv(socket).await {
            Frame::Ack { doc_id: acked } if acked == doc_id => return,
            Frame::Error { message, .. } => {
                panic!("update failed instead of being acked: {message}")
            }
            // Broadcasts can be interleaved with replies because the workspace
            // observer fans out the committed update to every subscriber.
            _ => continue,
        }
    }
}

async fn assert_no_additional_frame(socket: &mut Socket, context: &str) {
    if let Ok(frame) =
        tokio::time::timeout(std::time::Duration::from_millis(250), recv(socket)).await
    {
        panic!("{context}: unexpected additional frame {frame:?}");
    }
}

fn editor_edit_count(daemon: &Daemon, doc_id: &str) -> i64 {
    let actors = daemon.call("document_actors", serde_json::json!({ "doc_id": doc_id }));
    actors["actors"]
        .as_array()
        .expect("actors")
        .iter()
        .find(|actor| actor["actor_id"] == thoughtd::EDITOR_ACTOR_ID)
        .and_then(|actor| actor["edits"].as_i64())
        .expect("local editor activity")
}

#[tokio::test]
async fn a_peer_syncs_then_receives_another_peers_edit() {
    let daemon = Daemon::start();
    let doc_id = daemon.create_document("Sync");

    // Peer A joins knowing nothing and is caught up.
    let mut a = connect(&daemon).await;
    send(
        &mut a,
        Frame::Subscribe {
            doc_id: doc_id.clone(),
            state_vector: vec![],
        },
    )
    .await;
    let local = Document::new();
    match recv(&mut a).await {
        Frame::Sync { update, .. } => local.apply_update(&update).expect("valid sync"),
        other => panic!("expected SYNC, got {other:?}"),
    }
    // A named document is seeded with its title as a heading and a paragraph
    // to type in, so catching up means seeing both.
    assert_eq!(local.blocks().len(), 2, "caught up to the created document");

    // Peer B joins and subscribes too.
    let mut b = connect(&daemon).await;
    send(
        &mut b,
        Frame::Subscribe {
            doc_id: doc_id.clone(),
            state_vector: vec![],
        },
    )
    .await;
    let remote = Document::new();
    match recv(&mut b).await {
        Frame::Sync { update, .. } => remote.apply_update(&update).expect("valid sync"),
        other => panic!("expected SYNC, got {other:?}"),
    }

    // A edits and publishes only the delta.
    let before = local.state_vector();
    let block = local.blocks()[1].block_id.clone();
    local
        .replace_block(
            &block,
            &normalize(&Node::element(
                "paragraph",
                vec![Node::text("typed in the window", vec![])],
            )),
        )
        .expect("replace");
    let delta = local.diff_since(&before);
    send(
        &mut a,
        Frame::SourcedUpdate {
            doc_id: doc_id.clone(),
            source: LocalInputSource::Written,
            update: delta.clone(),
        },
    )
    .await;

    // ACK is sent only after `apply_peer_update` completes its SQLite commit.
    recv_ack(&mut a, &doc_id).await;
    let view = daemon.read_document(&doc_id);
    assert!(
        view["markdown"]
            .as_str()
            .expect("markdown")
            .contains("typed in the window"),
        "ACK arrived before the edit reached SQLite"
    );

    // B is told, without having asked.
    match recv(&mut b).await {
        Frame::Broadcast { update, .. } => remote.apply_update(&update).expect("valid update"),
        other => panic!("expected BROADCAST, got {other:?}"),
    }
    assert_no_additional_frame(&mut b, "one committed update must fan out exactly once").await;
    assert_eq!(
        remote.block_text(&block).expect("block"),
        "typed in the window"
    );

    // A legacy window may send the old unlabelled frame. It is accepted as an
    // Unknown source, and a no-op resend is still acknowledged after the
    // daemon confirms the document already contains it.
    send(
        &mut a,
        Frame::Update {
            doc_id: doc_id.clone(),
            update: delta,
        },
    )
    .await;
    recv_ack(&mut a, &doc_id).await;
}

#[tokio::test]
async fn an_anchored_batch_commits_every_mutation_before_one_ack() {
    let daemon = Daemon::start();
    let doc_id = daemon.create_document("");
    let mut socket = connect(&daemon).await;
    send(
        &mut socket,
        Frame::Subscribe {
            doc_id: doc_id.clone(),
            state_vector: vec![],
        },
    )
    .await;

    let local = Document::new();
    match recv(&mut socket).await {
        Frame::Sync { update, .. } => local.apply_update(&update).expect("valid sync"),
        other => panic!("expected SYNC, got {other:?}"),
    }
    let block = local.blocks()[0].block_id.clone();

    let before_first = local.state_vector();
    local
        .replace_block(
            &block,
            &normalize(&Node::element(
                "paragraph",
                vec![Node::text("first", vec![])],
            )),
        )
        .expect("first replacement");
    let first = local.diff_since(&before_first);

    let before_second = local.state_vector();
    local
        .replace_block(
            &block,
            &normalize(&Node::element(
                "paragraph",
                vec![Node::text("second", vec![])],
            )),
        )
        .expect("second replacement");
    let second = local.diff_since(&before_second);

    send(
        &mut socket,
        Frame::AnchoredBatch {
            doc_id: doc_id.clone(),
            mutations: vec![
                AnchoredMutation {
                    source: LocalInputSource::Written,
                    client_event_id: "keyboard-1".into(),
                    hints: vec![ProseMirrorRangeHint {
                        before_from: 1,
                        before_to: 1,
                        after_from: 1,
                        after_to: 6,
                    }],
                    update: first,
                },
                AnchoredMutation {
                    source: LocalInputSource::Paste,
                    client_event_id: "paste-2".into(),
                    hints: vec![ProseMirrorRangeHint {
                        before_from: 1,
                        before_to: 6,
                        after_from: 1,
                        after_to: 7,
                    }],
                    update: second,
                },
            ],
        },
    )
    .await;

    let mut ack_count = 0;
    let mut broadcast_count = 0;
    while ack_count < 1 || broadcast_count < 2 {
        match recv(&mut socket).await {
            Frame::Ack { doc_id: acked } if acked == doc_id => ack_count += 1,
            Frame::Broadcast {
                doc_id: broadcast_doc,
                ..
            } if broadcast_doc == doc_id => broadcast_count += 1,
            Frame::Error { message, .. } => {
                panic!("anchored batch failed instead of being acked: {message}")
            }
            other => panic!("unexpected anchored-batch response: {other:?}"),
        }
    }
    assert_eq!(ack_count, 1, "one batch must produce exactly one ACK");
    assert_eq!(
        broadcast_count, 2,
        "each committed mutation must use the observer fanout exactly once"
    );
    assert_no_additional_frame(
        &mut socket,
        "anchored batch must not produce duplicate ACKs or broadcasts",
    )
    .await;
    let view = daemon.read_document(&doc_id);
    assert_eq!(view["markdown"].as_str(), Some("second"));
    let lineage = daemon.call("document_lineage", serde_json::json!({ "doc_id": doc_id }));
    assert_eq!(lineage["consumer_eligible"], true);
}

#[tokio::test]
async fn retrying_an_anchored_batch_after_a_lost_ack_is_exactly_once() {
    let daemon = Daemon::start();
    let doc_id = daemon.create_document("");
    let mut socket = connect(&daemon).await;
    send(
        &mut socket,
        Frame::Subscribe {
            doc_id: doc_id.clone(),
            state_vector: vec![],
        },
    )
    .await;

    let local = Document::new();
    match recv(&mut socket).await {
        Frame::Sync { update, .. } => local.apply_update(&update).expect("valid sync"),
        other => panic!("expected SYNC, got {other:?}"),
    }
    let block = local.blocks()[0].block_id.clone();
    let before = local.state_vector();
    local
        .replace_block(
            &block,
            &normalize(&Node::element(
                "paragraph",
                vec![Node::text("retry", vec![])],
            )),
        )
        .expect("replacement");
    let batch = Frame::AnchoredBatch {
        doc_id: doc_id.clone(),
        mutations: vec![AnchoredMutation {
            source: LocalInputSource::Written,
            client_event_id: "lost-ack-retry".into(),
            hints: vec![ProseMirrorRangeHint {
                before_from: 1,
                before_to: 1,
                after_from: 1,
                after_to: 6,
            }],
            update: local.diff_since(&before),
        }],
    };
    send(&mut socket, batch.clone()).await;
    // The transport delivered this ACK, but the application intentionally
    // forgets it and reconnects. That models a disconnect after the durable
    // commit but before the client records the acknowledgement.
    recv_ack(&mut socket, &doc_id).await;
    assert_eq!(editor_edit_count(&daemon, &doc_id), 1);
    let lineage_before_retry =
        daemon.call("document_lineage", serde_json::json!({ "doc_id": doc_id }));
    drop(socket);

    let mut retry = connect(&daemon).await;
    send(
        &mut retry,
        Frame::Subscribe {
            doc_id: doc_id.clone(),
            state_vector: local.state_vector(),
        },
    )
    .await;
    match recv(&mut retry).await {
        Frame::Sync { doc_id: synced, .. } => assert_eq!(synced, doc_id),
        other => panic!("expected retry SYNC, got {other:?}"),
    }
    send(&mut retry, batch).await;
    match recv(&mut retry).await {
        Frame::Ack { doc_id: acked } => assert_eq!(acked, doc_id),
        other => panic!("expected retry ACK, got {other:?}"),
    }
    assert_no_additional_frame(&mut retry, "a no-op retry must only be acknowledged once").await;

    assert_eq!(
        editor_edit_count(&daemon, &doc_id),
        1,
        "retry must not append another durable edit/provenance event"
    );
    assert_eq!(
        daemon.call("document_lineage", serde_json::json!({ "doc_id": doc_id })),
        lineage_before_retry,
        "retry must not change the durable lineage"
    );
}

#[tokio::test]
async fn a_partial_anchored_batch_retries_without_duplicating_its_prefix() {
    let daemon = Daemon::start();
    let doc_id = daemon.create_document("");
    let mut observer = connect(&daemon).await;
    send(
        &mut observer,
        Frame::Subscribe {
            doc_id: doc_id.clone(),
            state_vector: vec![],
        },
    )
    .await;
    let local = Document::new();
    match recv(&mut observer).await {
        Frame::Sync { update, .. } => local.apply_update(&update).expect("valid sync"),
        other => panic!("expected SYNC, got {other:?}"),
    }

    let block = local.blocks()[0].block_id.clone();
    let before_first = local.state_vector();
    local
        .replace_block(
            &block,
            &normalize(&Node::element(
                "paragraph",
                vec![Node::text("durable prefix", vec![])],
            )),
        )
        .expect("first replacement");
    let first_update = local.diff_since(&before_first);

    let before_second = local.state_vector();
    local
        .replace_block(
            &block,
            &normalize(&Node::element(
                "paragraph",
                vec![Node::text("completed retry", vec![])],
            )),
        )
        .expect("second replacement");
    let second_update = local.diff_since(&before_second);

    let first = AnchoredMutation {
        source: LocalInputSource::Written,
        client_event_id: "partial-prefix".into(),
        hints: vec![],
        update: first_update.clone(),
    };
    let second = AnchoredMutation {
        source: LocalInputSource::Paste,
        client_event_id: "partial-tail".into(),
        hints: vec![],
        update: second_update.clone(),
    };

    let mut publisher = connect(&daemon).await;
    send(
        &mut publisher,
        Frame::AnchoredBatch {
            doc_id: doc_id.clone(),
            mutations: vec![
                first.clone(),
                AnchoredMutation {
                    update: vec![0xff],
                    ..second.clone()
                },
            ],
        },
    )
    .await;
    match recv(&mut publisher).await {
        Frame::Error {
            doc_id: failed_doc,
            message,
        } => {
            assert_eq!(failed_doc, doc_id);
            assert!(
                message.contains("bad update"),
                "unexpected error: {message}"
            );
        }
        other => panic!("expected partial-batch error, got {other:?}"),
    }
    match recv(&mut observer).await {
        Frame::Broadcast {
            doc_id: broadcast_doc,
            update,
        } => {
            assert_eq!(broadcast_doc, doc_id);
            assert_eq!(update, first_update);
        }
        other => panic!("expected durable-prefix BROADCAST, got {other:?}"),
    }
    assert_no_additional_frame(
        &mut observer,
        "a failed tail must not duplicate the durable prefix",
    )
    .await;
    assert_eq!(editor_edit_count(&daemon, &doc_id), 1);
    assert_eq!(
        daemon.read_document(&doc_id)["markdown"].as_str(),
        Some("durable prefix")
    );

    let completed = Frame::AnchoredBatch {
        doc_id: doc_id.clone(),
        mutations: vec![first.clone(), second],
    };
    send(&mut publisher, completed.clone()).await;
    recv_ack(&mut publisher, &doc_id).await;
    match recv(&mut observer).await {
        Frame::Broadcast {
            doc_id: broadcast_doc,
            update,
        } => {
            assert_eq!(broadcast_doc, doc_id);
            assert_eq!(update, second_update);
        }
        other => panic!("expected repaired-tail BROADCAST, got {other:?}"),
    }
    assert_no_additional_frame(
        &mut observer,
        "retry must not broadcast the durable prefix twice",
    )
    .await;
    assert_eq!(editor_edit_count(&daemon, &doc_id), 2);
    assert_eq!(
        daemon.read_document(&doc_id)["markdown"].as_str(),
        Some("completed retry")
    );

    send(&mut publisher, completed).await;
    recv_ack(&mut publisher, &doc_id).await;
    assert_no_additional_frame(
        &mut observer,
        "a fully replayed batch must not broadcast any mutation",
    )
    .await;
    assert_eq!(editor_edit_count(&daemon, &doc_id), 2);

    let before_conflict = local.state_vector();
    local
        .replace_block(
            &block,
            &normalize(&Node::element(
                "paragraph",
                vec![Node::text("conflicting reuse", vec![])],
            )),
        )
        .expect("conflicting replacement");
    send(
        &mut publisher,
        Frame::AnchoredBatch {
            doc_id: doc_id.clone(),
            mutations: vec![AnchoredMutation {
                source: LocalInputSource::Written,
                client_event_id: "partial-prefix".into(),
                hints: vec![],
                update: local.diff_since(&before_conflict),
            }],
        },
    )
    .await;
    match recv(&mut publisher).await {
        Frame::Error { message, .. } => assert!(
            message.contains("reused with different provenance"),
            "unexpected conflict error: {message}"
        ),
        other => panic!("expected client-event conflict, got {other:?}"),
    }
    assert_no_additional_frame(
        &mut observer,
        "conflicting client event bytes must not broadcast",
    )
    .await;
    assert_eq!(editor_edit_count(&daemon, &doc_id), 2);
    assert_eq!(
        daemon.read_document(&doc_id)["markdown"].as_str(),
        Some("completed retry")
    );
}

#[tokio::test]
async fn subscribing_twice_to_one_document_still_fans_out_once() {
    let daemon = Daemon::start();
    let doc_id = daemon.create_document("Subscribe once");
    let mut observer = connect(&daemon).await;
    let observer_doc = Document::new();

    send(
        &mut observer,
        Frame::Subscribe {
            doc_id: doc_id.clone(),
            state_vector: vec![],
        },
    )
    .await;
    match recv(&mut observer).await {
        Frame::Sync { update, .. } => observer_doc.apply_update(&update).expect("valid sync"),
        other => panic!("expected first SYNC, got {other:?}"),
    }
    send(
        &mut observer,
        Frame::Subscribe {
            doc_id: doc_id.clone(),
            state_vector: observer_doc.state_vector(),
        },
    )
    .await;
    match recv(&mut observer).await {
        Frame::Sync { doc_id: synced, .. } => assert_eq!(synced, doc_id),
        other => panic!("expected repeated-subscribe SYNC, got {other:?}"),
    }

    let mut publisher = connect(&daemon).await;
    send(
        &mut publisher,
        Frame::Subscribe {
            doc_id: doc_id.clone(),
            state_vector: vec![],
        },
    )
    .await;
    let publisher_doc = Document::new();
    match recv(&mut publisher).await {
        Frame::Sync { update, .. } => publisher_doc.apply_update(&update).expect("valid sync"),
        other => panic!("expected publisher SYNC, got {other:?}"),
    }
    let before = publisher_doc.state_vector();
    let block = publisher_doc.blocks()[1].block_id.clone();
    publisher_doc
        .replace_block(
            &block,
            &normalize(&Node::element(
                "paragraph",
                vec![Node::text("one fanout", vec![])],
            )),
        )
        .expect("replacement");
    send(
        &mut publisher,
        Frame::SourcedUpdate {
            doc_id: doc_id.clone(),
            source: LocalInputSource::Written,
            update: publisher_doc.diff_since(&before),
        },
    )
    .await;
    recv_ack(&mut publisher, &doc_id).await;

    match recv(&mut observer).await {
        Frame::Broadcast {
            doc_id: broadcast_doc,
            update,
        } => {
            assert_eq!(broadcast_doc, doc_id);
            observer_doc.apply_update(&update).expect("valid update");
        }
        other => panic!("expected one BROADCAST, got {other:?}"),
    }
    assert_no_additional_frame(
        &mut observer,
        "repeated SUBSCRIBE must not install a duplicate receiver",
    )
    .await;
    assert_eq!(
        observer_doc.block_text(&block).expect("observer block"),
        "one fanout"
    );
}

#[tokio::test]
async fn every_editor_source_reaches_document_lineage() {
    let daemon = Daemon::start();
    daemon.connect();

    let cases = [
        (Some(LocalInputSource::Written), "entered", "observed"),
        (Some(LocalInputSource::Paste), "pasted", "observed"),
        (Some(LocalInputSource::Import), "imported", "observed"),
        (Some(LocalInputSource::Command), "command", "observed"),
        (Some(LocalInputSource::Unknown), "unknown", "unknown"),
        (None, "unknown", "unknown"),
    ];

    for (index, (source, expected_ingress, expected_assurance)) in cases.into_iter().enumerate() {
        let created = daemon.call(
            "create_document",
            serde_json::json!({
                "title": "",
                "agent": "test",
                "session": format!("source-map-{index}")
            }),
        );
        let doc_id = created["doc_id"].as_str().expect("doc_id").to_string();

        let mut socket = connect(&daemon).await;
        send(
            &mut socket,
            Frame::Subscribe {
                doc_id: doc_id.clone(),
                state_vector: vec![],
            },
        )
        .await;

        let local = Document::new();
        match recv(&mut socket).await {
            Frame::Sync { update, .. } => local.apply_update(&update).expect("valid sync"),
            other => panic!("expected SYNC, got {other:?}"),
        }

        let before = local.state_vector();
        let block = local.blocks()[0].block_id.clone();
        local
            .replace_block(
                &block,
                &normalize(&Node::element(
                    "paragraph",
                    vec![Node::text(format!("source {index}"), vec![])],
                )),
            )
            .expect("replace");
        let update = local.diff_since(&before);
        let frame = match source {
            Some(source) => Frame::SourcedUpdate {
                doc_id: doc_id.clone(),
                source,
                update,
            },
            None => Frame::Update {
                doc_id: doc_id.clone(),
                update,
            },
        };
        send(&mut socket, frame).await;
        recv_ack(&mut socket, &doc_id).await;

        let lineage = daemon.call("document_lineage", serde_json::json!({ "doc_id": doc_id }));
        let contributions = lineage["summary"]["contributions"]
            .as_array()
            .expect("contributions");
        assert_eq!(
            contributions.len(),
            1,
            "unexpected lineage for case {index}"
        );
        let descriptor = &contributions[0]["source"];
        assert_eq!(
            descriptor["ingress"], expected_ingress,
            "wrong ingress for case {index}"
        );
        assert_eq!(
            descriptor["assurance"], expected_assurance,
            "wrong assurance for case {index}"
        );
    }
}

#[tokio::test]
async fn awareness_is_broadcast_but_never_persisted() {
    let daemon = Daemon::start();
    let doc_id = daemon.create_document("Presence");

    let mut a = connect(&daemon).await;
    let mut b = connect(&daemon).await;
    for socket in [&mut a, &mut b] {
        send(
            socket,
            Frame::Subscribe {
                doc_id: doc_id.clone(),
                state_vector: vec![],
            },
        )
        .await;
        recv(socket).await;
    }

    let before = daemon.read_document(&doc_id);
    send(
        &mut a,
        Frame::Awareness {
            doc_id: doc_id.clone(),
            payload: b"cursor-at-block-1".to_vec(),
        },
    )
    .await;

    match recv(&mut b).await {
        Frame::Awareness { payload, .. } => {
            assert_eq!(payload, b"cursor-at-block-1");
        }
        other => panic!("expected AWARENESS, got {other:?}"),
    }

    // Presence is not content: it must leave no trace in the document.
    let after = daemon.read_document(&doc_id);
    assert_eq!(
        before["version"], after["version"],
        "awareness mutated the document"
    );
}

#[tokio::test]
async fn the_sync_endpoint_requires_the_editor_capability() {
    let daemon = Daemon::start();
    let result = tokio_tungstenite::connect_async(daemon.sync_url()).await;
    assert!(
        result.is_err(),
        "sync must not be reachable without its editor capability"
    );
}

#[tokio::test]
async fn a_browser_uses_the_editor_capability_without_putting_it_in_the_url() {
    let daemon = Daemon::start();
    let doc_id = daemon.create_document("Browser capability");
    let mut socket = connect_as_browser(&daemon).await;

    assert!(!daemon.sync_url().contains(&daemon.editor_token));
    send(
        &mut socket,
        Frame::Subscribe {
            doc_id: doc_id.clone(),
            state_vector: vec![],
        },
    )
    .await;
    match recv(&mut socket).await {
        Frame::Sync { doc_id: synced, .. } => assert_eq!(synced, doc_id),
        other => panic!("expected SYNC, got {other:?}"),
    }
}

#[tokio::test]
async fn the_sync_endpoint_rejects_the_mcp_capability() {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;

    let daemon = Daemon::start();
    let mut request = daemon.sync_url().into_client_request().expect("request");
    request.headers_mut().insert(
        "Sec-WebSocket-Protocol",
        format!("thought.v1, thought.token.{}", daemon.token)
            .parse()
            .expect("header"),
    );

    let result = tokio_tungstenite::connect_async(request).await;
    assert!(
        result.is_err(),
        "an MCP capability must not authorize editor source claims"
    );
}

#[tokio::test]
async fn malformed_and_server_only_client_frames_receive_protocol_errors() {
    let daemon = Daemon::start();
    let mut socket = connect(&daemon).await;

    socket
        .send(Message::Binary(vec![0xff].into()))
        .await
        .expect("send malformed frame");
    match recv(&mut socket).await {
        Frame::Error { doc_id, message } => {
            assert!(doc_id.is_empty());
            assert_eq!(message, "invalid sync frame");
        }
        other => panic!("expected ERROR, got {other:?}"),
    }

    send(
        &mut socket,
        Frame::Ack {
            doc_id: "client-ack".into(),
        },
    )
    .await;
    match recv(&mut socket).await {
        Frame::Error { doc_id, message } => {
            assert_eq!(doc_id, "client-ack");
            assert_eq!(message, "frame is server-to-client only");
        }
        other => panic!("expected ERROR, got {other:?}"),
    }
}
