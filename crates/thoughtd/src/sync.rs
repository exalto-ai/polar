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
use thought_mcp::lineage::ProseMirrorRangeHint;
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
    pub const ANCHORED_BATCH: u8 = 0x0a;
}

/// Current body version for [`Frame::AnchoredBatch`].
pub const ANCHORED_BATCH_VERSION: u8 = 1;
/// Keep one transport batch small enough to validate and commit promptly.
pub const MAX_ANCHORED_MUTATIONS: usize = 128;
/// ProseMirror normally reports only a handful of changed ranges per
/// transaction. This cap makes hostile count fields cheap to reject.
pub const MAX_ANCHORED_HINTS: usize = 64;
/// Client event ids are opaque, retry-stable correlation keys, not payloads.
pub const MAX_CLIENT_EVENT_ID_BYTES: usize = 64;

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

/// One ordered editor dispatch carried by an anchored batch.
///
/// The event id and ranges are observed by the editor transport. Public MCP
/// callers cannot choose them, which is why they can support stronger local
/// provenance once the workspace validates them against both document trees.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnchoredMutation {
    pub source: LocalInputSource,
    pub client_event_id: String,
    pub hints: Vec<ProseMirrorRangeHint>,
    pub update: Vec<u8>,
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
    /// An ordered transport batch of semantic editor dispatches. Each
    /// mutation remains a distinct durable event and is applied in order.
    AnchoredBatch {
        doc_id: String,
        mutations: Vec<AnchoredMutation>,
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
    /// One client batch has been fully committed to durable storage. A legacy
    /// Update is a batch of one. WebSocket ordering makes this an
    /// acknowledgement of the oldest unacknowledged batch from that peer.
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
            Frame::AnchoredBatch { doc_id, mutations } => {
                let mut body = Vec::new();
                body.push(ANCHORED_BATCH_VERSION);
                body.extend_from_slice(&(mutations.len() as u16).to_be_bytes());
                for mutation in mutations {
                    body.push(mutation.source.encode());
                    let client_event_id = mutation.client_event_id.as_bytes();
                    body.push(client_event_id.len() as u8);
                    body.extend_from_slice(client_event_id);
                    body.extend_from_slice(&(mutation.hints.len() as u16).to_be_bytes());
                    for hint in &mutation.hints {
                        body.extend_from_slice(&hint.before_from.to_be_bytes());
                        body.extend_from_slice(&hint.before_to.to_be_bytes());
                        body.extend_from_slice(&hint.after_from.to_be_bytes());
                        body.extend_from_slice(&hint.after_to.to_be_bytes());
                    }
                    body.extend_from_slice(&(mutation.update.len() as u32).to_be_bytes());
                    body.extend_from_slice(&mutation.update);
                }
                (tag::ANCHORED_BATCH, doc_id, body)
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
        let body_start = 5usize.checked_add(id_len)?;
        if bytes.len() < body_start {
            return None;
        }
        let doc_id = String::from_utf8(bytes[5..body_start].to_vec()).ok()?;
        let body = bytes[body_start..].to_vec();
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
            tag::ANCHORED_BATCH => Frame::AnchoredBatch {
                doc_id,
                mutations: decode_anchored_batch(&body)?,
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
            tag::PRESENCE => Frame::Presence {
                doc_id,
                actor: String::from_utf8_lossy(&body).into_owned(),
            },
            tag::ACK if body.is_empty() => Frame::Ack { doc_id },
            _ => return None,
        })
    }
}

/// Decode a batch body without trusting any length or count from the peer.
/// Returning `None` for every malformed form keeps the WebSocket connection
/// task total over arbitrary binary input.
fn decode_anchored_batch(body: &[u8]) -> Option<Vec<AnchoredMutation>> {
    let mut cursor = WireCursor::new(body);
    if cursor.u8()? != ANCHORED_BATCH_VERSION {
        return None;
    }
    let mutation_count = usize::from(cursor.u16()?);
    if !(1..=MAX_ANCHORED_MUTATIONS).contains(&mutation_count) {
        return None;
    }

    let mut mutations = Vec::with_capacity(mutation_count);
    for _ in 0..mutation_count {
        let source = LocalInputSource::decode(cursor.u8()?)?;

        let client_event_id_len = usize::from(cursor.u8()?);
        if !(1..=MAX_CLIENT_EVENT_ID_BYTES).contains(&client_event_id_len) {
            return None;
        }
        let client_event_id = String::from_utf8(cursor.take(client_event_id_len)?.to_vec()).ok()?;

        let hint_count = usize::from(cursor.u16()?);
        if hint_count > MAX_ANCHORED_HINTS {
            return None;
        }
        let mut hints = Vec::with_capacity(hint_count);
        for _ in 0..hint_count {
            let hint = ProseMirrorRangeHint {
                before_from: cursor.u32()?,
                before_to: cursor.u32()?,
                after_from: cursor.u32()?,
                after_to: cursor.u32()?,
            };
            if hint.before_from > hint.before_to || hint.after_from > hint.after_to {
                return None;
            }
            hints.push(hint);
        }

        let update_len = usize::try_from(cursor.u32()?).ok()?;
        if update_len == 0 {
            return None;
        }
        let update = cursor.take(update_len)?.to_vec();
        mutations.push(AnchoredMutation {
            source,
            client_event_id,
            hints,
            update,
        });
    }

    cursor.is_empty().then_some(mutations)
}

