//! Semantic text lineage for Proof of Thought documents.
//!
//! Yjs updates describe how replicas converge, not the visible edit a writer
//! made. In particular, a whole-block rewrite may delete and reinsert an
//! unchanged subtree. This crate compares the visible before and after trees,
//! preserves the source of equal graphemes, and records only the semantic
//! insertions, deletions, formatting changes, and structural changes.

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

/// The persisted event which first introduced a live piece of text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceId(pub u64);

/// How a current change entered Proof of Thought.
///
/// This is observed input provenance, not an authorship guess. In particular,
/// editor commands are distinct from direct text entry, and a current update
/// whose source was missed is distinct from history created before this
/// vocabulary existed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ingress {
    /// Direct text entry observed by the editor, including keyboard and IME.
    Entered,
    /// An observed toolbar, undo, cut, or structural editor command.
    Command,
    /// Clipboard insertion observed by the editor.
    Pasted,
    /// Content introduced through an import flow.
    Imported,
    /// A change delivered through an external MCP connection.
    Mcp,
    /// A change produced through Proof of Thought's provider API flow.
    Api,
    /// Content introduced by accepting or editing a suggestion.
    Suggestion,
    /// A current update whose input source was not observed.
    Unknown,
    /// Content from history created before detailed provenance tracking.
    LegacyUnknown,
}

/// How strongly Proof of Thought can support the asserted source identity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Assurance {
    Observed,
    Reported,
    Verified,
    Unknown,
}

/// Consumer-facing metadata for one provenance event/source.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceDescriptor {
    pub id: SourceId,
    /// Stable identity used only for consumer grouping. Event identity remains
    /// `id`, so grouping never erases forensic contributions or span sources.
    pub group_key: String,
    pub label: String,
    pub ingress: Ingress,
    pub assurance: Assurance,
}

impl SourceDescriptor {
    pub fn new(
        id: SourceId,
        group_key: impl Into<String>,
        label: impl Into<String>,
        ingress: Ingress,
        assurance: Assurance,
    ) -> Self {
        Self {
            id,
            group_key: group_key.into(),
            label: label.into(),
            ingress,
            assurance,
        }
    }
}

/// One formatted text leaf in a ProseMirror block.
///
/// `path` is the child-index path from the top-level block to the leaf.
/// `format_key` is a caller-supplied canonical representation of the marks on
/// the leaf. It is deliberately opaque to keep this crate schema-independent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextLeafSnapshot {
    pub path: Vec<u32>,
    pub text: String,
    pub format_key: String,
}

impl TextLeafSnapshot {
    pub fn new(
        path: impl Into<Vec<u32>>,
        text: impl Into<String>,
        format_key: impl Into<String>,
    ) -> Self {
        Self {
            path: path.into(),
            text: text.into(),
            format_key: format_key.into(),
        }
    }
}

/// The visible and structural state of one top-level block.
///
/// `structure_key` is a caller-supplied canonical representation of structure
/// and non-formatting attributes below the block. Visible text lives in
/// `leaves`; changing either structural key does not reattribute that text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockSnapshot {
    pub block_id: String,
    pub kind: String,
    pub structure_key: String,
    pub leaves: Vec<TextLeafSnapshot>,
}

impl BlockSnapshot {
    pub fn new(
        block_id: impl Into<String>,
        kind: impl Into<String>,
        structure_key: impl Into<String>,
        leaves: Vec<TextLeafSnapshot>,
    ) -> Self {
        Self {
            block_id: block_id.into(),
            kind: kind.into(),
            structure_key: structure_key.into(),
            leaves,
        }
    }

    /// Convenience for the common case of one plain-text leaf.
    pub fn plain(
        block_id: impl Into<String>,
        kind: impl Into<String>,
        text: impl Into<String>,
    ) -> Self {
        Self::new(
            block_id,
            kind,
            "",
            vec![TextLeafSnapshot::new(vec![0], text, "")],
        )
    }
}

/// A current text range. Offsets are UTF-16 code units, matching Yjs and
/// ProseMirror positions in the webview.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TextLocation {
    pub block_id: String,
    pub path: Vec<u32>,
    pub from_utf16: u32,
    pub to_utf16: u32,
}

/// A block's position in one document snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockLocation {
    pub block_id: String,
    pub index: usize,
}

/// The structural value used by a structure delta.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockShape {
    pub location: BlockLocation,
    pub kind: String,
    pub structure_key: String,
}

/// An immutable semantic change made by `event_source_id`.
///
/// Inserted text is sourced to the event itself. Deleted and formatted text
/// retain `content_source_id`, allowing history to say whose live text was
/// affected without changing the surviving text's origin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "snake_case")]
pub enum DeltaSegment {
    Insert {
        event_source_id: SourceId,
        after: TextLocation,
        text: String,
    },
    Delete {
        event_source_id: SourceId,
        content_source_id: SourceId,
        before: TextLocation,
        text: String,
    },
    Format {
        event_source_id: SourceId,
        content_source_id: SourceId,
        before: TextLocation,
        after: TextLocation,
        text: String,
        before_format: String,
        after_format: String,
    },
    Structure {
        event_source_id: SourceId,
        before: Option<BlockShape>,
        after: Option<BlockShape>,
    },
}

/// One current, contiguous range introduced by the same source event.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveLineageSpan {
    pub location: TextLocation,
    pub source_id: SourceId,
}

/// The current visible tree and its rebuildable text-lineage read model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineageState {
    blocks: Vec<BlockSnapshot>,
    spans: Vec<LiveLineageSpan>,
    sources: BTreeMap<SourceId, SourceDescriptor>,
}

impl LineageState {
    /// Seed a new or legacy snapshot from one source.
    pub fn seed(
        blocks: Vec<BlockSnapshot>,
        source: SourceDescriptor,
    ) -> Result<Self, ReconcileError> {
        validate_blocks(&blocks)?;
        let flat = FlatDocument::new(&blocks);
        let sources = vec![source.id; flat.tokens.len()];
        let spans = compress_spans(&flat.tokens, &sources);
        let descriptors = BTreeMap::from([(source.id, source)]);
        Ok(Self {
            blocks,
            spans,
            sources: descriptors,
        })
    }

