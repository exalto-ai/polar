//! Canonical evidence hashing for local provenance records.
//!
//! Each digest is SHA-256 over a domain-separated, versioned TLV encoding.
//! Every field has a numeric tag and an explicit byte length; lists include a
//! count and length-prefix every ordered item. No digest depends on Rust's
//! memory layout, map iteration, debug output, or an ad hoc JSON object.
//!
//! The constants below domain-separate the encodings inside the frozen V1
//! evidence and reconciliation suite. They are not persisted and dispatched as
//! independently upgradeable versions. [`EventHashInput::chain_version`] is
//! the durable suite version, while [`LineageHashInput::algorithm_version`]
//! binds the current derived read model. This build accepts only V1. Before any
//! version changes, a schema migration and version-dispatched verifier and
//! reconciler must retain support for already-recorded evidence.
//!
//! # Privacy
//!
//! These are deterministic, unsalted local hashes. Text and metadata with low
//! entropy remain vulnerable to guessing and cross-document correlation. Raw
//! local digests are therefore **not safe privacy-preserving publication
//! anchors**. A publishing protocol such as Seal must salt, blind, or otherwise
//! commit to them before disclosure.

use sha2::{Digest as _, Sha256};
use thought_provenance::{
    Assurance, BlockLocation, BlockShape, BlockSnapshot, DeltaSegment, Ingress, LiveLineageSpan,
    SourceId, TextLeafSnapshot, TextLocation,
};

pub type EvidenceDigest = [u8; 32];

pub const DOCUMENT_DIGEST_VERSION: u32 = 1;
pub const EVENT_DIGEST_ENCODING_VERSION: u32 = 1;
pub const UPDATE_LOG_DIGEST_VERSION: u32 = 1;
pub const LINEAGE_DIGEST_VERSION: u32 = 1;
pub const CURRENT_EVENT_CHAIN_VERSION: u32 = 1;

const DOCUMENT_DOMAIN: &str = "proof-of-thought/document";
const EVENT_DOMAIN: &str = "proof-of-thought/event-chain";
const UPDATE_LOG_DOMAIN: &str = "proof-of-thought/yjs-update-log";
const LINEAGE_DOMAIN: &str = "proof-of-thought/live-lineage";

/// The durable action represented by one provenance event.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventAction {
    Edit,
    Trash,
    Restore,
    LegacySeed,
    Suggestion,
    Accept,
    Reject,
}

/// Actor facts copied into an event so later connection/model changes cannot
/// rewrite the historical envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ActorEventMetadata<'a> {
    pub actor_id: Option<&'a str>,
    pub actor_label: &'a str,
    pub provider: Option<&'a str>,
    pub requested_model: Option<&'a str>,
    pub reported_model: Option<&'a str>,
    pub connection_id: Option<&'a str>,
    pub session_id: Option<&'a str>,
}

/// Optional identifiers linking an event to local evidence and UI flows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EventReferences<'a> {
    pub evidence_ref: Option<&'a str>,
    pub suggestion_id: Option<&'a str>,
    pub client_event_id: Option<&'a str>,
}

/// Typed input to the append-only event-chain hash.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EventHashInput<'a> {
    pub chain_version: u32,
    pub event_id: SourceId,
    pub document_id: &'a str,
    pub update_seq: Option<u64>,
    pub action: EventAction,
    pub ingress: Ingress,
    pub assurance: Assurance,
    /// Frozen consumer label for the source. This is distinct from the actor
    /// label because a reviewer/provider name can differ from its submitter.
    pub source_label: &'a str,
    pub actor: ActorEventMetadata<'a>,
    pub references: EventReferences<'a>,
    pub created_at_ms: i64,
    pub recorded_at_ms: i64,
    pub before_document_hash: EvidenceDigest,
    pub after_document_hash: EvidenceDigest,
    /// Cumulative, document-local root through `update_seq`. This binds the
    /// exact opaque Yjs bytes and immutable update metadata, including legacy
    /// history that predates semantic events.
    pub update_log_root: EvidenceDigest,
    pub previous_event_hash: Option<EvidenceDigest>,
    pub deltas: &'a [DeltaSegment],
}

/// One immutable row folded into a document-local Yjs update-log root.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateLogEntry<'a> {
    pub document_id: &'a str,
    pub seq: u64,
    pub payload: &'a [u8],
    pub actor_id: &'a str,
    pub origin: &'a str,
    pub session_id: Option<&'a str>,
    pub created_at_ms: i64,
}

