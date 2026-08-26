//! Checked adapters between semantic provenance values and their SQLite rows.
//!
//! Node paths and block shapes use canonical typed JSON. The adapters never
//! build those values with string concatenation, and hydration rejects a JSON
//! representation that does not round-trip to the canonical encoding.

use crate::mutation::{action_name, assurance_name, ingress_name, source_group_key};
use crate::provenance_hash::EventAction;
use serde::{Serialize, de::DeserializeOwned};
use std::collections::BTreeMap;
use std::fmt;
use thought_provenance::{
    Assurance, DeltaSegment, Ingress, LiveLineageSpan, SourceDescriptor, SourceId, TextLocation,
};
use thought_store::{
    LineageSpanInput, LineageSpanRow, ProvenanceChangeInput, ProvenanceChangeRow,
    ProvenanceEventRow,
};

/// Persisted live-lineage values ready to pass to `LineageState::from_parts`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PersistedLineageParts {
    pub spans: Vec<LiveLineageSpan>,
    pub sources: BTreeMap<SourceId, SourceDescriptor>,
}

/// A persisted provenance value was not a lossless representation of the
/// typed semantic model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProvenanceStoreError {
    SourceIdOutOfRange(SourceId),
    InvalidStoredId {
        field: &'static str,
        value: i64,
    },
    InvalidStoredOffset {
        field: &'static str,
        value: i64,
    },
    InvalidTextRange {
        block_id: String,
        from_utf16: i64,
        to_utf16: i64,
    },
    EventSourceMismatch {
        expected: SourceId,
        found: SourceId,
    },
    ChangeEventMismatch {
        ordinal: i64,
        expected: SourceId,
        found: i64,
    },
    InvalidOrdinal {
        position: usize,
        found: i64,
    },
    InvalidChangeLayout {
        ordinal: i64,
        field: &'static str,
    },
    Json {
        field: &'static str,
        message: String,
    },
    NonCanonicalJson {
        field: &'static str,
    },
    UnknownPersistedValue {
        field: &'static str,
        value: String,
    },
    UnsupportedClassification {
        ingress: Ingress,
        assurance: Assurance,
    },
    MissingSourceLabel {
        event_id: SourceId,
    },
    DocumentMismatch {
        row: &'static str,
        expected: String,
        found: String,
    },
    DuplicateEventId(SourceId),
    MissingSourceEvent {
        document_id: String,
        source_event_id: SourceId,
    },
}

impl fmt::Display for ProvenanceStoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::SourceIdOutOfRange(id) => {
                write!(f, "source event id {} does not fit SQLite", id.0)
            }
            Self::InvalidStoredId { field, value } => {
                write!(f, "stored {field} must be positive, found {value}")
            }
            Self::InvalidStoredOffset { field, value } => {
                write!(
                    f,
                    "stored {field} is outside the UTF-16 offset range: {value}"
                )
            }
            Self::InvalidTextRange {
                block_id,
                from_utf16,
                to_utf16,
            } => write!(
                f,
                "invalid UTF-16 range {from_utf16}..{to_utf16} in block `{block_id}`"
            ),
            Self::EventSourceMismatch { expected, found } => write!(
                f,
                "delta belongs to event {}, expected {}",
                found.0, expected.0
            ),
            Self::ChangeEventMismatch {
                ordinal,
                expected,
                found,
            } => write!(
                f,
                "stored change {ordinal} belongs to event {found}, expected {}",
                expected.0
            ),
            Self::InvalidOrdinal { position, found } => write!(
                f,
                "stored change at position {position} has ordinal {found}"
            ),
            Self::InvalidChangeLayout { ordinal, field } => {
                write!(f, "stored change {ordinal} has an invalid `{field}` field")
            }
            Self::Json { field, message } => {
                write!(f, "invalid JSON in `{field}`: {message}")
            }
            Self::NonCanonicalJson { field } => {
                write!(f, "stored `{field}` is valid JSON but is not canonical")
            }
            Self::UnknownPersistedValue { field, value } => {
                write!(f, "unknown persisted {field} value `{value}`")
            }
            Self::UnsupportedClassification { ingress, assurance } => write!(
                f,
                "unsupported persisted source classification {ingress:?}/{assurance:?}"
            ),
            Self::MissingSourceLabel { event_id } => {
                write!(
                    f,
                    "source event {} has no frozen consumer label",
                    event_id.0
                )
            }
            Self::DocumentMismatch {
                row,
                expected,
                found,
            } => write!(
                f,
                "{row} belongs to document `{found}`, expected `{expected}`"
            ),
            Self::DuplicateEventId(id) => {
                write!(f, "duplicate persisted source event {}", id.0)
            }
            Self::MissingSourceEvent {
                document_id,
                source_event_id,
            } => write!(
                f,
                "lineage in document `{document_id}` refers to missing source event {}",
                source_event_id.0
            ),
        }
    }
}

impl std::error::Error for ProvenanceStoreError {}

/// Convert an event's ordered semantic deltas to ordered store inputs.
///
/// `event_id` is checked against every segment because delete, format, and
/// structure rows rely on their containing row for the event that performed
/// the change. Insert and delete segments do not carry snapshot format keys,
/// so only explicit format segments populate the store's format columns.
pub fn deltas_to_store(
    event_id: SourceId,
    deltas: &[DeltaSegment],
) -> Result<Vec<ProvenanceChangeInput>, ProvenanceStoreError> {
    source_id_to_i64(event_id)?;
    deltas
        .iter()
        .map(|delta| delta_to_store(event_id, delta))
        .collect()
}

