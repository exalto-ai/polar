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
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use thought_mcp::{ActorRef, MutationContext, Workspace};
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
    pub const PRESENCE: u8 = 0x07;
    pub const ACK: u8 = 0x08;
    pub const SOURCED_UPDATE: u8 = 0x09;
}

/// How a local editor observed content entering the document.
///
/// This is deliberately not actor identity. It travels beside a Yjs update
/// because Yjs transaction origins are process-local and disappear on encode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalInputSource {
    Unknown,
    Written,
    Paste,
    Import,
    Command,
}

impl LocalInputSource {
    fn encode(self) -> u8 {
        match self {
            LocalInputSource::Unknown => 0x00,
            LocalInputSource::Written => 0x01,
            LocalInputSource::Paste => 0x02,
            LocalInputSource::Import => 0x03,
            LocalInputSource::Command => 0x04,
        }
    }

    fn decode(byte: u8) -> Option<LocalInputSource> {
        Some(match byte {
            0x00 => LocalInputSource::Unknown,
            0x01 => LocalInputSource::Written,
            0x02 => LocalInputSource::Paste,
            0x03 => LocalInputSource::Import,
            0x04 => LocalInputSource::Command,
            _ => return None,
        })
    }
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
    /// The first body byte is a closed local-input-source value; the rest is
    /// the unchanged Yjs update. Legacy `Update` remains accepted as Unknown.
    SourcedUpdate {
        doc_id: String,
        source: LocalInputSource,
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
    /// An agent just wrote. Agents connect over MCP, which has no awareness
    /// protocol, so their presence is inferred from their edits and published
    /// here rather than pretended into the awareness channel — it is a
    /// different kind of signal, and conflating them would mislead the window.
    Presence {
        doc_id: String,
        actor: String,
    },
    /// One client Update has been committed to durable storage. WebSocket
    /// ordering makes this an acknowledgement of the oldest unacknowledged
    /// update from that peer.
    Ack {
        doc_id: String,
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
            Frame::SourcedUpdate {
                doc_id,
                source,
                update,
            } => {
                let mut body = Vec::with_capacity(update.len() + 1);
                body.push(source.encode());
                body.extend_from_slice(update);
                (tag::SOURCED_UPDATE, doc_id, body)
            }
            Frame::Broadcast { doc_id, update } => (tag::BROADCAST, doc_id, update.clone()),
            Frame::Awareness { doc_id, payload } => (tag::AWARENESS, doc_id, payload.clone()),
            Frame::Error { doc_id, message } => (tag::ERROR, doc_id, message.as_bytes().to_vec()),
            Frame::Presence { doc_id, actor } => (tag::PRESENCE, doc_id, actor.as_bytes().to_vec()),
            Frame::Ack { doc_id } => (tag::ACK, doc_id, vec![]),
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
            tag::SOURCED_UPDATE => {
                let (&source, update) = body.split_first()?;
                Frame::SourcedUpdate {
                    doc_id,
                    source: LocalInputSource::decode(source)?,
                    update: update.to_vec(),
                }
            }
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
            tag::PRESENCE => Frame::Presence {
                doc_id,
                actor: String::from_utf8_lossy(&body).into_owned(),
            },
            tag::ACK if body.is_empty() => Frame::Ack { doc_id },
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
        let state = SyncState {
            workspace: workspace.clone(),
            channels: Arc::new(Mutex::new(HashMap::new())),
            next_peer: Arc::new(std::sync::atomic::AtomicU64::new(1)),
        };

        // Every committed change fans out, whatever produced it. Broadcasting
        // only from this socket left agent edits over MCP invisible to an open
        // editor — which is most of the point of the app.
        //
        // A peer therefore receives its own update back. That is harmless: Yjs
        // updates are idempotent, so applying one you already have is a no-op,
        // and one fan-out path is worth more than the saved bytes.
        let channels = state.channels.clone();
        workspace.observe(move |doc_id, update, actor| {
            let sender = channels
                .lock()
                .expect("sync channels poisoned")
                .get(doc_id)
                .cloned();
            let Some(sender) = sender else { return };

            let _ = sender.send((
                u64::MAX,
                Frame::Broadcast {
                    doc_id: doc_id.to_string(),
                    update: update.to_vec(),
                },
            ));

            // An agent's only way of saying "I am here" is that it wrote.
            if actor.kind == "agent" {
                let payload = serde_json::json!({
                    "actor_id": actor.id,
                    "name": actor.display_name,
                    "model": actor.model,
                    "session": actor.session_id,
                });
                let _ = sender.send((
                    u64::MAX,
                    Frame::Presence {
                        doc_id: doc_id.to_string(),
                        actor: payload.to_string(),
                    },
                ));
            }
        });
        state
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
    // A browser closes the connection unless the server names one of the
    // subprotocols it offered, and the token rides in as one of them.
    ws.protocols(["thought.v1"])
        .on_upgrade(move |socket| connection(socket, state))
}

async fn connection(mut socket: WebSocket, state: SyncState) {
    let peer = state
        .next_peer
        .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    // The window is a human's edit path; agents come in over MCP.
    let actor = ActorRef::editor();
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
                let Some(frame) = Frame::decode(&bytes) else {
                    let error = Frame::Error {
                        doc_id: String::new(),
                        message: "invalid sync frame".into(),
                    };
                    if socket.send(Message::Binary(error.encode().into())).await.is_err() {
                        return;
                    }
                    continue;
                };

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

        Frame::Update { doc_id, update } => apply_editor_update(
            state,
            actor,
            doc_id,
            update,
            LocalInputSource::Unknown,
            senders,
        ),

        Frame::SourcedUpdate {
            doc_id,
            source,
            update,
        } => apply_editor_update(state, actor, doc_id, update, source, senders),

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

        // Server-to-client frames are a protocol error when sent by a peer.
        Frame::Sync { doc_id, .. }
        | Frame::Broadcast { doc_id, .. }
        | Frame::Error { doc_id, .. }
        | Frame::Presence { doc_id, .. }
        | Frame::Ack { doc_id } => vec![Frame::Error {
            doc_id,
            message: "frame is server-to-client only".into(),
        }],
    }
}

/// Commit one editor update before acknowledging it.
///
/// `source` is already decoded and validated at the wire boundary. Mapping it
/// here keeps the provenance claim owned by the transport and persists it
/// atomically with the update.
fn apply_editor_update(
    state: &SyncState,
    actor: &ActorRef,
    doc_id: String,
    update: Vec<u8>,
    source: LocalInputSource,
    senders: &HashMap<String, Fanout>,
) -> Vec<Frame> {
    let context = mutation_context(source);
    match state
        .workspace
        .apply_peer_update_with_context(&doc_id, &update, actor, &context)
    {
        // A no-op update is not broadcast: Yjs updates are idempotent
        // and a reconnecting peer resends what it already sent. It is
        // still acknowledged, because the persisted document already
        // contains it.
        Ok(None) => vec![Frame::Ack { doc_id }],
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
            // `apply_peer_update_with_context` returns only after the workspace
            // commit succeeds, so this is a persistence acknowledgement,
            // not merely a receipt acknowledgement.
            vec![Frame::Ack { doc_id }]
        }
        Err(e) => vec![Frame::Error {
            doc_id,
            message: e.to_string(),
        }],
    }
}