/// Typed input to the current, rebuildable lineage digest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineageHashInput<'a> {
    pub algorithm_version: u32,
    pub document_id: &'a str,
    pub through_update_seq: u64,
    pub through_event_id: SourceId,
    /// Spans must be in the canonical current-document order. Reordering them
    /// changes the digest by design.
    pub spans: &'a [LiveLineageSpan],
}

/// Hash ordered semantic document snapshots and replicated document metadata.
pub fn document_digest(blocks: &[BlockSnapshot], deleted_at: Option<i64>) -> EvidenceDigest {
    let mut encoder = Encoder::root(DOCUMENT_DOMAIN, DOCUMENT_DIGEST_VERSION);
    encoder.list(10, blocks.iter().map(encode_block));
    encoder.optional_i64(11, deleted_at);
    encoder.digest()
}

/// Root for a document with no durable Yjs update rows.
pub fn empty_update_log_digest(document_id: &str) -> EvidenceDigest {
    let mut encoder = Encoder::root(UPDATE_LOG_DOMAIN, UPDATE_LOG_DIGEST_VERSION);
    encoder.string(10, document_id);
    encoder.digest()
}

/// Extend the cumulative root with one exact Yjs update-log row.
pub fn update_log_digest(
    previous: Option<EvidenceDigest>,
    entry: &UpdateLogEntry<'_>,
) -> EvidenceDigest {
    let mut encoder = Encoder::root(UPDATE_LOG_DOMAIN, UPDATE_LOG_DIGEST_VERSION);
    encoder.optional_fixed(10, previous.as_ref().map(|digest| digest.as_slice()));
    encoder.string(11, entry.document_id);
    encoder.u64(12, entry.seq);
    encoder.fixed(13, entry.payload);
    encoder.string(14, entry.actor_id);
    encoder.string(15, entry.origin);
    encoder.optional_string(16, entry.session_id);
    encoder.i64(17, entry.created_at_ms);
    encoder.digest()
}

/// Hash one event and bind it to the previous event digest.
pub fn event_chain_digest(input: &EventHashInput<'_>) -> EvidenceDigest {
    let mut encoder = Encoder::root(EVENT_DOMAIN, EVENT_DIGEST_ENCODING_VERSION);
    encoder.u32(10, input.chain_version);
    encoder.u64(11, input.event_id.0);
    encoder.string(12, input.document_id);
    encoder.optional_u64(13, input.update_seq);
    encoder.u8(14, action_code(input.action));
    encoder.u8(15, ingress_code(input.ingress));
    encoder.u8(16, assurance_code(input.assurance));
    encoder.nested(17, encode_actor(input.actor));
    encoder.nested(18, encode_references(input.references));
    encoder.i64(19, input.created_at_ms);
    encoder.i64(20, input.recorded_at_ms);
    encoder.fixed(21, &input.before_document_hash);
    encoder.fixed(22, &input.after_document_hash);
    encoder.fixed(23, &input.update_log_root);
    encoder.optional_fixed(
        24,
        input
            .previous_event_hash
            .as_ref()
            .map(|hash| hash.as_slice()),
    );
    encoder.list(25, input.deltas.iter().map(encode_delta));
    encoder.string(26, input.source_label);
    encoder.digest()
}

/// Hash the ordered current lineage read model and its rebuild watermark.
pub fn live_lineage_digest(input: &LineageHashInput<'_>) -> EvidenceDigest {
    let mut encoder = Encoder::root(LINEAGE_DOMAIN, LINEAGE_DIGEST_VERSION);
    encoder.u32(10, input.algorithm_version);
    encoder.string(11, input.document_id);
    encoder.u64(12, input.through_update_seq);
    encoder.u64(13, input.through_event_id.0);
    encoder.list(14, input.spans.iter().map(encode_span));
    encoder.digest()
}

fn encode_block(block: &BlockSnapshot) -> Vec<u8> {
    let mut encoder = Encoder::new();
    encoder.string(1, &block.block_id);
    encoder.string(2, &block.kind);
    encoder.string(3, &block.structure_key);
    encoder.list(4, block.leaves.iter().map(encode_leaf));
    encoder.into_bytes()
}

fn encode_leaf(leaf: &TextLeafSnapshot) -> Vec<u8> {
    let mut encoder = Encoder::new();
    encoder.u32_list(1, &leaf.path);
    encoder.string(2, &leaf.text);
    encoder.string(3, &leaf.format_key);
    encoder.into_bytes()
}

