//! The sync endpoint (M2.1), driven by real WebSocket peers.
//!
//! What matters here is that the *protocol* works, because M3's relay reuses
//! it. If the editor had a private channel this test would only prove the
//! editor works.

mod harness;

use futures_util::{SinkExt, StreamExt};
use harness::{Daemon, Frame};
use polar_core::Document;
use polar_schema::{Node, normalize};
use tokio_tungstenite::tungstenite::Message;

type Socket =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

async fn connect(daemon: &Daemon) -> Socket {
    use tokio_tungstenite::tungstenite::client::IntoClientRequest;
    let mut request = daemon.sync_url().into_client_request().expect("request");
    request.headers_mut().insert(
        "Authorization",
        format!("Bearer {}", daemon.token).parse().expect("header"),
    );
    let (socket, _) = tokio_tungstenite::connect_async(request)
        .await
        .expect("sync endpoint accepted the connection");
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
    send(
        &mut a,
        Frame::Update {
            doc_id: doc_id.clone(),
            update: local.diff_since(&before),
        },
    )
    .await;

    // B is told, without having asked.
    match recv(&mut b).await {
        Frame::Broadcast { update, .. } => remote.apply_update(&update).expect("valid update"),
        other => panic!("expected BROADCAST, got {other:?}"),
    }
    assert_eq!(
        remote.block_text(&block).expect("block"),
        "typed in the window"
    );

    // And it reached the store, so an agent reading over MCP sees it.
    let view = daemon.read_document(&doc_id);
    assert!(
        view["markdown"]
            .as_str()
            .expect("markdown")
            .contains("typed in the window"),
        "the edit never reached SQLite"
    );
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
async fn the_sync_endpoint_requires_the_token() {
    let daemon = Daemon::start();
    let result = tokio_tungstenite::connect_async(daemon.sync_url()).await;
    assert!(
        result.is_err(),
        "sync must not be reachable without the discovery token"
    );
}
