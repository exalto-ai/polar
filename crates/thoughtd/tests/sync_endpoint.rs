//! The sync endpoint (M2.1), driven by real WebSocket peers.
//!
//! What matters here is that the *protocol* works, because M3's relay reuses
//! it. If the editor had a private channel this test would only prove the
//! editor works.

mod harness;

use futures_util::{SinkExt, StreamExt};
use harness::{Daemon, Frame};
use thought_core::Document;
use thought_schema::{Node, normalize};
use thoughtd::sync::LocalInputSource;
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