fn encode_actor(actor: ActorEventMetadata<'_>) -> Vec<u8> {
    let mut encoder = Encoder::new();
    encoder.optional_string(1, actor.actor_id);
    encoder.string(2, actor.actor_label);
    encoder.optional_string(3, actor.provider);
    encoder.optional_string(4, actor.requested_model);
    encoder.optional_string(5, actor.reported_model);
    encoder.optional_string(6, actor.connection_id);
    encoder.optional_string(7, actor.session_id);
    encoder.into_bytes()
}

fn encode_references(references: EventReferences<'_>) -> Vec<u8> {
    let mut encoder = Encoder::new();
    encoder.optional_string(1, references.evidence_ref);
    encoder.optional_string(2, references.suggestion_id);
    encoder.optional_string(3, references.client_event_id);
    encoder.into_bytes()
}

fn encode_delta(delta: &DeltaSegment) -> Vec<u8> {
    let mut encoder = Encoder::new();
    match delta {
        DeltaSegment::Insert {
            event_source_id,
            after,
            text,
        } => {
            encoder.u8(1, 1);
            encoder.u64(2, event_source_id.0);
            encoder.nested(3, encode_text_location(after));
            encoder.string(4, text);
        }
        DeltaSegment::Delete {
            event_source_id,
            content_source_id,
            before,
            text,
        } => {
            encoder.u8(1, 2);
            encoder.u64(2, event_source_id.0);
            encoder.u64(3, content_source_id.0);
            encoder.nested(4, encode_text_location(before));
            encoder.string(5, text);
        }
        DeltaSegment::Format {
            event_source_id,
            content_source_id,
            before,
            after,
            text,
            before_format,
            after_format,
        } => {
            encoder.u8(1, 3);
            encoder.u64(2, event_source_id.0);
            encoder.u64(3, content_source_id.0);
            encoder.nested(4, encode_text_location(before));
            encoder.nested(5, encode_text_location(after));
            encoder.string(6, text);
            encoder.string(7, before_format);
            encoder.string(8, after_format);
        }
        DeltaSegment::Structure {
            event_source_id,
            before,
            after,
        } => {
            encoder.u8(1, 4);
            encoder.u64(2, event_source_id.0);
            encoder.optional_nested(3, before.as_ref().map(encode_block_shape));
            encoder.optional_nested(4, after.as_ref().map(encode_block_shape));
        }
    }
    encoder.into_bytes()
}

fn encode_span(span: &LiveLineageSpan) -> Vec<u8> {
    let mut encoder = Encoder::new();
    encoder.nested(1, encode_text_location(&span.location));
    encoder.u64(2, span.source_id.0);
    encoder.into_bytes()
}

fn encode_text_location(location: &TextLocation) -> Vec<u8> {
    let mut encoder = Encoder::new();
    encoder.string(1, &location.block_id);
    encoder.u32_list(2, &location.path);
    encoder.u32(3, location.from_utf16);
    encoder.u32(4, location.to_utf16);
    encoder.into_bytes()
}

fn encode_block_shape(shape: &BlockShape) -> Vec<u8> {
    let mut encoder = Encoder::new();
    encoder.nested(1, encode_block_location(&shape.location));
    encoder.string(2, &shape.kind);
    encoder.string(3, &shape.structure_key);
    encoder.into_bytes()
}

fn encode_block_location(location: &BlockLocation) -> Vec<u8> {
    let mut encoder = Encoder::new();
    encoder.string(1, &location.block_id);
    encoder.u64(
        2,
        u64::try_from(location.index).expect("block index fits in canonical u64"),
    );
    encoder.into_bytes()
}

fn action_code(action: EventAction) -> u8 {
    match action {
        EventAction::Edit => 1,
        EventAction::Trash => 2,
        EventAction::Restore => 3,
        EventAction::LegacySeed => 4,
        EventAction::Suggestion => 5,
        EventAction::Accept => 6,
        EventAction::Reject => 7,
    }
}

fn ingress_code(ingress: Ingress) -> u8 {
    match ingress {
        Ingress::Entered => 1,
        Ingress::Command => 2,
        Ingress::Pasted => 3,
        Ingress::Imported => 4,
        Ingress::Mcp => 5,
        Ingress::Api => 6,
        Ingress::Suggestion => 7,
        Ingress::Unknown => 8,
        Ingress::LegacyUnknown => 9,
    }
}