    /// Rehydrate a state from persisted snapshots and live spans.
    pub fn from_parts(
        blocks: Vec<BlockSnapshot>,
        spans: Vec<LiveLineageSpan>,
        sources: BTreeMap<SourceId, SourceDescriptor>,
    ) -> Result<Self, ReconcileError> {
        validate_blocks(&blocks)?;
        let state = Self {
            blocks,
            spans,
            sources,
        };
        state.token_sources()?;
        Ok(state)
    }

    pub fn blocks(&self) -> &[BlockSnapshot] {
        &self.blocks
    }

    pub fn spans(&self) -> &[LiveLineageSpan] {
        &self.spans
    }

    pub fn sources(&self) -> &BTreeMap<SourceId, SourceDescriptor> {
        &self.sources
    }

    /// Reconcile a new snapshot and attribute only newly inserted visible text
    /// to `event_source`.
    pub fn reconcile(
        &self,
        after: Vec<BlockSnapshot>,
        event_source: SourceDescriptor,
    ) -> Result<Reconciliation, ReconcileError> {
        reconcile(self, after, event_source)
    }

    pub fn current_source_summary(&self) -> Result<CurrentSourceSummary, ReconcileError> {
        let flat = FlatDocument::new(&self.blocks);
        let token_sources = self.token_sources()?;
        let mut counts: BTreeMap<SourceId, (usize, usize)> = BTreeMap::new();
        for (token, source_id) in flat.tokens.iter().zip(token_sources) {
            let entry = counts.entry(source_id).or_default();
            entry.0 += 1;
            if !token.text.chars().all(char::is_whitespace) {
                entry.1 += 1;
            }
        }

        let mut contributions = counts
            .into_iter()
            .map(|(source_id, (graphemes, non_whitespace_graphemes))| {
                let source = self
                    .sources
                    .get(&source_id)
                    .expect("token_sources validates descriptors")
                    .clone();
                SourceContribution {
                    source,
                    graphemes,
                    non_whitespace_graphemes,
                }
            })
            .collect::<Vec<_>>();
        let grouped_contributions = grouped_contributions(&contributions);
        contributions.sort_by(|a, b| {
            b.non_whitespace_graphemes
                .cmp(&a.non_whitespace_graphemes)
                .then_with(|| a.source.id.cmp(&b.source.id))
        });

        Ok(CurrentSourceSummary {
            total_graphemes: flat.tokens.len(),
            total_non_whitespace_graphemes: flat
                .tokens
                .iter()
                .filter(|token| !token.text.chars().all(char::is_whitespace))
                .count(),
            contributions,
            grouped_contributions,
        })
    }

    fn token_sources(&self) -> Result<Vec<SourceId>, ReconcileError> {
        let flat = FlatDocument::new(&self.blocks);
        let mut leaf_bounds: HashMap<LeafKey<'_>, (u32, HashSet<u32>)> = HashMap::new();
        for block in &self.blocks {
            for leaf in &block.leaves {
                let mut utf16 = 0_u32;
                let mut boundaries = HashSet::from([0]);
                for grapheme in leaf.text.graphemes(true) {
                    utf16 += grapheme.encode_utf16().count() as u32;
                    boundaries.insert(utf16);
                }
                leaf_bounds.insert(
                    LeafKey {
                        block_id: &block.block_id,
                        path: &leaf.path,
                    },
                    (utf16, boundaries),
                );
            }
        }
        let mut by_leaf: HashMap<LeafKey<'_>, Vec<&LiveLineageSpan>> = HashMap::new();
        for span in &self.spans {
            if span.location.from_utf16 >= span.location.to_utf16 {
                return Err(ReconcileError::InvalidSpan(span.location.clone()));
            }
            if !self.sources.contains_key(&span.source_id) {
                return Err(ReconcileError::UnknownSource(span.source_id));
            }
            let key = LeafKey::from_location(&span.location);
            let Some((leaf_end, boundaries)) = leaf_bounds.get(&key) else {
                return Err(ReconcileError::InvalidSpan(span.location.clone()));
            };
            if span.location.to_utf16 > *leaf_end
                || !boundaries.contains(&span.location.from_utf16)
                || !boundaries.contains(&span.location.to_utf16)
            {
                return Err(ReconcileError::InvalidSpan(span.location.clone()));
            }
            by_leaf.entry(key).or_default().push(span);
        }
        for spans in by_leaf.values_mut() {
            spans.sort_by_key(|span| span.location.from_utf16);
            for pair in spans.windows(2) {
                if pair[0].location.to_utf16 > pair[1].location.from_utf16 {
                    return Err(ReconcileError::OverlappingSpans(
                        pair[0].location.clone(),
                        pair[1].location.clone(),
                    ));
                }
            }
        }

        flat.tokens
            .iter()
            .map(|token| {
                let key = LeafKey::from_location(&token.location);
                let source_id = by_leaf
                    .get(&key)
                    .and_then(|spans| {
                        spans.iter().find(|span| {
                            span.location.from_utf16 <= token.location.from_utf16
                                && span.location.to_utf16 >= token.location.to_utf16
                        })
                    })
                    .map(|span| span.source_id)
                    .ok_or_else(|| ReconcileError::UncoveredText(token.location.clone()))?;
                Ok(source_id)
            })
            .collect()
    }
}

/// The new lineage state plus this event's immutable semantic deltas.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Reconciliation {
    pub state: LineageState,
    pub deltas: Vec<DeltaSegment>,
}

/// One source's contribution to the current visible text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceContribution {
    pub source: SourceDescriptor,
    pub graphemes: usize,
    pub non_whitespace_graphemes: usize,
}

/// Stable consumer identity shared by one or more forensic event sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceGroup {
    pub key: String,
    pub label: String,
    pub ingress: Ingress,
    pub assurance: Assurance,
}

/// Current visible wording combined across event sources in one stable group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupedSourceContribution {
    pub group: SourceGroup,
    /// Number of currently contributing forensic event sources in this group.
    pub event_count: usize,
    pub graphemes: usize,
    pub non_whitespace_graphemes: usize,
}