/// Rehydrate ordered store rows into their typed semantic deltas.
///
/// Rows must retain their zero-based ordinal order and belong to `event_id`.
pub fn deltas_from_store(
    event_id: SourceId,
    rows: &[ProvenanceChangeRow],
) -> Result<Vec<DeltaSegment>, ProvenanceStoreError> {
    source_id_to_i64(event_id)?;
    rows.iter()
        .enumerate()
        .map(|(position, row)| {
            let expected_ordinal =
                i64::try_from(position).map_err(|_| ProvenanceStoreError::InvalidOrdinal {
                    position,
                    found: row.ordinal,
                })?;
            if row.ordinal != expected_ordinal {
                return Err(ProvenanceStoreError::InvalidOrdinal {
                    position,
                    found: row.ordinal,
                });
            }
            let row_event_id = source_id_from_i64("event_id", row.event_id)?;
            if row_event_id != event_id {
                return Err(ProvenanceStoreError::ChangeEventMismatch {
                    ordinal: row.ordinal,
                    expected: event_id,
                    found: row.event_id,
                });
            }
            delta_from_store(event_id, row.ordinal, &row.change)
        })
        .collect()
}

/// Convert ordered current lineage spans to store inputs.
pub fn spans_to_store(
    spans: &[LiveLineageSpan],
) -> Result<Vec<LineageSpanInput>, ProvenanceStoreError> {
    spans.iter().map(span_to_store).collect()
}

/// Rehydrate current spans and their immutable source descriptors.
///
/// Every supplied row must belong to `document_id`, and every live span must
/// point to a supplied event from that same document.
pub fn lineage_from_store(
    document_id: &str,
    span_rows: &[LineageSpanRow],
    event_rows: &[ProvenanceEventRow],
) -> Result<PersistedLineageParts, ProvenanceStoreError> {
    let mut sources = BTreeMap::new();
    for row in event_rows {
        require_document("provenance event", document_id, &row.doc_id)?;
        let source_id = source_id_from_i64("event_id", row.event_id)?;
        let _action = parse_action(&row.action)?;
        let ingress = parse_ingress(&row.ingress)?;
        let assurance = parse_assurance(&row.assurance)?;
        let label = source_label(row, source_id, ingress, assurance)?;
        let group_key = source_group_key(
            ingress,
            assurance,
            &label,
            row.connection_id.as_deref(),
            row.provider.as_deref(),
        );
        let descriptor = SourceDescriptor::new(source_id, group_key, label, ingress, assurance);
        if sources.insert(source_id, descriptor).is_some() {
            return Err(ProvenanceStoreError::DuplicateEventId(source_id));
        }
    }

    let mut spans = Vec::with_capacity(span_rows.len());
    for row in span_rows {
        require_document("lineage span", document_id, &row.doc_id)?;
        let source_id = source_id_from_i64("source_event_id", row.span.source_event_id)?;
        if !sources.contains_key(&source_id) {
            return Err(ProvenanceStoreError::MissingSourceEvent {
                document_id: document_id.to_owned(),
                source_event_id: source_id,
            });
        }
        let path = from_canonical_json("node_path", &row.span.node_path)?;
        let from_utf16 = offset_from_i64("start_utf16", row.span.start_utf16)?;
        let to_utf16 = offset_from_i64("end_utf16", row.span.end_utf16)?;
        if from_utf16 >= to_utf16 {
            return Err(ProvenanceStoreError::InvalidTextRange {
                block_id: row.span.block_id.clone(),
                from_utf16: row.span.start_utf16,
                to_utf16: row.span.end_utf16,
            });
        }
        spans.push(LiveLineageSpan {
            location: TextLocation {
                block_id: row.span.block_id.clone(),
                path,
                from_utf16,
                to_utf16,
            },
            source_id,
        });
    }

    Ok(PersistedLineageParts { spans, sources })
}

fn delta_to_store(
    expected_event_id: SourceId,
    delta: &DeltaSegment,
) -> Result<ProvenanceChangeInput, ProvenanceStoreError> {
    match delta {
        DeltaSegment::Insert {
            event_source_id,
            after,
            text,
        } => {
            require_event_source(expected_event_id, *event_source_id)?;
            let after = location_to_store(after)?;
            Ok(ProvenanceChangeInput {
                op: "insert".into(),
                source_event_id: Some(source_id_to_i64(*event_source_id)?),
                before_block_id: None,
                before_path: None,
                before_from_utf16: None,
                before_to_utf16: None,
                after_block_id: Some(after.block_id),
                after_path: Some(after.path),
                after_from_utf16: Some(after.from_utf16),
                after_to_utf16: Some(after.to_utf16),
                before_text: String::new(),
                after_text: text.clone(),
                before_format: None,
                after_format: None,
                before_shape: None,
                after_shape: None,
            })
        }
        DeltaSegment::Delete {
            event_source_id,
            content_source_id,
            before,
            text,
        } => {
            require_event_source(expected_event_id, *event_source_id)?;
            let before = location_to_store(before)?;
            Ok(ProvenanceChangeInput {
                op: "delete".into(),
                source_event_id: Some(source_id_to_i64(*content_source_id)?),
                before_block_id: Some(before.block_id),
                before_path: Some(before.path),
                before_from_utf16: Some(before.from_utf16),
                before_to_utf16: Some(before.to_utf16),
                after_block_id: None,
                after_path: None,
                after_from_utf16: None,
                after_to_utf16: None,
                before_text: text.clone(),
                after_text: String::new(),
                before_format: None,
                after_format: None,
                before_shape: None,
                after_shape: None,
            })
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
            require_event_source(expected_event_id, *event_source_id)?;
            let before = location_to_store(before)?;
            let after = location_to_store(after)?;
            Ok(ProvenanceChangeInput {
                op: "format".into(),
                source_event_id: Some(source_id_to_i64(*content_source_id)?),
                before_block_id: Some(before.block_id),
                before_path: Some(before.path),
                before_from_utf16: Some(before.from_utf16),
                before_to_utf16: Some(before.to_utf16),
                after_block_id: Some(after.block_id),
                after_path: Some(after.path),
                after_from_utf16: Some(after.from_utf16),
                after_to_utf16: Some(after.to_utf16),
                before_text: text.clone(),
                after_text: text.clone(),
                before_format: Some(before_format.clone()),
                after_format: Some(after_format.clone()),
                before_shape: None,
                after_shape: None,
            })
        }
        DeltaSegment::Structure {
            event_source_id,
            before,
            after,
        } => {
            require_event_source(expected_event_id, *event_source_id)?;
            Ok(ProvenanceChangeInput {
                op: "structure".into(),
                source_event_id: None,
                before_block_id: before.as_ref().map(|shape| shape.location.block_id.clone()),
                before_path: None,
                before_from_utf16: None,
                before_to_utf16: None,
                after_block_id: after.as_ref().map(|shape| shape.location.block_id.clone()),
                after_path: None,
                after_from_utf16: None,
                after_to_utf16: None,
                before_text: String::new(),
                after_text: String::new(),
                before_format: None,
                after_format: None,
                before_shape: before
                    .as_ref()
                    .map(|shape| to_canonical_json("before_shape", shape))
                    .transpose()?,
                after_shape: after
                    .as_ref()
                    .map(|shape| to_canonical_json("after_shape", shape))
                    .transpose()?,
            })
        }
    }
}