fn assurance_code(assurance: Assurance) -> u8 {
    match assurance {
        Assurance::Observed => 1,
        Assurance::Reported => 2,
        Assurance::Verified => 3,
        Assurance::Unknown => 4,
    }
}

/// Minimal tagged-length-value encoder. Tags are two-byte big-endian values;
/// lengths and list counts are eight-byte big-endian values.
struct Encoder {
    bytes: Vec<u8>,
}

impl Encoder {
    fn new() -> Self {
        Self { bytes: Vec::new() }
    }

    fn root(domain: &str, version: u32) -> Self {
        let mut encoder = Self::new();
        encoder.fixed(1, b"ProofOfThought canonical evidence");
        encoder.string(2, domain);
        encoder.u32(3, version);
        encoder
    }

    fn field(&mut self, tag: u16, value: &[u8]) {
        self.bytes.extend_from_slice(&tag.to_be_bytes());
        self.bytes.extend_from_slice(
            &u64::try_from(value.len())
                .expect("canonical field length fits u64")
                .to_be_bytes(),
        );
        self.bytes.extend_from_slice(value);
    }

    fn u8(&mut self, tag: u16, value: u8) {
        self.field(tag, &[value]);
    }

    fn u32(&mut self, tag: u16, value: u32) {
        self.field(tag, &value.to_be_bytes());
    }

    fn u64(&mut self, tag: u16, value: u64) {
        self.field(tag, &value.to_be_bytes());
    }

    fn i64(&mut self, tag: u16, value: i64) {
        self.field(tag, &value.to_be_bytes());
    }

    fn string(&mut self, tag: u16, value: &str) {
        self.field(tag, value.as_bytes());
    }

    fn fixed(&mut self, tag: u16, value: &[u8]) {
        self.field(tag, value);
    }

    fn nested(&mut self, tag: u16, value: Vec<u8>) {
        self.field(tag, &value);
    }

    fn optional_string(&mut self, tag: u16, value: Option<&str>) {
        let encoded = value.map_or_else(|| vec![0], |value| option_payload(value.as_bytes()));
        self.field(tag, &encoded);
    }

    fn optional_u64(&mut self, tag: u16, value: Option<u64>) {
        let encoded = value.map_or_else(|| vec![0], |value| option_payload(&value.to_be_bytes()));
        self.field(tag, &encoded);
    }

    fn optional_i64(&mut self, tag: u16, value: Option<i64>) {
        let encoded = value.map_or_else(|| vec![0], |value| option_payload(&value.to_be_bytes()));
        self.field(tag, &encoded);
    }

    fn optional_fixed(&mut self, tag: u16, value: Option<&[u8]>) {
        let encoded = value.map_or_else(|| vec![0], option_payload);
        self.field(tag, &encoded);
    }

    fn optional_nested(&mut self, tag: u16, value: Option<Vec<u8>>) {
        let encoded = value.map_or_else(|| vec![0], |value| option_payload(&value));
        self.field(tag, &encoded);
    }

    fn u32_list(&mut self, tag: u16, values: &[u32]) {
        let mut encoded = Vec::with_capacity(8 + values.len() * 4);
        encoded.extend_from_slice(
            &u64::try_from(values.len())
                .expect("canonical list length fits u64")
                .to_be_bytes(),
        );
        for value in values {
            encoded.extend_from_slice(&value.to_be_bytes());
        }
        self.field(tag, &encoded);
    }

    fn list(&mut self, tag: u16, values: impl Iterator<Item = Vec<u8>>) {
        let values = values.collect::<Vec<_>>();
        let mut encoded = Vec::new();
        encoded.extend_from_slice(
            &u64::try_from(values.len())
                .expect("canonical list length fits u64")
                .to_be_bytes(),
        );
        for value in values {
            encoded.extend_from_slice(
                &u64::try_from(value.len())
                    .expect("canonical item length fits u64")
                    .to_be_bytes(),
            );
            encoded.extend_from_slice(&value);
        }
        self.field(tag, &encoded);
    }

    fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    fn digest(self) -> EvidenceDigest {
        Sha256::digest(self.bytes).into()
    }
}

fn option_payload(value: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(1 + 8 + value.len());
    encoded.push(1);
    encoded.extend_from_slice(
        &u64::try_from(value.len())
            .expect("canonical option length fits u64")
            .to_be_bytes(),
    );
    encoded.extend_from_slice(value);
    encoded
}