struct WireCursor<'a> {
    remaining: &'a [u8],
}

impl<'a> WireCursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { remaining: bytes }
    }

    fn take(&mut self, len: usize) -> Option<&'a [u8]> {
        if self.remaining.len() < len {
            return None;
        }
        let (value, remaining) = self.remaining.split_at(len);
        self.remaining = remaining;
        Some(value)
    }

    fn u8(&mut self) -> Option<u8> {
        self.take(1).map(|bytes| bytes[0])
    }

    fn u16(&mut self) -> Option<u16> {
        Some(u16::from_be_bytes(self.take(2)?.try_into().ok()?))
    }

    fn u32(&mut self) -> Option<u32> {
        Some(u32::from_be_bytes(self.take(4)?.try_into().ok()?))
    }

    fn is_empty(&self) -> bool {
        self.remaining.is_empty()
    }
}

/// One broadcast channel per document plus an optional peer discriminator.
/// Observer-owned document fanout uses `u64::MAX`, so every replica, including
/// the originator, receives the idempotent authoritative update.
type Fanout = broadcast::Sender<(u64, Frame)>;

/// One connection's live receiver for a document. Keeping the document id
/// beside the receiver lets a lagged peer recover from a complete snapshot
/// without asking the client to reconnect or guess which subscription fell
/// behind.
struct Subscription {
    doc_id: String,
    receiver: broadcast::Receiver<(u64, Frame)>,
}

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
    let mut subscriptions: Vec<Subscription> = Vec::new();
    let mut senders: HashMap<String, Fanout> = HashMap::new();

    loop {
        // Wait on either the socket or any document this peer subscribes to.
        let inbound = socket.recv();
        let fanned = next_broadcast(&mut subscriptions, peer, state.workspace.as_ref());

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
    subscriptions: &mut [Subscription],
    peer: u64,
    workspace: &Workspace,
) -> Option<Frame> {
    if subscriptions.is_empty() {
        // Nothing to wait on; let the select arm stay pending forever rather
        // than spinning.
        std::future::pending::<()>().await;
    }
    let mut futures = Vec::new();
    for (index, subscription) in subscriptions.iter_mut().enumerate() {
        let doc_id = subscription.doc_id.clone();
        futures.push(Box::pin(async move {
            (index, doc_id, subscription.receiver.recv().await)
        }));
    }
    loop {
        let ((index, doc_id, result), _, rest) = futures::future::select_all(futures).await;
        match result {
            // Suppress only frames explicitly tagged with this peer. The
            // workspace observer uses `u64::MAX`, intentionally broadcasting
            // committed document updates back to their originator too.
            Ok((from, frame)) if from != peer => return Some(frame),
            Ok(_) => {
                if rest.is_empty() {
                    return None;
                }
                futures = rest;
            }
            Err(broadcast::error::RecvError::Lagged(skipped)) => {
                // The queued deltas are now an incomplete suffix. Subscribe at
                // the live tail before taking the snapshot so every concurrent
                // commit is either included in the snapshot, queued after it,
                // or harmlessly present in both. Yjs makes the overlap safe.
                drop(rest);
                subscriptions[index].receiver = subscriptions[index].receiver.resubscribe();
                tracing::warn!(%doc_id, skipped, "sync peer lagged; sending full resync");
                return Some(match workspace.sync_since(&doc_id, &[]) {
                    Ok(update) => Frame::Sync { doc_id, update },
                    Err(error) => Frame::Error {
                        doc_id,
                        message: error.to_string(),
                    },
                });
            }
            Err(broadcast::error::RecvError::Closed) => {
                if rest.is_empty() {
                    return None;
                }
                futures = rest;
            }
        }
    }
}