fn delta_from_store(
    event_id: SourceId,
    ordinal: i64,
    change: &ProvenanceChangeInput,
) -> Result<DeltaSegment, ProvenanceStoreError> {
    match change.op.as_str() {
        "insert" => {
            require_absent_before_location(ordinal, change)?;
            require_empty(ordinal, "before_text", &change.before_text)?;
            require_none(ordinal, "before_format", &change.before_format)?;
            require_none(ordinal, "after_format", &change.after_format)?;
            require_none(ordinal, "before_shape", &change.before_shape)?;
            require_none(ordinal, "after_shape", &change.after_shape)?;
            let source = required_source(ordinal, change.source_event_id)?;
            require_event_source(event_id, source)?;
            Ok(DeltaSegment::Insert {
                event_source_id: event_id,
                after: after_location_from_store(ordinal, change)?,
                text: change.after_text.clone(),
            })
        }
        "delete" => {
            require_absent_after_location(ordinal, change)?;
            require_empty(ordinal, "after_text", &change.after_text)?;
            require_none(ordinal, "before_format", &change.before_format)?;
            require_none(ordinal, "after_format", &change.after_format)?;
            require_none(ordinal, "before_shape", &change.before_shape)?;
            require_none(ordinal, "after_shape", &change.after_shape)?;
            Ok(DeltaSegment::Delete {
                event_source_id: event_id,
                content_source_id: required_source(ordinal, change.source_event_id)?,
                before: before_location_from_store(ordinal, change)?,
                text: change.before_text.clone(),
            })
        }
        "format" => {
            require_none(ordinal, "before_shape", &change.before_shape)?;
            require_none(ordinal, "after_shape", &change.after_shape)?;
            if change.before_text != change.after_text {
                return Err(ProvenanceStoreError::InvalidChangeLayout {
                    ordinal,
                    field: "after_text",
                });
            }
            Ok(DeltaSegment::Format {
                event_source_id: event_id,
                content_source_id: required_source(ordinal, change.source_event_id)?,
                before: before_location_from_store(ordinal, change)?,
                after: after_location_from_store(ordinal, change)?,
                text: change.before_text.clone(),
                before_format: required_string(ordinal, "before_format", &change.before_format)?,
                after_format: required_string(ordinal, "after_format", &change.after_format)?,
            })
        }
        "structure" => {
            require_none(ordinal, "source_event_id", &change.source_event_id)?;
            require_none(ordinal, "before_path", &change.before_path)?;
            require_none(ordinal, "before_from_utf16", &change.before_from_utf16)?;
            require_none(ordinal, "before_to_utf16", &change.before_to_utf16)?;
            require_none(ordinal, "after_path", &change.after_path)?;
            require_none(ordinal, "after_from_utf16", &change.after_from_utf16)?;
            require_none(ordinal, "after_to_utf16", &change.after_to_utf16)?;
            require_empty(ordinal, "before_text", &change.before_text)?;
            require_empty(ordinal, "after_text", &change.after_text)?;
            require_none(ordinal, "before_format", &change.before_format)?;
            require_none(ordinal, "after_format", &change.after_format)?;
            let before = stored_shape(
                ordinal,
                "before_block_id",
                change.before_block_id.as_deref(),
                "before_shape",
                change.before_shape.as_deref(),
            )?;
            let after = stored_shape(
                ordinal,
                "after_block_id",
                change.after_block_id.as_deref(),
                "after_shape",
                change.after_shape.as_deref(),
            )?;
            Ok(DeltaSegment::Structure {
                event_source_id: event_id,
                before,
                after,
            })
        }
        value => Err(ProvenanceStoreError::UnknownPersistedValue {
            field: "op",
            value: value.to_owned(),
        }),
    }
}

struct StoredLocation {
    block_id: String,
    path: String,
    from_utf16: i64,
    to_utf16: i64,
}

fn location_to_store(location: &TextLocation) -> Result<StoredLocation, ProvenanceStoreError> {
    if location.from_utf16 > location.to_utf16 {
        return Err(ProvenanceStoreError::InvalidTextRange {
            block_id: location.block_id.clone(),
            from_utf16: i64::from(location.from_utf16),
            to_utf16: i64::from(location.to_utf16),
        });
    }
    Ok(StoredLocation {
        block_id: location.block_id.clone(),
        path: to_canonical_json("node_path", &location.path)?,
        from_utf16: i64::from(location.from_utf16),
        to_utf16: i64::from(location.to_utf16),
    })
}