#[cfg(test)]
mod tests {
    use super::*;

    fn location(block: &str, path: &[u32], from: u32, to: u32) -> TextLocation {
        TextLocation {
            block_id: block.into(),
            path: path.to_vec(),
            from_utf16: from,
            to_utf16: to,
        }
    }

    fn blocks() -> Vec<BlockSnapshot> {
        vec![
            BlockSnapshot::new(
                "a",
                "paragraph",
                r#"{"type":"paragraph","content":[{"type":"text"}]}"#,
                vec![
                    TextLeafSnapshot::new(vec![0], "Hello", "bold"),
                    TextLeafSnapshot::new(vec![1], "!", "italic"),
                ],
            ),
            BlockSnapshot::plain("b", "heading", "World"),
        ]
    }

    fn deltas() -> Vec<DeltaSegment> {
        vec![
            DeltaSegment::Delete {
                event_source_id: SourceId(7),
                content_source_id: SourceId(2),
                before: location("a", &[0], 4, 5),
                text: "o".into(),
            },
            DeltaSegment::Insert {
                event_source_id: SourceId(7),
                after: location("a", &[0], 4, 5),
                text: "!".into(),
            },
            DeltaSegment::Format {
                event_source_id: SourceId(7),
                content_source_id: SourceId(2),
                before: location("a", &[0], 0, 4),
                after: location("a", &[0], 0, 4),
                text: "Hell".into(),
                before_format: "bold".into(),
                after_format: "italic".into(),
            },
            DeltaSegment::Structure {
                event_source_id: SourceId(7),
                before: Some(BlockShape {
                    location: BlockLocation {
                        block_id: "b".into(),
                        index: 1,
                    },
                    kind: "paragraph".into(),
                    structure_key: "before".into(),
                }),
                after: Some(BlockShape {
                    location: BlockLocation {
                        block_id: "b".into(),
                        index: 1,
                    },
                    kind: "heading".into(),
                    structure_key: "after".into(),
                }),
            },
        ]
    }