/// Counts for the versioned current-source alignment without storing a rounded
/// or ambiguous percentage in the provenance model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentSourceSummary {
    pub total_graphemes: usize,
    pub total_non_whitespace_graphemes: usize,
    /// Forensic event-level contributions. These remain separate even when
    /// several events share one consumer group.
    pub contributions: Vec<SourceContribution>,
    /// Consumer-facing totals combined by `SourceDescriptor::group_key`.
    pub grouped_contributions: Vec<GroupedSourceContribution>,
}

fn grouped_contributions(contributions: &[SourceContribution]) -> Vec<GroupedSourceContribution> {
    let mut grouped = BTreeMap::<String, (SourceId, GroupedSourceContribution)>::new();
    for contribution in contributions {
        let key = contribution.source.group_key.clone();
        let (latest_source_id, entry) = grouped.entry(key.clone()).or_insert_with(|| {
            (
                contribution.source.id,
                GroupedSourceContribution {
                    group: SourceGroup {
                        key,
                        label: contribution.source.label.clone(),
                        ingress: contribution.source.ingress,
                        assurance: contribution.source.assurance,
                    },
                    event_count: 0,
                    graphemes: 0,
                    non_whitespace_graphemes: 0,
                },
            )
        });
        debug_assert_eq!(entry.group.ingress, contribution.source.ingress);
        debug_assert_eq!(entry.group.assurance, contribution.source.assurance);
        if contribution.source.id > *latest_source_id {
            *latest_source_id = contribution.source.id;
            entry.group.label = contribution.source.label.clone();
            entry.group.ingress = contribution.source.ingress;
            entry.group.assurance = contribution.source.assurance;
        }
        entry.event_count += 1;
        entry.graphemes += contribution.graphemes;
        entry.non_whitespace_graphemes += contribution.non_whitespace_graphemes;
    }

    let mut grouped = grouped
        .into_values()
        .map(|(_, contribution)| contribution)
        .collect::<Vec<_>>();
    grouped.sort_by(|a, b| {
        b.non_whitespace_graphemes
            .cmp(&a.non_whitespace_graphemes)
            .then_with(|| a.group.key.cmp(&b.group.key))
    });
    grouped
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReconcileError {
    DuplicateBlockId(String),
    DuplicateLeafPath { block_id: String, path: Vec<u32> },
    SourceConflict(SourceId),
    UnknownSource(SourceId),
    InvalidSpan(TextLocation),
    OverlappingSpans(TextLocation, TextLocation),
    UncoveredText(TextLocation),
}

impl fmt::Display for ReconcileError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateBlockId(id) => write!(f, "duplicate block id `{id}`"),
            Self::DuplicateLeafPath { block_id, path } => {
                write!(f, "duplicate text-leaf path {path:?} in block `{block_id}`")
            }
            Self::SourceConflict(id) => write!(f, "source {id:?} has conflicting metadata"),
            Self::UnknownSource(id) => write!(f, "lineage refers to unknown source {id:?}"),
            Self::InvalidSpan(location) => write!(f, "invalid lineage span at {location:?}"),
            Self::OverlappingSpans(a, b) => {
                write!(f, "overlapping lineage spans at {a:?} and {b:?}")
            }
            Self::UncoveredText(location) => {
                write!(f, "visible grapheme has no lineage at {location:?}")
            }
        }
    }
}

impl std::error::Error for ReconcileError {}

/// Reconcile `after` against `before`, preserving the lineage of the longest
/// deterministic sequence of equal visible graphemes.
pub fn reconcile(
    before: &LineageState,
    after: Vec<BlockSnapshot>,
    event_source: SourceDescriptor,
) -> Result<Reconciliation, ReconcileError> {
    validate_blocks(&after)?;
    let old_flat = FlatDocument::new(&before.blocks);
    let new_flat = FlatDocument::new(&after);
    let old_sources = before.token_sources()?;

    let mut descriptors = before.sources.clone();
    if let Some(existing) = descriptors.get(&event_source.id) {
        if existing != &event_source {
            return Err(ReconcileError::SourceConflict(event_source.id));
        }
    } else {
        descriptors.insert(event_source.id, event_source.clone());
    }

    let anchors = unchanged_block_anchors(&before.blocks, &old_flat, &after, &new_flat);
    let mut matches = Vec::new();
    let mut old_block_cursor = 0;
    let mut new_block_cursor = 0;

    for &(old_block, new_block) in &anchors {
        let old_region = token_range(&old_flat.block_ranges, old_block_cursor..old_block);
        let new_region = token_range(&new_flat.block_ranges, new_block_cursor..new_block);
        matches.extend(match_region(
            &old_flat.tokens,
            old_region,
            &new_flat.tokens,
            new_region,
        ));

        let old_anchor = old_flat.block_ranges[old_block].clone();
        let new_anchor = new_flat.block_ranges[new_block].clone();
        debug_assert_eq!(old_anchor.len(), new_anchor.len());
        matches.extend(old_anchor.zip(new_anchor));

        old_block_cursor = old_block + 1;
        new_block_cursor = new_block + 1;
    }
    matches.extend(match_region(
        &old_flat.tokens,
        token_range(
            &old_flat.block_ranges,
            old_block_cursor..before.blocks.len(),
        ),
        &new_flat.tokens,
        token_range(&new_flat.block_ranges, new_block_cursor..after.len()),
    ));
    matches.sort_unstable();

    let mut new_sources = vec![event_source.id; new_flat.tokens.len()];
    for &(old_index, new_index) in &matches {
        new_sources[new_index] = old_sources[old_index];
    }

    let mut deltas = structure_deltas(&before.blocks, &after, event_source.id);
    append_text_deltas(
        &old_flat.tokens,
        &old_sources,
        &new_flat.tokens,
        event_source.id,
        &matches,
        &mut deltas,
    );
    append_format_deltas(
        &old_flat.tokens,
        &old_sources,
        &new_flat.tokens,
        event_source.id,
        &matches,
        &mut deltas,
    );

    Ok(Reconciliation {
        state: LineageState {
            blocks: after,
            spans: compress_spans(&new_flat.tokens, &new_sources),
            sources: descriptors,
        },
        deltas,
    })
}

#[derive(Debug, Clone)]
struct FlatToken {
    text: String,
    format_key: String,
    location: TextLocation,
}