fn mutation_context(source: LocalInputSource) -> MutationContext {
    match source {
        LocalInputSource::Written => MutationContext::entered(),
        LocalInputSource::Paste => MutationContext::pasted(),
        LocalInputSource::Import => MutationContext::imported(),
        LocalInputSource::Command => MutationContext::command(),
        LocalInputSource::Unknown => MutationContext::unknown(),
    }
}

#[cfg(test)]
mod tests {
    use super::{LocalInputSource, mutation_context};
    use thought_mcp::{Assurance, Ingress};

    #[test]
    fn every_local_source_maps_to_its_transport_owned_context() {
        let cases = [
            (
                LocalInputSource::Written,
                Ingress::Entered,
                Assurance::Observed,
            ),
            (
                LocalInputSource::Paste,
                Ingress::Pasted,
                Assurance::Observed,
            ),
            (
                LocalInputSource::Import,
                Ingress::Imported,
                Assurance::Observed,
            ),
            (
                LocalInputSource::Command,
                Ingress::Command,
                Assurance::Observed,
            ),
            (
                LocalInputSource::Unknown,
                Ingress::Unknown,
                Assurance::Unknown,
            ),
        ];

        for (source, ingress, assurance) in cases {
            let context = mutation_context(source);
            assert_eq!(context.ingress(), ingress, "wrong ingress for {source:?}");
            assert_eq!(
                context.assurance(),
                assurance,
                "wrong assurance for {source:?}"
            );
        }
    }
}