    fn event<'a>(deltas: &'a [DeltaSegment]) -> EventHashInput<'a> {
        EventHashInput {
            chain_version: CURRENT_EVENT_CHAIN_VERSION,
            event_id: SourceId(7),
            document_id: "document-1",
            update_seq: Some(6),
            action: EventAction::Edit,
            ingress: Ingress::Mcp,
            assurance: Assurance::Reported,
            source_label: "Claude (reported)",
            actor: ActorEventMetadata {
                actor_id: Some("agent:claude"),
                actor_label: "Claude",
                provider: Some("anthropic"),
                requested_model: Some("claude-sonnet"),
                reported_model: Some("claude-sonnet-4"),
                connection_id: Some("connection-1"),
                session_id: Some("session-1"),
            },
            references: EventReferences {
                evidence_ref: Some("evidence-1"),
                suggestion_id: Some("suggestion-1"),
                client_event_id: Some("client-event-1"),
            },
            created_at_ms: 1_700_000_000_000,
            recorded_at_ms: 1_700_000_000_123,
            before_document_hash: [1; 32],
            after_document_hash: [2; 32],
            update_log_root: [4; 32],
            previous_event_hash: Some([3; 32]),
            deltas,
        }
    }

    fn spans() -> Vec<LiveLineageSpan> {
        vec![
            LiveLineageSpan {
                location: location("a", &[0], 0, 5),
                source_id: SourceId(2),
            },
            LiveLineageSpan {
                location: location("b", &[0], 0, 5),
                source_id: SourceId(7),
            },
        ]
    }

    fn assert_event_changed(base: EvidenceDigest, changed: &EventHashInput<'_>) {
        assert_ne!(base, event_chain_digest(changed));
    }

    fn assert_delta_change(
        base: EvidenceDigest,
        original: &[DeltaSegment],
        mutate: impl FnOnce(&mut [DeltaSegment]),
    ) {
        let mut changed = original.to_vec();
        mutate(&mut changed);
        assert_ne!(base, event_chain_digest(&event(&changed)));
    }

    #[test]
    fn document_digest_is_deterministic_and_covers_every_field_and_order() {
        let original = blocks();
        let base = document_digest(&original, None);
        assert_eq!(base, document_digest(&original, None));

        let mut changed = original.clone();
        changed.reverse();
        assert_ne!(base, document_digest(&changed, None));
        let mut changed = original.clone();
        changed[0].leaves.reverse();
        assert_ne!(base, document_digest(&changed, None));

        for mutate in [
            |blocks: &mut Vec<BlockSnapshot>| blocks[0].block_id.push('x'),
            |blocks: &mut Vec<BlockSnapshot>| blocks[0].kind.push('x'),
            |blocks: &mut Vec<BlockSnapshot>| blocks[0].structure_key.push('x'),
            |blocks: &mut Vec<BlockSnapshot>| blocks[0].leaves[0].path.push(9),
            |blocks: &mut Vec<BlockSnapshot>| blocks[0].leaves[0].text.push('x'),
            |blocks: &mut Vec<BlockSnapshot>| blocks[0].leaves[0].format_key.push('x'),
        ] {
            let mut changed = original.clone();
            mutate(&mut changed);
            assert_ne!(base, document_digest(&changed, None));
        }
        assert_ne!(base, document_digest(&original, Some(1)));
    }

    #[test]
    fn update_log_digest_covers_every_immutable_field_previous_root_and_order() {
        let entry = UpdateLogEntry {
            document_id: "document-1",
            seq: 7,
            payload: b"opaque-yjs-update",
            actor_id: "human:editor",
            origin: "human",
            session_id: Some("window-1"),
            created_at_ms: 1_700_000_000_000,
        };
        let previous = [3; 32];
        let base = update_log_digest(Some(previous), &entry);
        assert_eq!(base, update_log_digest(Some(previous), &entry));

        for changed in [
            UpdateLogEntry {
                document_id: "document-2",
                ..entry
            },
            UpdateLogEntry { seq: 8, ..entry },
            UpdateLogEntry {
                payload: b"different-update",
                ..entry
            },
            UpdateLogEntry {
                actor_id: "agent:claude",
                ..entry
            },
            UpdateLogEntry {
                origin: "agent",
                ..entry
            },
            UpdateLogEntry {
                session_id: None,
                ..entry
            },
            UpdateLogEntry {
                created_at_ms: entry.created_at_ms + 1,
                ..entry
            },
        ] {
            assert_ne!(base, update_log_digest(Some(previous), &changed));
        }
        assert_ne!(base, update_log_digest(None, &entry));
        assert_ne!(base, update_log_digest(Some([4; 32]), &entry));

        let next = UpdateLogEntry {
            seq: 8,
            payload: b"next-update",
            created_at_ms: entry.created_at_ms + 1,
            ..entry
        };
        let forward = update_log_digest(Some(base), &next);
        let reversed_first = update_log_digest(Some(previous), &next);
        let reversed = update_log_digest(Some(reversed_first), &entry);
        assert_ne!(forward, reversed);
    }

    #[test]
    fn digest_domains_are_distinct_even_for_empty_collections() {
        let document = document_digest(&[], None);
        let lineage = live_lineage_digest(&LineageHashInput {
            algorithm_version: 1,
            document_id: "",
            through_update_seq: 0,
            through_event_id: SourceId(0),
            spans: &[],
        });

        assert_ne!(document, lineage);
    }

    #[test]
    fn event_digest_covers_envelope_metadata_hashes_and_delta_order() {
        let deltas = deltas();
        let input = event(&deltas);
        let base = event_chain_digest(&input);
        assert_eq!(base, event_chain_digest(&input));

        let mut changed = input.clone();
        changed.chain_version += 1;
        assert_event_changed(base, &changed);
        let mut changed = input.clone();
        changed.event_id = SourceId(8);
        assert_event_changed(base, &changed);
        let mut changed = input.clone();
        changed.document_id = "document-2";
        assert_event_changed(base, &changed);
        let mut changed = input.clone();
        changed.update_seq = None;
        assert_event_changed(base, &changed);
        let mut changed = input.clone();
        changed.action = EventAction::Reject;
        assert_event_changed(base, &changed);
        let mut changed = input.clone();
        changed.ingress = Ingress::Command;
        assert_event_changed(base, &changed);
        let mut changed = input.clone();
        changed.assurance = Assurance::Verified;
        assert_event_changed(base, &changed);
        let mut changed = input.clone();
        changed.source_label = "ChatGPT (reported)";
        assert_event_changed(base, &changed);

        let actor_changes = [
            ActorEventMetadata {
                actor_id: None,
                ..input.actor
            },
            ActorEventMetadata {
                actor_label: "ChatGPT",
                ..input.actor
            },
            ActorEventMetadata {
                provider: None,
                ..input.actor
            },
            ActorEventMetadata {
                requested_model: None,
                ..input.actor
            },
            ActorEventMetadata {
                reported_model: None,
                ..input.actor
            },
            ActorEventMetadata {
                connection_id: None,
                ..input.actor
            },
            ActorEventMetadata {
                session_id: None,
                ..input.actor
            },
        ];
        for actor in actor_changes {
            let mut changed = input.clone();
            changed.actor = actor;
            assert_event_changed(base, &changed);
        }

        let reference_changes = [
            EventReferences {
                evidence_ref: None,
                ..input.references
            },
            EventReferences {
                suggestion_id: None,
                ..input.references
            },
            EventReferences {
                client_event_id: None,
                ..input.references
            },
        ];
        for references in reference_changes {
            let mut changed = input.clone();
            changed.references = references;
            assert_event_changed(base, &changed);
        }

        let mut changed = input.clone();
        changed.created_at_ms += 1;
        assert_event_changed(base, &changed);
        let mut changed = input.clone();
        changed.recorded_at_ms += 1;
        assert_event_changed(base, &changed);
        let mut changed = input.clone();
        changed.before_document_hash[0] ^= 1;
        assert_event_changed(base, &changed);
        let mut changed = input.clone();
        changed.after_document_hash[0] ^= 1;
        assert_event_changed(base, &changed);
        let mut changed = input.clone();
        changed.update_log_root[0] ^= 1;
        assert_event_changed(base, &changed);
        let mut changed = input.clone();
        changed.previous_event_hash = None;
        assert_event_changed(base, &changed);

        let mut reordered = deltas.clone();
        reordered.reverse();
        let reordered_input = EventHashInput {
            deltas: &reordered,
            ..input
        };
        assert_event_changed(base, &reordered_input);
    }

    #[test]
    fn every_delta_variant_and_material_field_changes_the_event_digest() {
        let original = deltas();
        let base = event_chain_digest(&event(&original));

        // Delete.
        assert_delta_change(base, &original, |deltas| {
            let DeltaSegment::Delete {
                event_source_id, ..
            } = &mut deltas[0]
            else {
                unreachable!()
            };
            event_source_id.0 += 1;
        });
        assert_delta_change(base, &original, |deltas| {
            let DeltaSegment::Delete {
                content_source_id, ..
            } = &mut deltas[0]
            else {
                unreachable!()
            };
            content_source_id.0 += 1;
        });
        for mutate in [
            |location: &mut TextLocation| location.block_id.push('x'),
            |location: &mut TextLocation| location.path.push(9),
            |location: &mut TextLocation| location.from_utf16 += 1,
            |location: &mut TextLocation| location.to_utf16 += 1,
        ] {
            assert_delta_change(base, &original, |deltas| {
                let DeltaSegment::Delete { before, .. } = &mut deltas[0] else {
                    unreachable!()
                };
                mutate(before);
            });
        }
        assert_delta_change(base, &original, |deltas| {
            let DeltaSegment::Delete { text, .. } = &mut deltas[0] else {
                unreachable!()
            };
            text.push('x');
        });

        // Insert.
        assert_delta_change(base, &original, |deltas| {
            let DeltaSegment::Insert {
                event_source_id, ..
            } = &mut deltas[1]
            else {
                unreachable!()
            };
            event_source_id.0 += 1;
        });
        for mutate in [
            |location: &mut TextLocation| location.block_id.push('x'),
            |location: &mut TextLocation| location.path.push(9),
            |location: &mut TextLocation| location.from_utf16 += 1,
            |location: &mut TextLocation| location.to_utf16 += 1,
        ] {
            assert_delta_change(base, &original, |deltas| {
                let DeltaSegment::Insert { after, .. } = &mut deltas[1] else {
                    unreachable!()
                };
                mutate(after);
            });
        }
        assert_delta_change(base, &original, |deltas| {
            let DeltaSegment::Insert { text, .. } = &mut deltas[1] else {
                unreachable!()
            };
            text.push('x');
        });

        // Format.
        assert_delta_change(base, &original, |deltas| {
            let DeltaSegment::Format {
                event_source_id, ..
            } = &mut deltas[2]
            else {
                unreachable!()
            };
            event_source_id.0 += 1;
        });
        assert_delta_change(base, &original, |deltas| {
            let DeltaSegment::Format {
                content_source_id, ..
            } = &mut deltas[2]
            else {
                unreachable!()
            };
            content_source_id.0 += 1;
        });
        for before in [true, false] {
            for mutate in [
                |location: &mut TextLocation| location.block_id.push('x'),
                |location: &mut TextLocation| location.path.push(9),
                |location: &mut TextLocation| location.from_utf16 += 1,
                |location: &mut TextLocation| location.to_utf16 += 1,
            ] {
                assert_delta_change(base, &original, |deltas| {
                    let DeltaSegment::Format {
                        before: before_location,
                        after: after_location,
                        ..
                    } = &mut deltas[2]
                    else {
                        unreachable!()
                    };
                    mutate(if before {
                        before_location
                    } else {
                        after_location
                    });
                });
            }
        }
        for field in 0..3 {
            assert_delta_change(base, &original, |deltas| {
                let DeltaSegment::Format {
                    text,
                    before_format,
                    after_format,
                    ..
                } = &mut deltas[2]
                else {
                    unreachable!()
                };
                match field {
                    0 => text.push('x'),
                    1 => before_format.push('x'),
                    _ => after_format.push('x'),
                }
            });
        }

        // Structure, including optionality and every field of both shapes.
        assert_delta_change(base, &original, |deltas| {
            let DeltaSegment::Structure {
                event_source_id, ..
            } = &mut deltas[3]
            else {
                unreachable!()
            };
            event_source_id.0 += 1;
        });
        for before in [true, false] {
            assert_delta_change(base, &original, |deltas| {
                let DeltaSegment::Structure {
                    before: before_shape,
                    after: after_shape,
                    ..
                } = &mut deltas[3]
                else {
                    unreachable!()
                };
                if before {
                    *before_shape = None;
                } else {
                    *after_shape = None;
                }
            });
            for field in 0..4 {
                assert_delta_change(base, &original, |deltas| {
                    let DeltaSegment::Structure {
                        before: before_shape,
                        after: after_shape,
                        ..
                    } = &mut deltas[3]
                    else {
                        unreachable!()
                    };
                    let shape = if before {
                        before_shape.as_mut().unwrap()
                    } else {
                        after_shape.as_mut().unwrap()
                    };
                    match field {
                        0 => shape.location.block_id.push('x'),
                        1 => shape.location.index += 1,
                        2 => shape.kind.push('x'),
                        _ => shape.structure_key.push('x'),
                    }
                });
            }
        }
    }

    #[test]
    fn lineage_digest_covers_watermarks_spans_fields_and_order() {
        let spans = spans();
        let input = LineageHashInput {
            algorithm_version: 1,
            document_id: "document-1",
            through_update_seq: 6,
            through_event_id: SourceId(7),
            spans: &spans,
        };
        let base = live_lineage_digest(&input);
        assert_eq!(base, live_lineage_digest(&input));

        let mut changed = input.clone();
        changed.algorithm_version += 1;
        assert_ne!(base, live_lineage_digest(&changed));
        let mut changed = input.clone();
        changed.document_id = "document-2";
        assert_ne!(base, live_lineage_digest(&changed));
        let mut changed = input.clone();
        changed.through_update_seq += 1;
        assert_ne!(base, live_lineage_digest(&changed));
        let mut changed = input.clone();
        changed.through_event_id.0 += 1;
        assert_ne!(base, live_lineage_digest(&changed));

        let mut reordered = spans.clone();
        reordered.reverse();
        assert_ne!(
            base,
            live_lineage_digest(&LineageHashInput {
                spans: &reordered,
                ..input.clone()
            })
        );

        for mutate in [
            |spans: &mut Vec<LiveLineageSpan>| spans[0].location.block_id.push('x'),
            |spans: &mut Vec<LiveLineageSpan>| spans[0].location.path.push(2),
            |spans: &mut Vec<LiveLineageSpan>| spans[0].location.from_utf16 += 1,
            |spans: &mut Vec<LiveLineageSpan>| spans[0].location.to_utf16 += 1,
            |spans: &mut Vec<LiveLineageSpan>| spans[0].source_id.0 += 1,
        ] {
            let mut changed_spans = spans.clone();
            mutate(&mut changed_spans);
            assert_ne!(
                base,
                live_lineage_digest(&LineageHashInput {
                    spans: &changed_spans,
                    ..input.clone()
                })
            );
        }
    }
}
