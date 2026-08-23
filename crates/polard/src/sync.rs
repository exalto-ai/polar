//! The document sync endpoint (M2.1).
//!
//! Speaks the relay protocol from §5, not a private channel for the window.
//! The editor is a peer that happens to be local, so M3's relay reuses this
//! rather than adding a second protocol that would drift from it.
//!
//! Awareness is broadcast and never persisted: it is presence, not content.

use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::Response;
use polar_mcp::{ActorRef, Workspace};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::broadcast;

/// Wire tags. Length-prefixed binary, because Yjs updates are binary and
/// base64 in JSON would inflate every keystroke.
mod tag {
    pub const SUBSCRIBE: u8 = 0x01;
    pub const SYNC: u8 = 0x02;
    pub const UPDATE: u8 = 0x03;
    pub const BROADCAST: u8 = 0x04;
    pub const AWARENESS: u8 = 0x05;
    pub const ERROR: u8 = 0x06;
}

#[derive(Debug, Clone)]
pub enum Frame {
    Subscribe {
        doc_id: String,
        state_vector: Vec<u8>,
    },
    Sync {
        doc_id: String,
        update: Vec<u8>,
    },
    Update {
        doc_id: String,
        update: Vec<u8>,
    },
    Broadcast {
        doc_id: String,
        update: Vec<u8>,
    },
    Awareness {
        doc_id: String,
        payload: Vec<u8>,
    },
    Error {
        doc_id: String,
        message: String,
    },
}

impl Frame {
    pub fn encode(&self) -> Vec<u8> {
        let (tag, doc_id, body) = match self {
            Frame::Subscribe {
                doc_id,
                state_vector,
            } => (tag::SUBSCRIBE, doc_id, state_vector.clone()),
            Frame::Sync { doc_id, update } => (tag::SYNC, doc_id, update.clone()),
            Frame::Update { doc_id, update } => (tag::UPDATE, doc_id, update.clone()),
            Frame::Broadcast { doc_id, update } => (tag::BROADCAST, doc_id, update.clone()),
            Frame::Awareness { doc_id, payload } => (tag::AWARENESS, doc_id, payload.clone()),
            Frame::Error { doc_id, message } => (tag::ERROR, doc_id, message.as_bytes().to_vec()),
        };
        let id = doc_id.as_bytes();
        let mut out = Vec::with_capacity(5 + id.len() + body.len());
        out.push(tag);
        out.extend_from_slice(&(id.len() as u32).to_be_bytes());
        out.extend_from_slice(id);
        out.extend_from_slice(&body);
        out
    }

    pub fn decode(bytes: &[u8]) -> Option<Frame> {
        if bytes.len() < 5 {
            return None;
        }
        let tag = bytes[0];
        let id_len = u32::from_be_bytes(bytes[1..5].try_into().ok()?) as usize;
        // Guard before slicing: a truncated or hostile frame must not panic the
        // connection task and take the endpoint down with it.
        if bytes.len() < 5 + id_len {
            return None;
        }
        let doc_id = String::from_utf8(bytes[5..5 + id_len].to_vec()).ok()?;
        let body = bytes[5 + id_len..].to_vec();
        Some(match tag {
            tag::SUBSCRIBE => Frame::Subscribe {
                doc_id,
                state_vector: body,
            },
            tag::SYNC => Frame::Sync {
                doc_id,
                update: body,
            },
            tag::UPDATE => Frame::Update {
                doc_id,
                update: body,
            },
            tag::BROADCAST => Frame::Broadcast {
                doc_id,
                update: body,
            },
            tag::AWARENESS => Frame::Awareness {
                doc_id,
                payload: body,
            },
            tag::ERROR => Frame::Error {
                doc_id,
                message: String::from_utf8_lossy(&body).into_owned(),
            },
            _ => return None,
        })
    }
}

/// One broadcast channel per document, plus the sender's id so a peer is not
/// handed back its own update.
type Fanout = broadcast::Sender<(u64, Frame)>;

#[derive(Clone)]
pub struct SyncState {
    pub workspace: Arc<Workspace>,
    channels: Arc<Mutex<HashMap<String, Fanout>>>,
    next_peer: Arc<std::sync::atomic::AtomicU64>,
}

impl SyncState {
    pub fn new(workspace: Arc<Workspace>) -> SyncState {
        SyncState {
            workspace,
            channels: Arc::new(Mutex::new(HashMap::new())),
            next_peer: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        }
    }