fn before_location_from_store(
    ordinal: i64,
    change: &ProvenanceChangeInput,
) -> Result<TextLocation, ProvenanceStoreError> {
    location_from_store(
        ordinal,
        "before_block_id",
        change.before_block_id.as_deref(),
        "before_path",
        change.before_path.as_deref(),
        "before_from_utf16",
        change.before_from_utf16,
        "before_to_utf16",
        change.before_to_utf16,
    )
}

fn after_location_from_store(
    ordinal: i64,
    change: &ProvenanceChangeInput,
) -> Result<TextLocation, ProvenanceStoreError> {
    location_from_store(
        ordinal,
        "after_block_id",
        change.after_block_id.as_deref(),
        "after_path",
        change.after_path.as_deref(),
        "after_from_utf16",
        change.after_from_utf16,
        "after_to_utf16",
        change.after_to_utf16,
    )
}

#[allow(clippy::too_many_arguments)]
fn location_from_store(
    ordinal: i64,
    block_field: &'static str,
    block_id: Option<&str>,
    path_field: &'static str,
    path: Option<&str>,
    from_field: &'static str,
    from_utf16: Option<i64>,
    to_field: &'static str,
    to_utf16: Option<i64>,
) -> Result<TextLocation, ProvenanceStoreError> {
    let block_id = block_id.ok_or(ProvenanceStoreError::InvalidChangeLayout {
        ordinal,
        field: block_field,
    })?;
    let path = path.ok_or(ProvenanceStoreError::InvalidChangeLayout {
        ordinal,
        field: path_field,
    })?;
    let from_utf16 = from_utf16.ok_or(ProvenanceStoreError::InvalidChangeLayout {
        ordinal,
        field: from_field,
    })?;
    let to_utf16 = to_utf16.ok_or(ProvenanceStoreError::InvalidChangeLayout {
        ordinal,
        field: to_field,
    })?;
    let path = from_canonical_json(path_field, path)?;
    let from = offset_from_i64(from_field, from_utf16)?;
    let to = offset_from_i64(to_field, to_utf16)?;
    if from > to {
        return Err(ProvenanceStoreError::InvalidTextRange {
            block_id: block_id.to_owned(),
            from_utf16,
            to_utf16,
        });
    }
    Ok(TextLocation {
        block_id: block_id.to_owned(),
        path,
        from_utf16: from,
        to_utf16: to,
    })
}

fn span_to_store(span: &LiveLineageSpan) -> Result<LineageSpanInput, ProvenanceStoreError> {
    if span.location.from_utf16 >= span.location.to_utf16 {
        return Err(ProvenanceStoreError::InvalidTextRange {
            block_id: span.location.block_id.clone(),
            from_utf16: i64::from(span.location.from_utf16),
            to_utf16: i64::from(span.location.to_utf16),
        });
    }
    Ok(LineageSpanInput {
        block_id: span.location.block_id.clone(),
        node_path: to_canonical_json("node_path", &span.location.path)?,
        start_utf16: i64::from(span.location.from_utf16),
        end_utf16: i64::from(span.location.to_utf16),
        source_event_id: source_id_to_i64(span.source_id)?,
    })
}

fn stored_shape(
    ordinal: i64,
    block_field: &'static str,
    block_id: Option<&str>,
    shape_field: &'static str,
    shape: Option<&str>,
) -> Result<Option<thought_provenance::BlockShape>, ProvenanceStoreError> {
    match (block_id, shape) {
        (None, None) => Ok(None),
        (Some(block_id), Some(shape)) => {
            let shape: thought_provenance::BlockShape = from_canonical_json(shape_field, shape)?;
            if shape.location.block_id != block_id {
                return Err(ProvenanceStoreError::InvalidChangeLayout {
                    ordinal,
                    field: block_field,
                });
            }
            Ok(Some(shape))
        }
        (None, Some(_)) => Err(ProvenanceStoreError::InvalidChangeLayout {
            ordinal,
            field: block_field,
        }),
        (Some(_), None) => Err(ProvenanceStoreError::InvalidChangeLayout {
            ordinal,
            field: shape_field,
        }),
    }
}

fn required_source(ordinal: i64, source: Option<i64>) -> Result<SourceId, ProvenanceStoreError> {
    let source = source.ok_or(ProvenanceStoreError::InvalidChangeLayout {
        ordinal,
        field: "source_event_id",
    })?;
    source_id_from_i64("source_event_id", source)
}

fn require_event_source(expected: SourceId, found: SourceId) -> Result<(), ProvenanceStoreError> {
    if expected == found {
        Ok(())
    } else {
        Err(ProvenanceStoreError::EventSourceMismatch { expected, found })
    }
}

fn require_absent_before_location(
    ordinal: i64,
    change: &ProvenanceChangeInput,
) -> Result<(), ProvenanceStoreError> {
    require_none(ordinal, "before_block_id", &change.before_block_id)?;
    require_none(ordinal, "before_path", &change.before_path)?;
    require_none(ordinal, "before_from_utf16", &change.before_from_utf16)?;
    require_none(ordinal, "before_to_utf16", &change.before_to_utf16)
}

fn require_absent_after_location(
    ordinal: i64,
    change: &ProvenanceChangeInput,
) -> Result<(), ProvenanceStoreError> {
    require_none(ordinal, "after_block_id", &change.after_block_id)?;
    require_none(ordinal, "after_path", &change.after_path)?;
    require_none(ordinal, "after_from_utf16", &change.after_from_utf16)?;
    require_none(ordinal, "after_to_utf16", &change.after_to_utf16)
}