fn handle(
    state: &SyncState,
    actor: &ActorRef,
    frame: Frame,
    senders: &mut HashMap<String, Fanout>,
    subscriptions: &mut Vec<Subscription>,
) -> Vec<Frame> {
    match frame {
        Frame::Subscribe {
            doc_id,
            state_vector,
        } => {
            let channel = state.channel(&doc_id);
            // SUBSCRIBE is idempotent for one connection and document. A
            // second receiver would duplicate every later broadcast.
            if !senders.contains_key(&doc_id) {
                subscriptions.push(Subscription {
                    doc_id: doc_id.clone(),
                    receiver: channel.subscribe(),
                });
                senders.insert(doc_id.clone(), channel);
            }

            match state.workspace.sync_since(&doc_id, &state_vector) {
                Ok(update) => vec![Frame::Sync { doc_id, update }],
                Err(e) => vec![Frame::Error {
                    doc_id,
                    message: e.to_string(),
                }],
            }
        }

        Frame::Update { doc_id, update } => {
            apply_editor_update(state, actor, doc_id, update, LocalInputSource::Unknown)
        }

        Frame::SourcedUpdate {
            doc_id,
            source,
            update,
        } => apply_editor_update(state, actor, doc_id, update, source),

        Frame::AnchoredBatch { doc_id, mutations } => {
            apply_anchored_batch(state, actor, doc_id, mutations)
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

/// Commit an ordered batch of editor dispatches, then acknowledge the batch
/// once every mutation is durable. If a later mutation fails, earlier commits
/// remain valid and a retry is safe because the client event ids and Yjs
/// updates are stable and idempotent.
fn apply_anchored_batch(
    state: &SyncState,
    actor: &ActorRef,
    doc_id: String,
    mutations: Vec<AnchoredMutation>,
) -> Vec<Frame> {
    for mutation in mutations {
        let context =
            mutation_context(mutation.source).with_client_event_id(mutation.client_event_id);
        match state.workspace.apply_anchored_peer_update_with_context(
            &doc_id,
            &mutation.update,
            actor,
            &context,
            &mutation.hints,
        ) {
            Ok(None) => {}
            // The workspace observer owns the one fan-out path for every
            // committed mutation, including this editor update.
            Ok(Some(_)) => {}
            Err(error) => {
                return vec![Frame::Error {
                    doc_id,
                    message: error.to_string(),
                }];
            }
        }
    }

    vec![Frame::Ack { doc_id }]
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
    use super::{
        Frame, LocalInputSource, Subscription, SyncState, handle, mutation_context, next_broadcast,
    };
    use std::{collections::HashMap, sync::Arc};
    use thought_core::Document;
    use thought_mcp::{ActorRef, Assurance, Ingress, Workspace};

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

    #[test]
    fn repeated_subscribe_reuses_one_connection_receiver() {
        let workspace = Arc::new(Workspace::open_in_memory().unwrap());
        let created = workspace
            .create_document("One receiver", &ActorRef::editor())
            .unwrap();
        let state = SyncState::new(workspace);
        let mut senders = HashMap::new();
        let mut subscriptions = Vec::new();

        for _ in 0..2 {
            let reply = handle(
                &state,
                &ActorRef::editor(),
                Frame::Subscribe {
                    doc_id: created.doc_id.clone(),
                    state_vector: vec![],
                },
                &mut senders,
                &mut subscriptions,
            );
            assert!(matches!(reply.as_slice(), [Frame::Sync { .. }]));
        }

        assert_eq!(senders.len(), 1);
        assert_eq!(subscriptions.len(), 1);
        assert_eq!(subscriptions[0].doc_id, created.doc_id);
    }

    #[tokio::test]
    async fn a_lagged_receiver_gets_a_full_resync_then_resumes_at_the_live_tail() {
        let workspace = Arc::new(Workspace::open_in_memory().unwrap());
        let state = SyncState::new(workspace.clone());
        let created = workspace
            .create_document("Recovery", &ActorRef::editor())
            .unwrap();
        let channel = state.channel(&created.doc_id);
        let mut subscriptions = vec![Subscription {
            doc_id: created.doc_id.clone(),
            receiver: channel.subscribe(),
        }];

        // The per-document channel retains 256 messages. The 257th makes this
        // untouched receiver observably lag by one frame.
        for sequence in 0..257 {
            channel
                .send((
                    u64::MAX,
                    Frame::Presence {
                        doc_id: created.doc_id.clone(),
                        actor: sequence.to_string(),
                    },
                ))
                .unwrap();
        }

        let recovered = next_broadcast(&mut subscriptions, 1, workspace.as_ref())
            .await
            .expect("lag recovery frame");
        match recovered {
            Frame::Sync { doc_id, update } => {
                assert_eq!(doc_id, created.doc_id);
                let replica = Document::new();
                replica.apply_update(&update).expect("full resync update");
                assert_eq!(replica.blocks().len(), created.blocks.len());
            }
            other => panic!("expected full SYNC after lag, got {other:?}"),
        }

        let live_payload = b"live-after-resync".to_vec();
        channel
            .send((
                u64::MAX,
                Frame::Awareness {
                    doc_id: created.doc_id.clone(),
                    payload: live_payload.clone(),
                },
            ))
            .unwrap();
        match next_broadcast(&mut subscriptions, 1, workspace.as_ref()).await {
            Some(Frame::Awareness { doc_id, payload }) => {
                assert_eq!(doc_id, created.doc_id);
                assert_eq!(payload, live_payload);
            }
            other => panic!("expected live frame after resync, got {other:?}"),
        }
    }
}