    fn channel(&self, doc_id: &str) -> Fanout {
        let mut channels = self.channels.lock().expect("sync channels poisoned");
        channels
            .entry(doc_id.to_string())
            .or_insert_with(|| broadcast::channel(256).0)
            .clone()
    }
}

pub async fn handler(ws: WebSocketUpgrade, State(state): State<SyncState>) -> Response {
    ws.on_upgrade(move |socket| connection(socket, state))
}

async fn connection(mut socket: WebSocket, state: SyncState) {
    let peer = state
        .next_peer
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // The window is a human's edit path; agents come in over MCP.
    let actor = ActorRef::human("editor");
    let mut subscriptions: Vec<broadcast::Receiver<(u64, Frame)>> = Vec::new();
    let mut senders: HashMap<String, Fanout> = HashMap::new();

    loop {
        // Wait on either the socket or any document this peer subscribes to.
        let inbound = socket.recv();
        let fanned = next_broadcast(&mut subscriptions, peer);

        tokio::select! {
            message = inbound => {
                let Some(Ok(message)) = message else { break };
                let Message::Binary(bytes) = message else { continue };
                let Some(frame) = Frame::decode(&bytes) else { continue };

                let reply = handle(&state, &actor, frame, &mut senders, &mut subscriptions);
                for out in reply {
                    if socket.send(Message::Binary(out.encode().into())).await.is_err() {
                        return;
                    }
                }
            }
            Some(frame) = fanned => {
                if socket.send(Message::Binary(frame.encode().into())).await.is_err() {
                    return;
                }
            }
        }
    }
}

/// Await the next frame from any subscribed document.
async fn next_broadcast(
    subscriptions: &mut [broadcast::Receiver<(u64, Frame)>],
    peer: u64,
) -> Option<Frame> {
    if subscriptions.is_empty() {
        // Nothing to wait on; let the select arm stay pending forever rather
        // than spinning.
        std::future::pending::<()>().await;
    }
    let mut futures = Vec::new();
    for receiver in subscriptions.iter_mut() {
        futures.push(Box::pin(receiver.recv()));
    }
    loop {
        let (result, _, rest) = futures::future::select_all(futures).await;
        match result {
            // Never hand a peer back its own update.
            Ok((from, frame)) if from != peer => return Some(frame),
            Ok(_) => {
                if rest.is_empty() {
                    return None;
                }
                futures = rest;
            }
            Err(_) => return None,
        }
    }
}

fn handle(
    state: &SyncState,
    actor: &ActorRef,
    frame: Frame,
    senders: &mut HashMap<String, Fanout>,
    subscriptions: &mut Vec<broadcast::Receiver<(u64, Frame)>>,
) -> Vec<Frame> {
    match frame {
        Frame::Subscribe {
            doc_id,
            state_vector,
        } => {
            let channel = state.channel(&doc_id);
            subscriptions.push(channel.subscribe());
            senders.insert(doc_id.clone(), channel);

            match state.workspace.sync_since(&doc_id, &state_vector) {
                Ok(update) => vec![Frame::Sync { doc_id, update }],
                Err(e) => vec![Frame::Error {
                    doc_id,
                    message: e.to_string(),
                }],
            }
        }

        Frame::Update { doc_id, update } => {
            match state.workspace.apply_peer_update(&doc_id, &update, actor) {
                // A no-op update is not broadcast: Yjs updates are idempotent
                // and a reconnecting peer resends what it already sent.
                Ok(None) => vec![],
                Ok(Some(_)) => {
                    if let Some(channel) = senders.get(&doc_id) {
                        let _ = channel.send((
                            u64::MAX,
                            Frame::Broadcast {
                                doc_id: doc_id.clone(),
                                update,
                            },
                        ));
                    }
                    vec![]
                }
                Err(e) => vec![Frame::Error {
                    doc_id,
                    message: e.to_string(),
                }],
            }
        }

        // Presence, never persisted.
        Frame::Awareness { doc_id, payload } => {
            if let Some(channel) = senders.get(&doc_id) {
                let _ = channel.send((
                    u64::MAX,
                    Frame::Awareness {
                        doc_id: doc_id.clone(),
                        payload,
                    },
                ));
            }
            vec![]
        }

        // Server-to-client only; a client sending one is confused.
        Frame::Sync { .. } | Frame::Broadcast { .. } | Frame::Error { .. } => vec![],
    }
}