fn require_none<T>(
    ordinal: i64,
    field: &'static str,
    value: &Option<T>,
) -> Result<(), ProvenanceStoreError> {
    if value.is_none() {
        Ok(())
    } else {
        Err(ProvenanceStoreError::InvalidChangeLayout { ordinal, field })
    }
}

fn require_empty(
    ordinal: i64,
    field: &'static str,
    value: &str,
) -> Result<(), ProvenanceStoreError> {
    if value.is_empty() {
        Ok(())
    } else {
        Err(ProvenanceStoreError::InvalidChangeLayout { ordinal, field })
    }
}

fn required_string(
    ordinal: i64,
    field: &'static str,
    value: &Option<String>,
) -> Result<String, ProvenanceStoreError> {
    value
        .clone()
        .ok_or(ProvenanceStoreError::InvalidChangeLayout { ordinal, field })
}

fn source_id_to_i64(id: SourceId) -> Result<i64, ProvenanceStoreError> {
    if id.0 == 0 {
        return Err(ProvenanceStoreError::SourceIdOutOfRange(id));
    }
    i64::try_from(id.0).map_err(|_| ProvenanceStoreError::SourceIdOutOfRange(id))
}

fn source_id_from_i64(field: &'static str, value: i64) -> Result<SourceId, ProvenanceStoreError> {
    if value <= 0 {
        return Err(ProvenanceStoreError::InvalidStoredId { field, value });
    }
    let value =
        u64::try_from(value).map_err(|_| ProvenanceStoreError::InvalidStoredId { field, value })?;
    Ok(SourceId(value))
}

fn offset_from_i64(field: &'static str, value: i64) -> Result<u32, ProvenanceStoreError> {
    u32::try_from(value).map_err(|_| ProvenanceStoreError::InvalidStoredOffset { field, value })
}

fn to_canonical_json<T: Serialize>(
    field: &'static str,
    value: &T,
) -> Result<String, ProvenanceStoreError> {
    serde_json::to_string(value).map_err(|error| ProvenanceStoreError::Json {
        field,
        message: error.to_string(),
    })
}