#[derive(Debug)]
struct FlatDocument {
    tokens: Vec<FlatToken>,
    block_ranges: Vec<Range<usize>>,
}

impl FlatDocument {
    fn new(blocks: &[BlockSnapshot]) -> Self {
        let mut tokens = Vec::new();
        let mut block_ranges = Vec::with_capacity(blocks.len());
        for block in blocks {
            let start = tokens.len();
            for leaf in &block.leaves {
                let mut utf16 = 0_u32;
                for grapheme in leaf.text.graphemes(true) {
                    let width = grapheme.encode_utf16().count() as u32;
                    tokens.push(FlatToken {
                        text: grapheme.to_string(),
                        format_key: leaf.format_key.clone(),
                        location: TextLocation {
                            block_id: block.block_id.clone(),
                            path: leaf.path.clone(),
                            from_utf16: utf16,
                            to_utf16: utf16 + width,
                        },
                    });
                    utf16 += width;
                }
            }
            block_ranges.push(start..tokens.len());
        }
        Self {
            tokens,
            block_ranges,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct LeafKey<'a> {
    block_id: &'a str,
    path: &'a [u32],
}

impl<'a> LeafKey<'a> {
    fn from_location(location: &'a TextLocation) -> Self {
        Self {
            block_id: &location.block_id,
            path: &location.path,
        }
    }
}

fn validate_blocks(blocks: &[BlockSnapshot]) -> Result<(), ReconcileError> {
    let mut ids = HashSet::new();
    for block in blocks {
        if !ids.insert(&block.block_id) {
            return Err(ReconcileError::DuplicateBlockId(block.block_id.clone()));
        }
        let mut paths = HashSet::new();
        for leaf in &block.leaves {
            if !paths.insert(&leaf.path) {
                return Err(ReconcileError::DuplicateLeafPath {
                    block_id: block.block_id.clone(),
                    path: leaf.path.clone(),
                });
            }
        }
    }
    Ok(())
}

fn token_range(ranges: &[Range<usize>], blocks: Range<usize>) -> Range<usize> {
    if blocks.is_empty() {
        let at = ranges
            .get(blocks.start)
            .map(|range| range.start)
            .or_else(|| ranges.last().map(|range| range.end))
            .unwrap_or(0);
        return at..at;
    }
    ranges[blocks.start].start..ranges[blocks.end - 1].end
}

/// Anchors are unchanged-text blocks whose ids occur in the same relative
/// order. Modified blocks are deliberately not anchors so a paragraph split or
/// merge can reconcile text across the old block boundary.
fn unchanged_block_anchors(
    old_blocks: &[BlockSnapshot],
    old: &FlatDocument,
    new_blocks: &[BlockSnapshot],
    new: &FlatDocument,
) -> Vec<(usize, usize)> {
    let new_by_id: HashMap<&str, usize> = new_blocks
        .iter()
        .enumerate()
        .map(|(index, block)| (block.block_id.as_str(), index))
        .collect();
    let candidates = old_blocks
        .iter()
        .enumerate()
        .filter_map(|(old_index, block)| {
            let &new_index = new_by_id.get(block.block_id.as_str())?;
            let old_range = old.block_ranges[old_index].clone();
            let new_range = new.block_ranges[new_index].clone();
            let same = old.tokens[old_range]
                .iter()
                .map(|token| token.text.as_str())
                .eq(new.tokens[new_range]
                    .iter()
                    .map(|token| token.text.as_str()));
            same.then_some((old_index, new_index))
        })
        .collect::<Vec<_>>();

    // Longest increasing subsequence of new indices. Block ids are unique, so
    // a strict LIS is enough and gives deterministic anchors after reorders.
    let mut tails: Vec<usize> = Vec::new();
    let mut previous: Vec<Option<usize>> = vec![None; candidates.len()];
    for (candidate_index, &(_, new_index)) in candidates.iter().enumerate() {
        let slot = tails.partition_point(|&tail| candidates[tail].1 < new_index);
        if slot > 0 {
            previous[candidate_index] = Some(tails[slot - 1]);
        }
        if slot == tails.len() {
            tails.push(candidate_index);
        } else {
            tails[slot] = candidate_index;
        }
    }
    let Some(&last) = tails.last() else {
        return vec![];
    };
    let mut indices = Vec::with_capacity(tails.len());
    let mut cursor = Some(last);
    while let Some(index) = cursor {
        indices.push(index);
        cursor = previous[index];
    }
    indices.reverse();
    indices.into_iter().map(|index| candidates[index]).collect()
}

fn match_region(
    old: &[FlatToken],
    old_range: Range<usize>,
    new: &[FlatToken],
    new_range: Range<usize>,
) -> Vec<(usize, usize)> {
    let old_slice = &old[old_range.clone()];
    let new_slice = &new[new_range.clone()];
    let mut prefix = 0;
    while prefix < old_slice.len()
        && prefix < new_slice.len()
        && old_slice[prefix].text == new_slice[prefix].text
    {
        prefix += 1;
    }

    let mut suffix = 0;
    while suffix < old_slice.len() - prefix
        && suffix < new_slice.len() - prefix
        && old_slice[old_slice.len() - 1 - suffix].text
            == new_slice[new_slice.len() - 1 - suffix].text
    {
        suffix += 1;
    }

    let mut matches = (0..prefix)
        .map(|offset| (old_range.start + offset, new_range.start + offset))
        .collect::<Vec<_>>();
    let old_middle = &old_slice[prefix..old_slice.len() - suffix];
    let new_middle = &new_slice[prefix..new_slice.len() - suffix];
    let mut middle = Vec::new();
    hirschberg(
        old_middle,
        new_middle,
        old_range.start + prefix,
        new_range.start + prefix,
        &mut middle,
    );
    matches.extend(middle);
    matches.extend((0..suffix).map(|offset| {
        (
            old_range.end - suffix + offset,
            new_range.end - suffix + offset,
        )
    }));
    matches
}

/// Linear-memory LCS. Ties choose the split nearest the same relative
/// position, then the earlier new position, which makes duplicate text stable.
fn hirschberg(
    old: &[FlatToken],
    new: &[FlatToken],
    old_offset: usize,
    new_offset: usize,
    out: &mut Vec<(usize, usize)>,
) {
    if old.is_empty() || new.is_empty() {
        return;
    }
    if old.len() == 1 {
        let target = new.len() / 2;
        let matching = new
            .iter()
            .enumerate()
            .filter(|(_, token)| token.text == old[0].text)
            .min_by_key(|(index, _)| (index.abs_diff(target), *index));
        if let Some((index, _)) = matching {
            out.push((old_offset, new_offset + index));
        }
        return;
    }

    let old_mid = old.len() / 2;
    let left = lcs_lengths(&old[..old_mid], new);
    let old_right = old[old_mid..].iter().rev().collect::<Vec<_>>();
    let new_reverse = new.iter().rev().collect::<Vec<_>>();
    let right = lcs_lengths_refs(&old_right, &new_reverse);
    let target = old_mid * new.len() / old.len();
    let split = (0..=new.len())
        .max_by_key(|&index| {
            (
                left[index] + right[new.len() - index],
                std::cmp::Reverse(index.abs_diff(target)),
                std::cmp::Reverse(index),
            )
        })
        .unwrap_or(0);

    hirschberg(&old[..old_mid], &new[..split], old_offset, new_offset, out);
    hirschberg(
        &old[old_mid..],
        &new[split..],
        old_offset + old_mid,
        new_offset + split,
        out,
    );
}

fn lcs_lengths(old: &[FlatToken], new: &[FlatToken]) -> Vec<usize> {
    let old_refs = old.iter().collect::<Vec<_>>();
    let new_refs = new.iter().collect::<Vec<_>>();
    lcs_lengths_refs(&old_refs, &new_refs)
}

fn lcs_lengths_refs(old: &[&FlatToken], new: &[&FlatToken]) -> Vec<usize> {
    let mut previous = vec![0; new.len() + 1];
    let mut current = vec![0; new.len() + 1];
    for old_token in old {
        current[0] = 0;
        for (index, new_token) in new.iter().enumerate() {
            current[index + 1] = if old_token.text == new_token.text {
                previous[index] + 1
            } else {
                current[index].max(previous[index + 1])
            };
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous
}

fn structure_deltas(
    old: &[BlockSnapshot],
    new: &[BlockSnapshot],
    event_source_id: SourceId,
) -> Vec<DeltaSegment> {
    let old_by_id: HashMap<&str, (usize, &BlockSnapshot)> = old
        .iter()
        .enumerate()
        .map(|(index, block)| (block.block_id.as_str(), (index, block)))
        .collect();
    let new_by_id: HashMap<&str, (usize, &BlockSnapshot)> = new
        .iter()
        .enumerate()
        .map(|(index, block)| (block.block_id.as_str(), (index, block)))
        .collect();
    let mut deltas = Vec::new();

    for (index, block) in old.iter().enumerate() {
        match new_by_id.get(block.block_id.as_str()) {
            Some(&(new_index, new_block))
                if index != new_index
                    || block.kind != new_block.kind
                    || block.structure_key != new_block.structure_key =>
            {
                deltas.push(DeltaSegment::Structure {
                    event_source_id,
                    before: Some(shape(index, block)),
                    after: Some(shape(new_index, new_block)),
                });
            }
            None => deltas.push(DeltaSegment::Structure {
                event_source_id,
                before: Some(shape(index, block)),
                after: None,
            }),
            _ => {}
        }
    }
    for (index, block) in new.iter().enumerate() {
        if !old_by_id.contains_key(block.block_id.as_str()) {
            deltas.push(DeltaSegment::Structure {
                event_source_id,
                before: None,
                after: Some(shape(index, block)),
            });
        }
    }
    deltas
}

fn shape(index: usize, block: &BlockSnapshot) -> BlockShape {
    BlockShape {
        location: BlockLocation {
            block_id: block.block_id.clone(),
            index,
        },
        kind: block.kind.clone(),
        structure_key: block.structure_key.clone(),
    }
}

fn append_text_deltas(
    old: &[FlatToken],
    old_sources: &[SourceId],
    new: &[FlatToken],
    event_source_id: SourceId,
    matches: &[(usize, usize)],
    deltas: &mut Vec<DeltaSegment>,
) {
    let mut old_cursor = 0;
    let mut new_cursor = 0;
    for &(old_match, new_match) in matches
        .iter()
        .chain(std::iter::once(&(old.len(), new.len())))
    {
        append_deletions(
            &old[old_cursor..old_match],
            &old_sources[old_cursor..old_match],
            event_source_id,
            deltas,
        );
        append_insertions(&new[new_cursor..new_match], event_source_id, deltas);
        old_cursor = old_match.saturating_add(1);
        new_cursor = new_match.saturating_add(1);
    }
}

fn append_deletions(
    tokens: &[FlatToken],
    sources: &[SourceId],
    event_source_id: SourceId,
    deltas: &mut Vec<DeltaSegment>,
) {
    let mut start = 0;
    while start < tokens.len() {
        let source = sources[start];
        let mut end = start + 1;
        while end < tokens.len()
            && sources[end] == source
            && contiguous(&tokens[end - 1].location, &tokens[end].location)
        {
            end += 1;
        }
        deltas.push(DeltaSegment::Delete {
            event_source_id,
            content_source_id: source,
            before: joined_location(&tokens[start..end]),
            text: joined_text(&tokens[start..end]),
        });
        start = end;
    }
}

fn append_insertions(
    tokens: &[FlatToken],
    event_source_id: SourceId,
    deltas: &mut Vec<DeltaSegment>,
) {
    let mut start = 0;
    while start < tokens.len() {
        let mut end = start + 1;
        while end < tokens.len() && contiguous(&tokens[end - 1].location, &tokens[end].location) {
            end += 1;
        }
        deltas.push(DeltaSegment::Insert {
            event_source_id,
            after: joined_location(&tokens[start..end]),
            text: joined_text(&tokens[start..end]),
        });
        start = end;
    }
}

fn append_format_deltas(
    old: &[FlatToken],
    old_sources: &[SourceId],
    new: &[FlatToken],
    event_source_id: SourceId,
    matches: &[(usize, usize)],
    deltas: &mut Vec<DeltaSegment>,
) {
    let mut cursor = 0;
    while cursor < matches.len() {
        let (old_index, new_index) = matches[cursor];
        if old[old_index].format_key == new[new_index].format_key {
            cursor += 1;
            continue;
        }
        let source = old_sources[old_index];
        let before_format = old[old_index].format_key.clone();
        let after_format = new[new_index].format_key.clone();
        let mut end = cursor + 1;
        while end < matches.len() {
            let (previous_old, previous_new) = matches[end - 1];
            let (next_old, next_new) = matches[end];
            if next_old != previous_old + 1
                || next_new != previous_new + 1
                || old_sources[next_old] != source
                || old[next_old].format_key != before_format
                || new[next_new].format_key != after_format
                || !contiguous(&old[previous_old].location, &old[next_old].location)
                || !contiguous(&new[previous_new].location, &new[next_new].location)
            {
                break;
            }
            end += 1;
        }
        let old_tokens = matches[cursor..end]
            .iter()
            .map(|(old_index, _)| old[*old_index].clone())
            .collect::<Vec<_>>();
        let new_tokens = matches[cursor..end]
            .iter()
            .map(|(_, new_index)| new[*new_index].clone())
            .collect::<Vec<_>>();
        deltas.push(DeltaSegment::Format {
            event_source_id,
            content_source_id: source,
            before: joined_location(&old_tokens),
            after: joined_location(&new_tokens),
            text: joined_text(&new_tokens),
            before_format,
            after_format,
        });
        cursor = end;
    }
}

fn compress_spans(tokens: &[FlatToken], sources: &[SourceId]) -> Vec<LiveLineageSpan> {
    let mut spans = Vec::new();
    let mut start = 0;
    while start < tokens.len() {
        let source_id = sources[start];
        let mut end = start + 1;
        while end < tokens.len()
            && sources[end] == source_id
            && contiguous(&tokens[end - 1].location, &tokens[end].location)
        {
            end += 1;
        }
        spans.push(LiveLineageSpan {
            location: joined_location(&tokens[start..end]),
            source_id,
        });
        start = end;
    }
    spans
}

fn contiguous(left: &TextLocation, right: &TextLocation) -> bool {
    left.block_id == right.block_id && left.path == right.path && left.to_utf16 == right.from_utf16
}

fn joined_location(tokens: &[FlatToken]) -> TextLocation {
    debug_assert!(!tokens.is_empty());
    TextLocation {
        block_id: tokens[0].location.block_id.clone(),
        path: tokens[0].location.path.clone(),
        from_utf16: tokens[0].location.from_utf16,
        to_utf16: tokens.last().unwrap().location.to_utf16,
    }
}

fn joined_text(tokens: &[FlatToken]) -> String {
    tokens.iter().map(|token| token.text.as_str()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn human(id: u64) -> SourceDescriptor {
        SourceDescriptor::new(
            SourceId(id),
            "local:written",
            "Written here",
            Ingress::Entered,
            Assurance::Observed,
        )
    }

    fn pasted(id: u64) -> SourceDescriptor {
        SourceDescriptor::new(
            SourceId(id),
            "local:pasted",
            "Pasted",
            Ingress::Pasted,
            Assurance::Observed,
        )
    }

    fn claude(id: u64) -> SourceDescriptor {
        SourceDescriptor::new(
            SourceId(id),
            "mcp:connection:claude",
            "Claude",
            Ingress::Mcp,
            Assurance::Reported,
        )
    }

    fn chatgpt(id: u64) -> SourceDescriptor {
        SourceDescriptor::new(
            SourceId(id),
            "mcp:connection:chatgpt",
            "ChatGPT",
            Ingress::Mcp,
            Assurance::Reported,
        )
    }

    fn block(id: &str, text: &str) -> BlockSnapshot {
        BlockSnapshot::plain(id, "paragraph", text)
    }

    fn count(state: &LineageState, source: SourceId) -> usize {
        state
            .current_source_summary()
            .unwrap()
            .contributions
            .into_iter()
            .find(|item| item.source.id == source)
            .map(|item| item.graphemes)
            .unwrap_or(0)
    }

    fn span(from_utf16: u32, to_utf16: u32, source_id: SourceId) -> LiveLineageSpan {
        LiveLineageSpan {
            location: TextLocation {
                block_id: "a".into(),
                path: vec![0],
                from_utf16,
                to_utf16,
            },
            source_id,
        }
    }

    fn descriptors(
        sources: impl IntoIterator<Item = SourceDescriptor>,
    ) -> BTreeMap<SourceId, SourceDescriptor> {
        sources
            .into_iter()
            .map(|source| (source.id, source))
            .collect()
    }

    #[test]
    fn grammar_edit_preserves_every_equal_grapheme() {
        let before =
            LineageState::seed(vec![block("a", "This sentence are clear.")], human(1)).unwrap();
        let change = before
            .reconcile(vec![block("a", "This sentence is clear.")], claude(2))
            .unwrap();

        assert_eq!(count(&change.state, SourceId(2)), 2, "only `is` is new");
        assert_eq!(count(&change.state, SourceId(1)), 21);
        assert!(change.deltas.iter().any(|delta| matches!(
            delta,
            DeltaSegment::Delete { text, .. } if text == "are"
        )));
        assert!(change.deltas.iter().any(|delta| matches!(
            delta,
            DeltaSegment::Insert { text, .. } if text == "is"
        )));
    }

    #[test]
    fn ambiguous_duplicate_deletion_uses_the_documented_deterministic_inference() {
        let first = LineageState::seed(vec![block("a", "yes")], human(1)).unwrap();
        let both = first
            .reconcile(vec![block("a", "yesyes")], claude(2))
            .unwrap()
            .state;
        let once = both.reconcile(vec![block("a", "yes")], human(3)).unwrap();

        // Before/after snapshots cannot reveal which equal occurrence the
        // person deleted. V1 deliberately makes one stable choice and its API
        // labels the result deterministic inference, not observed intent.
        assert_eq!(count(&once.state, SourceId(1)), 3);
        assert_eq!(count(&once.state, SourceId(2)), 0);
        assert!(once.deltas.iter().any(|delta| matches!(
            delta,
            DeltaSegment::Delete { content_source_id: SourceId(2), text, .. } if text == "yes"
        )));
    }

    #[test]
    fn deletion_leaves_no_live_contribution_but_keeps_deleted_source() {
        let before = LineageState::seed(vec![block("a", "Keep. Remove this.")], human(1)).unwrap();
        let change = before
            .reconcile(vec![block("a", "Keep.")], claude(2))
            .unwrap();

        assert_eq!(count(&change.state, SourceId(2)), 0);
        assert!(change.deltas.iter().any(|delta| matches!(
            delta,
            DeltaSegment::Delete { content_source_id: SourceId(1), text, .. }
                if text == " Remove this."
        )));
    }

    #[test]
    fn later_edit_reassigns_only_the_replacement() {
        let initial = LineageState::seed(vec![block("a", "A tiny draft.")], claude(1)).unwrap();
        let revised = initial
            .reconcile(vec![block("a", "A short draft.")], human(2))
            .unwrap();

        // The shared `t` survives the replacement; only `shor` is introduced.
        assert_eq!(count(&revised.state, SourceId(2)), 4);
        assert_eq!(count(&revised.state, SourceId(1)), 10);
    }

    #[test]
    fn formatting_records_action_without_reattributing_text() {
        let plain = BlockSnapshot::new(
            "a",
            "paragraph",
            "",
            vec![TextLeafSnapshot::new(vec![0], "important", "")],
        );
        let bold = BlockSnapshot::new(
            "a",
            "paragraph",
            "",
            vec![TextLeafSnapshot::new(vec![0], "important", "bold")],
        );
        let before = LineageState::seed(vec![plain], human(1)).unwrap();
        let change = before.reconcile(vec![bold], claude(2)).unwrap();

        assert_eq!(count(&change.state, SourceId(1)), 9);
        assert_eq!(count(&change.state, SourceId(2)), 0);
        assert!(matches!(
            change.deltas.as_slice(),
            [DeltaSegment::Format { .. }]
        ));
    }

    #[test]
    fn block_type_change_preserves_text_and_records_structure() {
        let before = LineageState::seed(vec![block("a", "Heading")], human(1)).unwrap();
        let after = BlockSnapshot::plain("a", "heading", "Heading");
        let change = before.reconcile(vec![after], claude(2)).unwrap();

        assert_eq!(count(&change.state, SourceId(1)), 7);
        assert_eq!(count(&change.state, SourceId(2)), 0);
        assert!(matches!(
            change.deltas.as_slice(),
            [DeltaSegment::Structure { .. }]
        ));
    }

    #[test]
    fn split_and_merge_preserve_text_across_block_boundaries() {
        let before = LineageState::seed(vec![block("a", "alpha beta")], human(1)).unwrap();
        let split = before
            .reconcile(vec![block("a", "alpha "), block("b", "beta")], claude(2))
            .unwrap();
        assert_eq!(count(&split.state, SourceId(1)), 10);
        assert_eq!(count(&split.state, SourceId(2)), 0);

        let merged = split
            .state
            .reconcile(vec![block("a", "alpha beta")], pasted(3))
            .unwrap();
        assert_eq!(count(&merged.state, SourceId(1)), 10);
        assert_eq!(count(&merged.state, SourceId(3)), 0);
    }

    #[test]
    fn emoji_and_combining_marks_are_single_graphemes_with_utf16_locations() {
        let before = LineageState::seed(vec![block("a", "e\u{301} 👩‍💻")], human(1)).unwrap();
        let summary = before.current_source_summary().unwrap();
        assert_eq!(summary.total_graphemes, 3);
        assert_eq!(before.spans()[0].location.to_utf16, 8);

        let change = before
            .reconcile(vec![block("a", "e\u{301} 👩‍💻!")], claude(2))
            .unwrap();
        assert_eq!(count(&change.state, SourceId(2)), 1);
        assert!(change.deltas.iter().any(|delta| matches!(
            delta,
            DeltaSegment::Insert { after, text, .. }
                if text == "!" && after.from_utf16 == 8 && after.to_utf16 == 9
        )));
    }

    #[test]
    fn persisted_parts_rebuild_deterministically() {
        let initial =
            LineageState::seed(vec![block("a", "one"), block("b", "two")], human(1)).unwrap();
        let first = initial
            .reconcile(vec![block("a", "one plus"), block("b", "two")], claude(2))
            .unwrap();
        let rebuilt = LineageState::from_parts(
            first.state.blocks().to_vec(),
            first.state.spans().to_vec(),
            first.state.sources().clone(),
        )
        .unwrap();
        let after = vec![block("a", "one plus"), block("b", "two more")];

        assert_eq!(
            first.state.reconcile(after.clone(), human(3)).unwrap(),
            rebuilt.reconcile(after, human(3)).unwrap()
        );
    }

    #[test]
    fn sequential_insertions_preserve_each_distinct_source() {
        let empty = LineageState::seed(vec![block("a", "")], human(1)).unwrap();
        let first = empty
            .reconcile(vec![block("a", "Alpha")], claude(2))
            .unwrap();
        let second = first
            .state
            .reconcile(vec![block("a", "Alpha!")], chatgpt(3))
            .unwrap();

        assert_eq!(count(&second.state, SourceId(2)), 5);
        assert_eq!(count(&second.state, SourceId(3)), 1);
        assert_eq!(second.state.spans().len(), 2);
        let summary = second.state.current_source_summary().unwrap();
        assert_eq!(summary.contributions.len(), 2);
        assert_eq!(summary.grouped_contributions.len(), 2);
        assert!(matches!(
            second.deltas.as_slice(),
            [DeltaSegment::Insert {
                event_source_id: SourceId(3),
                text,
                ..
            }] if text == "!"
        ));
    }

    #[test]
    fn repeated_written_events_collapse_only_in_the_grouped_summary() {
        let first = LineageState::seed(vec![block("a", "A")], human(1)).unwrap();
        let second = first
            .reconcile(vec![block("a", "AB")], human(2))
            .unwrap()
            .state;
        let summary = second.current_source_summary().unwrap();

        assert_eq!(summary.contributions.len(), 2);
        assert_eq!(summary.contributions[0].source.id, SourceId(1));
        assert_eq!(summary.contributions[1].source.id, SourceId(2));
        assert_eq!(second.spans().len(), 2);

        assert_eq!(summary.grouped_contributions.len(), 1);
        let written = &summary.grouped_contributions[0];
        assert_eq!(written.group.key, "local:written");
        assert_eq!(written.group.label, "Written here");
        assert_eq!(written.event_count, 2);
        assert_eq!(written.graphemes, 2);
        assert_eq!(written.non_whitespace_graphemes, 2);
    }

    #[test]
    fn grouped_summary_uses_the_latest_contributing_source_label() {
        let old_label = SourceDescriptor::new(
            SourceId(1),
            "mcp:connection:reviewer-1",
            "Claude reviewer (reported)",
            Ingress::Mcp,
            Assurance::Reported,
        );
        let current_label = SourceDescriptor::new(
            SourceId(2),
            "mcp:connection:reviewer-1",
            "Research reviewer (reported)",
            Ingress::Mcp,
            Assurance::Reported,
        );
        let first = LineageState::seed(vec![block("a", "A")], old_label).unwrap();
        let state = first
            .reconcile(vec![block("a", "AB")], current_label)
            .unwrap()
            .state;
        let summary = state.current_source_summary().unwrap();

        assert_eq!(summary.contributions.len(), 2);
        assert_eq!(summary.grouped_contributions.len(), 1);
        let reviewer = &summary.grouped_contributions[0];
        assert_eq!(reviewer.event_count, 2);
        assert_eq!(reviewer.group.label, "Research reviewer (reported)");
    }

    #[test]
    fn summary_denominator_excludes_whitespace_but_includes_punctuation() {
        let entered = LineageState::seed(vec![block("a", "A")], human(1)).unwrap();
        let state = entered
            .reconcile(vec![block("a", "A !")], pasted(2))
            .unwrap()
            .state;
        let summary = state.current_source_summary().unwrap();

        assert_eq!(summary.total_graphemes, 3);
        assert_eq!(summary.total_non_whitespace_graphemes, 2);
        let entered = summary
            .contributions
            .iter()
            .find(|item| item.source.id == SourceId(1))
            .unwrap();
        let pasted = summary
            .contributions
            .iter()
            .find(|item| item.source.id == SourceId(2))
            .unwrap();
        assert_eq!(
            (entered.graphemes, entered.non_whitespace_graphemes),
            (1, 1)
        );
        assert_eq!((pasted.graphemes, pasted.non_whitespace_graphemes), (2, 1));
    }

    #[test]
    fn empty_visible_text_has_no_spans_or_contribution() {
        let state = LineageState::seed(vec![block("a", "")], human(1)).unwrap();
        let summary = state.current_source_summary().unwrap();

        assert!(state.spans().is_empty());
        assert_eq!(summary.total_graphemes, 0);
        assert_eq!(summary.total_non_whitespace_graphemes, 0);
        assert!(summary.contributions.is_empty());
    }

    #[test]
    fn rehydration_rejects_unknown_sources_and_uncovered_text() {
        let unknown = LineageState::from_parts(
            vec![block("a", "a")],
            vec![span(0, 1, SourceId(9))],
            descriptors([human(1)]),
        );
        assert_eq!(unknown, Err(ReconcileError::UnknownSource(SourceId(9))));

        let uncovered = LineageState::from_parts(
            vec![block("a", "ab")],
            vec![span(0, 1, SourceId(1))],
            descriptors([human(1)]),
        );
        assert!(
            matches!(uncovered, Err(ReconcileError::UncoveredText(location))
            if location.from_utf16 == 1 && location.to_utf16 == 2)
        );
    }

    #[test]
    fn rehydration_rejects_overlap_and_non_grapheme_boundaries() {
        let overlapping = LineageState::from_parts(
            vec![block("a", "abc")],
            vec![span(0, 2, SourceId(1)), span(1, 3, SourceId(1))],
            descriptors([human(1)]),
        );
        assert!(matches!(
            overlapping,
            Err(ReconcileError::OverlappingSpans(_, _))
        ));

        // `e` plus COMBINING ACUTE ACCENT is one grapheme spanning two UTF-16
        // code units, so offset 1 is not a legal lineage boundary.
        let split_grapheme = LineageState::from_parts(
            vec![block("a", "e\u{301}")],
            vec![span(0, 1, SourceId(1)), span(1, 2, SourceId(1))],
            descriptors([human(1)]),
        );
        assert!(matches!(
            split_grapheme,
            Err(ReconcileError::InvalidSpan(_))
        ));
    }

    #[test]
    fn conflicting_source_metadata_is_rejected() {
        let state = LineageState::seed(vec![block("a", "stable")], human(1)).unwrap();
        let conflicting = SourceDescriptor::new(
            SourceId(1),
            "mcp:connection:someone-else",
            "Someone else",
            Ingress::Mcp,
            Assurance::Verified,
        );

        assert_eq!(
            state.reconcile(vec![block("a", "stable")], conflicting),
            Err(ReconcileError::SourceConflict(SourceId(1)))
        );
    }

    #[test]
    fn no_visible_change_registers_the_event_without_claiming_text() {
        let state = LineageState::seed(vec![block("a", "unchanged")], human(1)).unwrap();
        let event = state
            .reconcile(vec![block("a", "unchanged")], claude(2))
            .unwrap();

        assert!(event.deltas.is_empty());
        assert_eq!(count(&event.state, SourceId(1)), 9);
        assert_eq!(count(&event.state, SourceId(2)), 0);
        assert!(event.state.sources().contains_key(&SourceId(2)));
    }

    #[test]
    fn ingress_trust_vocabulary_has_stable_distinct_serde_values() {
        let cases = [
            (Ingress::Entered, "entered"),
            (Ingress::Command, "command"),
            (Ingress::Pasted, "pasted"),
            (Ingress::Imported, "imported"),
            (Ingress::Mcp, "mcp"),
            (Ingress::Api, "api"),
            (Ingress::Suggestion, "suggestion"),
            (Ingress::Unknown, "unknown"),
            (Ingress::LegacyUnknown, "legacy_unknown"),
        ];

        for (ingress, persisted) in cases {
            let json = serde_json::to_string(&ingress).unwrap();
            assert_eq!(json, format!("\"{persisted}\""));
            assert_eq!(serde_json::from_str::<Ingress>(&json).unwrap(), ingress);
        }
    }
}