fn from_canonical_json<T>(field: &'static str, value: &str) -> Result<T, ProvenanceStoreError>
where
    T: DeserializeOwned + Serialize,
{
    let parsed = serde_json::from_str(value).map_err(|error| ProvenanceStoreError::Json {
        field,
        message: error.to_string(),
    })?;
    let canonical = to_canonical_json(field, &parsed)?;
    if canonical != value {
        return Err(ProvenanceStoreError::NonCanonicalJson { field });
    }
    Ok(parsed)
}

fn require_document(
    row: &'static str,
    expected: &str,
    found: &str,
) -> Result<(), ProvenanceStoreError> {
    if expected == found {
        Ok(())
    } else {
        Err(ProvenanceStoreError::DocumentMismatch {
            row,
            expected: expected.to_owned(),
            found: found.to_owned(),
        })
    }
}

fn parse_action(value: &str) -> Result<EventAction, ProvenanceStoreError> {
    match value {
        value if value == action_name(EventAction::Edit) => Ok(EventAction::Edit),
        value if value == action_name(EventAction::Trash) => Ok(EventAction::Trash),
        value if value == action_name(EventAction::Restore) => Ok(EventAction::Restore),
        value if value == action_name(EventAction::LegacySeed) => Ok(EventAction::LegacySeed),
        value if value == action_name(EventAction::Suggestion) => Ok(EventAction::Suggestion),
        value if value == action_name(EventAction::Accept) => Ok(EventAction::Accept),
        value if value == action_name(EventAction::Reject) => Ok(EventAction::Reject),
        value => Err(ProvenanceStoreError::UnknownPersistedValue {
            field: "action",
            value: value.to_owned(),
        }),
    }
}

fn parse_ingress(value: &str) -> Result<Ingress, ProvenanceStoreError> {
    match value {
        value if value == ingress_name(Ingress::Entered) => Ok(Ingress::Entered),
        value if value == ingress_name(Ingress::Command) => Ok(Ingress::Command),
        value if value == ingress_name(Ingress::Pasted) => Ok(Ingress::Pasted),
        value if value == ingress_name(Ingress::Imported) => Ok(Ingress::Imported),
        value if value == ingress_name(Ingress::Mcp) => Ok(Ingress::Mcp),
        value if value == ingress_name(Ingress::Api) => Ok(Ingress::Api),
        value if value == ingress_name(Ingress::Suggestion) => Ok(Ingress::Suggestion),
        value if value == ingress_name(Ingress::Unknown) => Ok(Ingress::Unknown),
        value if value == ingress_name(Ingress::LegacyUnknown) => Ok(Ingress::LegacyUnknown),
        value => Err(ProvenanceStoreError::UnknownPersistedValue {
            field: "ingress",
            value: value.to_owned(),
        }),
    }
}

fn parse_assurance(value: &str) -> Result<Assurance, ProvenanceStoreError> {
    match value {
        value if value == assurance_name(Assurance::Observed) => Ok(Assurance::Observed),
        value if value == assurance_name(Assurance::Reported) => Ok(Assurance::Reported),
        value if value == assurance_name(Assurance::Verified) => Ok(Assurance::Verified),
        value if value == assurance_name(Assurance::Unknown) => Ok(Assurance::Unknown),
        value => Err(ProvenanceStoreError::UnknownPersistedValue {
            field: "assurance",
            value: value.to_owned(),
        }),
    }
}

fn source_label(
    row: &ProvenanceEventRow,
    event_id: SourceId,
    ingress: Ingress,
    assurance: Assurance,
) -> Result<String, ProvenanceStoreError> {
    match (ingress, assurance) {
        (Ingress::Entered, Assurance::Observed)
        | (Ingress::Pasted, Assurance::Observed)
        | (Ingress::Imported, Assurance::Observed)
        | (Ingress::Command, Assurance::Observed)
        | (Ingress::Unknown, Assurance::Unknown)
        | (Ingress::LegacyUnknown, Assurance::Unknown)
        | (Ingress::Mcp | Ingress::Suggestion, Assurance::Reported)
        | (Ingress::Api | Ingress::Suggestion, Assurance::Verified) => {}
        _ => {
            return Err(ProvenanceStoreError::UnsupportedClassification { ingress, assurance });
        }
    }
    nonempty(&row.source_label)
        .map(str::to_string)
        .ok_or(ProvenanceStoreError::MissingSourceLabel { event_id })
}

fn nonempty(value: &str) -> Option<&str> {
    (!value.trim().is_empty()).then_some(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MutationContext;
    use thought_provenance::{
        BlockLocation, BlockShape, BlockSnapshot, LineageState, TextLeafSnapshot,
    };

    fn location(block_id: &str, path: &[u32], from: u32, to: u32) -> TextLocation {
        TextLocation {
            block_id: block_id.into(),
            path: path.to_vec(),
            from_utf16: from,
            to_utf16: to,
        }
    }

    fn deltas() -> Vec<DeltaSegment> {
        vec![
            DeltaSegment::Insert {
                event_source_id: SourceId(7),
                after: location("a", &[0, 2], 4, 6),
                text: "é".into(),
            },
            DeltaSegment::Delete {
                event_source_id: SourceId(7),
                content_source_id: SourceId(2),
                before: location("a", &[1], 6, 8),
                text: "🙂".into(),
            },
            DeltaSegment::Format {
                event_source_id: SourceId(7),
                content_source_id: SourceId(3),
                before: location("b", &[0], 0, 4),
                after: location("b", &[0], 0, 4),
                text: "text".into(),
                before_format: "plain".into(),
                after_format: "bold".into(),
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
                        index: 2,
                    },
                    kind: "heading".into(),
                    structure_key: "after".into(),
                }),
            },
        ]
    }

    fn rows(event_id: i64, changes: Vec<ProvenanceChangeInput>) -> Vec<ProvenanceChangeRow> {
        changes
            .into_iter()
            .enumerate()
            .map(|(ordinal, change)| ProvenanceChangeRow {
                event_id,
                ordinal: i64::try_from(ordinal).unwrap(),
                change,
            })
            .collect()
    }

    fn event(id: i64, ingress: &str, assurance: &str, actor_label: &str) -> ProvenanceEventRow {
        let source_label = match (ingress, assurance) {
            ("entered", "observed") => "Written here".into(),
            ("pasted", "observed") => "Pasted".into(),
            ("imported", "observed") => "Imported".into(),
            ("command", "observed") => "Edited here".into(),
            ("unknown", "unknown") => "Unclassified change".into(),
            ("legacy_unknown", "unknown") => "Legacy content".into(),
            (_, "reported") => format!("{actor_label} (reported)"),
            (_, "verified") => format!("{actor_label} (verified)"),
            _ => actor_label.into(),
        };
        ProvenanceEventRow {
            event_id: id,
            doc_id: "doc-1".into(),
            update_seq: Some(id),
            actor_id: Some(format!("actor-{id}")),
            action: "edit".into(),
            ingress: ingress.into(),
            assurance: assurance.into(),
            connection_id: None,
            session_id: None,
            actor_label: actor_label.into(),
            source_label,
            provider: None,
            requested_model: None,
            reported_model: None,
            evidence_ref: None,
            suggestion_id: None,
            client_event_id: None,
            chain_version: 1,
            before_hash: vec![0; 32],
            after_hash: vec![1; 32],
            update_log_root: vec![2; 32],
            previous_event_hash: None,
            event_hash: vec![3; 32],
            created_at: 10,
            recorded_at: 11,
        }
    }

    #[test]
    fn semantic_deltas_round_trip_in_order_with_canonical_json() {
        let original = deltas();
        let stored = deltas_to_store(SourceId(7), &original).unwrap();
        assert_eq!(stored[0].after_path.as_deref(), Some("[0,2]"));
        assert_eq!(
            stored[3].before_shape.as_deref(),
            Some(
                r#"{"location":{"block_id":"b","index":1},"kind":"paragraph","structure_key":"before"}"#
            )
        );
        assert_eq!(stored, deltas_to_store(SourceId(7), &original).unwrap());
        assert_eq!(
            deltas_from_store(SourceId(7), &rows(7, stored)).unwrap(),
            original
        );
    }

    #[test]
    fn delta_conversion_rejects_wrong_or_unrepresentable_event_ids() {
        let mut wrong = deltas();
        let DeltaSegment::Insert {
            event_source_id, ..
        } = &mut wrong[0]
        else {
            unreachable!()
        };
        *event_source_id = SourceId(8);
        assert_eq!(
            deltas_to_store(SourceId(7), &wrong),
            Err(ProvenanceStoreError::EventSourceMismatch {
                expected: SourceId(7),
                found: SourceId(8),
            })
        );

        assert_eq!(
            deltas_to_store(SourceId(i64::MAX as u64 + 1), &[]),
            Err(ProvenanceStoreError::SourceIdOutOfRange(SourceId(
                i64::MAX as u64 + 1
            )))
        );
        assert_eq!(
            deltas_to_store(SourceId(0), &[]),
            Err(ProvenanceStoreError::SourceIdOutOfRange(SourceId(0)))
        );
    }

    #[test]
    fn persisted_changes_reject_bad_order_layout_paths_shapes_and_ranges() {
        let stored = deltas_to_store(SourceId(7), &deltas()).unwrap();

        let mut bad = rows(7, stored.clone());
        bad[1].ordinal = 4;
        assert!(matches!(
            deltas_from_store(SourceId(7), &bad),
            Err(ProvenanceStoreError::InvalidOrdinal { .. })
        ));

        let mut bad = rows(7, stored.clone());
        bad[0].change.op = "rewrite".into();
        assert_eq!(
            deltas_from_store(SourceId(7), &bad),
            Err(ProvenanceStoreError::UnknownPersistedValue {
                field: "op",
                value: "rewrite".into(),
            })
        );

        let mut bad = rows(7, stored.clone());
        bad[0].event_id = -1;
        assert_eq!(
            deltas_from_store(SourceId(7), &bad),
            Err(ProvenanceStoreError::InvalidStoredId {
                field: "event_id",
                value: -1,
            })
        );

        let mut bad = rows(7, stored.clone());
        bad[0].change.after_path = Some("[0, 2]".into());
        assert_eq!(
            deltas_from_store(SourceId(7), &bad),
            Err(ProvenanceStoreError::NonCanonicalJson {
                field: "after_path"
            })
        );

        let mut bad = rows(7, stored.clone());
        bad[0].change.after_path = Some("[-1]".into());
        assert!(matches!(
            deltas_from_store(SourceId(7), &bad),
            Err(ProvenanceStoreError::Json {
                field: "after_path",
                ..
            })
        ));

        let mut bad = rows(7, stored.clone());
        bad[0].change.after_path = Some("[4294967296]".into());
        assert!(matches!(
            deltas_from_store(SourceId(7), &bad),
            Err(ProvenanceStoreError::Json {
                field: "after_path",
                ..
            })
        ));

        let mut bad = rows(7, stored.clone());
        bad[3].change.before_shape = Some("{\"kind\":\"paragraph\"}".into());
        assert!(matches!(
            deltas_from_store(SourceId(7), &bad),
            Err(ProvenanceStoreError::Json {
                field: "before_shape",
                ..
            })
        ));

        let mut bad = rows(7, stored.clone());
        bad[3].change.before_block_id = Some("another-block".into());
        assert_eq!(
            deltas_from_store(SourceId(7), &bad),
            Err(ProvenanceStoreError::InvalidChangeLayout {
                ordinal: 3,
                field: "before_block_id",
            })
        );

        let mut bad = rows(7, stored.clone());
        bad[1].change.before_from_utf16 = Some(-1);
        assert_eq!(
            deltas_from_store(SourceId(7), &bad),
            Err(ProvenanceStoreError::InvalidStoredOffset {
                field: "before_from_utf16",
                value: -1,
            })
        );

        let mut bad = rows(7, stored.clone());
        bad[1].change.before_to_utf16 = Some(i64::from(u32::MAX) + 1);
        assert_eq!(
            deltas_from_store(SourceId(7), &bad),
            Err(ProvenanceStoreError::InvalidStoredOffset {
                field: "before_to_utf16",
                value: i64::from(u32::MAX) + 1,
            })
        );

        let mut bad = rows(7, stored);
        bad[1].change.source_event_id = Some(-1);
        assert_eq!(
            deltas_from_store(SourceId(7), &bad),
            Err(ProvenanceStoreError::InvalidStoredId {
                field: "source_event_id",
                value: -1,
            })
        );
    }

    #[test]
    fn lineage_spans_round_trip_into_a_valid_lineage_state() {
        let original = vec![
            LiveLineageSpan {
                location: location("a", &[0], 0, 2),
                source_id: SourceId(1),
            },
            LiveLineageSpan {
                location: location("a", &[0], 2, 4),
                source_id: SourceId(2),
            },
        ];
        let stored = spans_to_store(&original).unwrap();
        assert_eq!(stored[0].node_path, "[0]");
        let span_rows = stored
            .into_iter()
            .map(|span| LineageSpanRow {
                doc_id: "doc-1".into(),
                span,
            })
            .collect::<Vec<_>>();
        let events = vec![
            event(1, "entered", "observed", "ignored"),
            event(2, "mcp", "reported", "Claude"),
        ];
        let parts = lineage_from_store("doc-1", &span_rows, &events).unwrap();
        assert_eq!(parts.spans, original);
        assert_eq!(parts.sources[&SourceId(1)].label, "Written here");
        assert_eq!(parts.sources[&SourceId(2)].label, "Claude (reported)");

        let blocks = vec![BlockSnapshot::new(
            "a",
            "paragraph",
            "",
            vec![TextLeafSnapshot::new(vec![0], "text", "")],
        )];
        LineageState::from_parts(blocks, parts.spans, parts.sources).unwrap();
    }

    #[test]
    fn source_labels_match_every_mutation_constructor_class() {
        let mut events = vec![
            event(1, "entered", "observed", "writer"),
            event(2, "pasted", "observed", "writer"),
            event(3, "imported", "observed", "writer"),
            event(4, "command", "observed", "writer"),
            event(5, "unknown", "unknown", "writer"),
            event(6, "legacy_unknown", "unknown", "writer"),
            event(7, "mcp", "reported", "Claude"),
            event(8, "api", "verified", "OpenAI"),
            event(9, "suggestion", "reported", "ChatGPT"),
            event(10, "suggestion", "verified", "Claude"),
        ];
        events[5].action = "legacy_seed".into();
        events[7].actor_label.clear();
        events[7].provider = Some("OpenAI".into());
        events[8].action = "accept".into();
        events[9].action = "suggestion".into();
        let parts = lineage_from_store("doc-1", &[], &events).unwrap();
        let labels = parts
            .sources
            .values()
            .map(|source| source.label.clone())
            .collect::<Vec<_>>();
        let expected = vec![
            MutationContext::entered().source(SourceId(1)).label,
            MutationContext::pasted().source(SourceId(2)).label,
            MutationContext::imported().source(SourceId(3)).label,
            MutationContext::command().source(SourceId(4)).label,
            MutationContext::unknown().source(SourceId(5)).label,
            MutationContext::legacy_seed().source(SourceId(6)).label,
            MutationContext::mcp_reported("Claude", None, None, None)
                .source(SourceId(7))
                .label,
            MutationContext::api_verified("OpenAI", "OpenAI", None, None, "evidence")
                .source(SourceId(8))
                .label,
            "ChatGPT (reported)".into(),
            "Claude (verified)".into(),
        ];
        assert_eq!(labels, expected);
    }

    #[test]
    fn hydration_preserves_frozen_labels_and_groups_mcp_sources_by_connection() {
        let mut first = event(1, "mcp", "reported", "Transport actor");
        first.source_label = "Claude reviewer (reported)".into();
        first.connection_id = Some("reviewer-1".into());

        let mut renamed = event(2, "mcp", "reported", "Different transport actor");
        renamed.source_label = "Research reviewer (reported)".into();
        renamed.connection_id = Some("reviewer-1".into());

        let mut other = event(3, "mcp", "reported", "Transport actor");
        other.source_label = "Claude reviewer (reported)".into();
        other.connection_id = Some("reviewer-2".into());

        let parts = lineage_from_store("doc-1", &[], &[first, renamed, other]).unwrap();
        assert_eq!(
            parts.sources[&SourceId(1)].label,
            "Claude reviewer (reported)"
        );
        assert_eq!(
            parts.sources[&SourceId(2)].label,
            "Research reviewer (reported)"
        );
        assert_eq!(
            parts.sources[&SourceId(1)].group_key,
            "mcp:connection:reviewer-1"
        );
        assert_eq!(
            parts.sources[&SourceId(1)].group_key,
            parts.sources[&SourceId(2)].group_key
        );
        assert_ne!(
            parts.sources[&SourceId(1)].group_key,
            parts.sources[&SourceId(3)].group_key
        );
    }

    #[test]
    fn hydration_rejects_unknown_values_and_unsupported_claims() {
        for (field, value) in [
            ("action", "mystery"),
            ("ingress", "telepathy"),
            ("assurance", "certain"),
        ] {
            let mut row = event(1, "entered", "observed", "writer");
            match field {
                "action" => row.action = value.into(),
                "ingress" => row.ingress = value.into(),
                _ => row.assurance = value.into(),
            }
            assert_eq!(
                lineage_from_store("doc-1", &[], &[row]),
                Err(ProvenanceStoreError::UnknownPersistedValue {
                    field,
                    value: value.into(),
                })
            );
        }

        let row = event(1, "mcp", "verified", "Claude");
        assert_eq!(
            lineage_from_store("doc-1", &[], &[row]),
            Err(ProvenanceStoreError::UnsupportedClassification {
                ingress: Ingress::Mcp,
                assurance: Assurance::Verified,
            })
        );

        let mut row = event(1, "entered", "observed", "writer");
        row.source_label.clear();
        assert_eq!(
            lineage_from_store("doc-1", &[], &[row]),
            Err(ProvenanceStoreError::MissingSourceLabel {
                event_id: SourceId(1),
            })
        );

        let duplicate = event(1, "entered", "observed", "writer");
        assert_eq!(
            lineage_from_store("doc-1", &[], &[duplicate.clone(), duplicate]),
            Err(ProvenanceStoreError::DuplicateEventId(SourceId(1)))
        );

        let negative = event(-1, "entered", "observed", "writer");
        assert_eq!(
            lineage_from_store("doc-1", &[], &[negative]),
            Err(ProvenanceStoreError::InvalidStoredId {
                field: "event_id",
                value: -1,
            })
        );
    }

    #[test]
    fn hydration_rejects_bad_span_rows_and_cross_document_sources() {
        let source = event(1, "entered", "observed", "writer");
        let base = LineageSpanRow {
            doc_id: "doc-1".into(),
            span: LineageSpanInput {
                block_id: "a".into(),
                node_path: "[0]".into(),
                start_utf16: 0,
                end_utf16: 1,
                source_event_id: 1,
            },
        };

        let mut bad = base.clone();
        bad.span.source_event_id = 2;
        assert_eq!(
            lineage_from_store("doc-1", &[bad], std::slice::from_ref(&source)),
            Err(ProvenanceStoreError::MissingSourceEvent {
                document_id: "doc-1".into(),
                source_event_id: SourceId(2),
            })
        );

        let mut bad = base.clone();
        bad.span.node_path = "[0, 1]".into();
        assert_eq!(
            lineage_from_store("doc-1", &[bad], std::slice::from_ref(&source)),
            Err(ProvenanceStoreError::NonCanonicalJson { field: "node_path" })
        );

        for value in [-1, i64::from(u32::MAX) + 1] {
            let mut bad = base.clone();
            bad.span.end_utf16 = value;
            assert_eq!(
                lineage_from_store("doc-1", &[bad], std::slice::from_ref(&source)),
                Err(ProvenanceStoreError::InvalidStoredOffset {
                    field: "end_utf16",
                    value,
                })
            );
        }

        let mut bad = base.clone();
        bad.span.start_utf16 = 1;
        assert!(matches!(
            lineage_from_store("doc-1", &[bad], std::slice::from_ref(&source)),
            Err(ProvenanceStoreError::InvalidTextRange { .. })
        ));

        let mut other_event = source.clone();
        other_event.doc_id = "doc-2".into();
        assert!(matches!(
            lineage_from_store("doc-1", std::slice::from_ref(&base), &[other_event]),
            Err(ProvenanceStoreError::DocumentMismatch {
                row: "provenance event",
                ..
            })
        ));

        let mut other_span = base;
        other_span.doc_id = "doc-2".into();
        assert!(matches!(
            lineage_from_store("doc-1", &[other_span], &[source]),
            Err(ProvenanceStoreError::DocumentMismatch {
                row: "lineage span",
                ..
            })
        ));
    }
}
