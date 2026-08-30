//! Current visible-text lineage.
//!
//! The CRDT remains the document history. This crate answers one smaller
//! question: which recorded mutation introduced each surviving grapheme?

use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;
use std::ops::Range;
use unicode_segmentation::UnicodeSegmentation;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct SourceId(pub u64);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Ingress {
    Entered,
    Command,
    Pasted,
    Imported,
    Mcp,
    Api,
    Suggestion,
    Unknown,
    LegacyUnknown,
}

impl Ingress {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Entered => "entered",
            Self::Command => "command",
            Self::Pasted => "pasted",
            Self::Imported => "imported",
            Self::Mcp => "mcp",
            Self::Api => "api",
            Self::Suggestion => "suggestion",
            Self::Unknown => "unknown",
            Self::LegacyUnknown => "legacy_unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "entered" => Self::Entered,
            "command" => Self::Command,
            "pasted" => Self::Pasted,
            "imported" => Self::Imported,
            "mcp" => Self::Mcp,
            "api" => Self::Api,
            "suggestion" => Self::Suggestion,
            "unknown" => Self::Unknown,
            "legacy_unknown" => Self::LegacyUnknown,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Assurance {
    Observed,
    Reported,
    Verified,
    Unknown,
}

impl Assurance {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Observed => "observed",
            Self::Reported => "reported",
            Self::Verified => "verified",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "observed" => Self::Observed,
            "reported" => Self::Reported,
            "verified" => Self::Verified,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Alignment {
    Exact,
    Inferred,
    Unknown,
}

impl Alignment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Exact => "exact",
            Self::Inferred => "inferred",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "exact" => Self::Exact,
            "inferred" => Self::Inferred,
            "unknown" => Self::Unknown,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceDescriptor {
    pub id: SourceId,
    pub group_key: String,
    pub label: String,
    pub ingress: Ingress,
    pub assurance: Assurance,
    pub alignment: Alignment,
}

impl SourceDescriptor {
    pub fn new(
        id: SourceId,
        group_key: impl Into<String>,
        label: impl Into<String>,
        ingress: Ingress,
        assurance: Assurance,
        alignment: Alignment,
    ) -> Self {
        Self {
            id,
            group_key: group_key.into(),
            label: label.into(),
            ingress,
            assurance,
            alignment,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TextLeafSnapshot {
    pub path: Vec<u32>,
    pub text: String,
}

impl TextLeafSnapshot {
    pub fn new(path: impl Into<Vec<u32>>, text: impl Into<String>) -> Self {
        Self {
            path: path.into(),
            text: text.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockSnapshot {
    pub block_id: String,
    pub leaves: Vec<TextLeafSnapshot>,
}

impl BlockSnapshot {
    pub fn new(block_id: impl Into<String>, leaves: Vec<TextLeafSnapshot>) -> Self {
        Self {
            block_id: block_id.into(),
            leaves,
        }
    }

    pub fn plain(block_id: impl Into<String>, text: impl Into<String>) -> Self {
        Self::new(block_id, vec![TextLeafSnapshot::new(vec![0], text)])
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TextLocation {
    pub block_id: String,
    pub path: Vec<u32>,
    pub from_utf16: u32,
    pub to_utf16: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveLineageSpan {
    pub location: TextLocation,
    pub source_id: SourceId,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SemanticRange {
    pub before: Range<usize>,
    pub after: Range<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LineageState {
    blocks: Vec<BlockSnapshot>,
    spans: Vec<LiveLineageSpan>,
    sources: BTreeMap<SourceId, SourceDescriptor>,
}

impl LineageState {
    pub fn seed(
        blocks: Vec<BlockSnapshot>,
        source: SourceDescriptor,
    ) -> Result<Self, LineageError> {
        validate_blocks(&blocks)?;
        let flat = flatten(&blocks);
        let spans = compress(&flat, &vec![source.id; flat.len()]);
        Ok(Self {
            blocks,
            spans,
            sources: BTreeMap::from([(source.id, source)]),
        })
    }

    pub fn from_parts(
        blocks: Vec<BlockSnapshot>,
        spans: Vec<LiveLineageSpan>,
        sources: BTreeMap<SourceId, SourceDescriptor>,
    ) -> Result<Self, LineageError> {
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

    /// Conservative fallback for an operation with no exact range metadata.
    /// Text in unchanged blocks keeps its source. In a changed block, only the
    /// common prefix and suffix keep their source. Everything between them is
    /// conservatively assigned to the new mutation.
    pub fn reconcile(
        &self,
        after: Vec<BlockSnapshot>,
        source: SourceDescriptor,
    ) -> Result<Self, LineageError> {
        validate_blocks(&after)?;
        let old_sources = self.token_sources()?;
        let old_flat = flatten(&self.blocks);
        let new_flat = flatten(&after);
        let mut new_sources = vec![source.id; new_flat.len()];
        let old_blocks = block_ranges(&old_flat);
        let new_blocks = block_ranges(&new_flat);

        for (block_id, new_range) in &new_blocks {
            let Some(old_range) = old_blocks.get(block_id) else {
                continue;
            };
            let old = &old_flat[old_range.clone()];
            let new = &new_flat[new_range.clone()];
            let prefix = common_prefix(old, new);
            let suffix = common_suffix(&old[prefix..], &new[prefix..]);
            for index in 0..prefix {
                new_sources[new_range.start + index] = old_sources[old_range.start + index];
            }
            for index in 0..suffix {
                new_sources[new_range.end - suffix + index] =
                    old_sources[old_range.end - suffix + index];
            }
        }

        self.finish(after, source, new_flat, new_sources)
    }

    /// Apply ranges captured at the operation boundary. Visible text outside
    /// the ranges must be identical. Inside each range, only an equal prefix
    /// and suffix retain their earlier source.
    pub fn reconcile_exact(
        &self,
        after: Vec<BlockSnapshot>,
        source: SourceDescriptor,
        ranges: &[SemanticRange],
    ) -> Result<Self, LineageError> {
        validate_blocks(&after)?;
        validate_ranges(ranges)?;
        let old_flat = flatten(&self.blocks);
        let new_flat = flatten(&after);
        let old_sources = self.token_sources()?;
        let mut new_sources = vec![source.id; new_flat.len()];
        let mut before_cursor = 0;
        let mut after_cursor = 0;

        for range in ranges {
            if range.before.end > old_flat.len() || range.after.end > new_flat.len() {
                return Err(LineageError::InvalidRange);
            }
            copy_equal_region(
                &old_flat,
                &new_flat,
                &old_sources,
                &mut new_sources,
                before_cursor..range.before.start,
                after_cursor..range.after.start,
            )?;

            let old = &old_flat[range.before.clone()];
            let new = &new_flat[range.after.clone()];
            let prefix = common_prefix(old, new);
            let suffix = common_suffix(&old[prefix..], &new[prefix..]);
            for index in 0..prefix {
                new_sources[range.after.start + index] = old_sources[range.before.start + index];
            }
            for index in 0..suffix {
                new_sources[range.after.end - suffix + index] =
                    old_sources[range.before.end - suffix + index];
            }
            before_cursor = range.before.end;
            after_cursor = range.after.end;
        }

        copy_equal_region(
            &old_flat,
            &new_flat,
            &old_sources,
            &mut new_sources,
            before_cursor..old_flat.len(),
            after_cursor..new_flat.len(),
        )?;
        self.finish(after, source, new_flat, new_sources)
    }

    fn finish(
        &self,
        blocks: Vec<BlockSnapshot>,
        source: SourceDescriptor,
        flat: Vec<Token>,
        token_sources: Vec<SourceId>,
    ) -> Result<Self, LineageError> {
        let mut sources = self.sources.clone();
        match sources.get(&source.id) {
            Some(existing) if existing != &source => return Err(LineageError::SourceConflict),
            Some(_) => {}
            None => {
                sources.insert(source.id, source);
            }
        }
        Ok(Self {
            blocks,
            spans: compress(&flat, &token_sources),
            sources,
        })
    }

    pub fn current_source_summary(&self) -> Result<CurrentSourceSummary, LineageError> {
        let flat = flatten(&self.blocks);
        let sources = self.token_sources()?;
        let mut counts = BTreeMap::<SourceId, (usize, usize)>::new();
        for (token, source) in flat.iter().zip(sources) {
            let count = counts.entry(source).or_default();
            count.0 += 1;
            if !token.text.chars().all(char::is_whitespace) {
                count.1 += 1;
            }
        }
        let mut contributions = counts
            .into_iter()
            .map(
                |(id, (graphemes, non_whitespace_graphemes))| SourceContribution {
                    source: self.sources[&id].clone(),
                    graphemes,
                    non_whitespace_graphemes,
                },
            )
            .collect::<Vec<_>>();
        contributions.sort_by(|a, b| {
            b.non_whitespace_graphemes
                .cmp(&a.non_whitespace_graphemes)
                .then_with(|| a.source.id.cmp(&b.source.id))
        });
        Ok(CurrentSourceSummary {
            total_graphemes: flat.len(),
            total_non_whitespace_graphemes: flat
                .iter()
                .filter(|token| !token.text.chars().all(char::is_whitespace))
                .count(),
            grouped_contributions: grouped(&contributions),
            contributions,
        })
    }

    fn token_sources(&self) -> Result<Vec<SourceId>, LineageError> {
        let flat = flatten(&self.blocks);
        flat.iter()
            .map(|token| {
                self.spans
                    .iter()
                    .find(|span| {
                        span.location.block_id == token.location.block_id
                            && span.location.path == token.location.path
                            && span.location.from_utf16 <= token.location.from_utf16
                            && span.location.to_utf16 >= token.location.to_utf16
                    })
                    .ok_or(LineageError::UncoveredText)
                    .and_then(|span| {
                        self.sources
                            .contains_key(&span.source_id)
                            .then_some(span.source_id)
                            .ok_or(LineageError::UnknownSource)
                    })
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceContribution {
    pub source: SourceDescriptor,
    pub graphemes: usize,
    pub non_whitespace_graphemes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceGroup {
    pub key: String,
    pub label: String,
    pub ingress: Ingress,
    pub assurance: Assurance,
    pub alignment: Alignment,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupedSourceContribution {
    pub group: SourceGroup,
    pub event_count: usize,
    pub graphemes: usize,
    pub non_whitespace_graphemes: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrentSourceSummary {
    pub total_graphemes: usize,
    pub total_non_whitespace_graphemes: usize,
    pub contributions: Vec<SourceContribution>,
    pub grouped_contributions: Vec<GroupedSourceContribution>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineageError {
    DuplicateBlock,
    DuplicateLeaf,
    SourceConflict,
    UnknownSource,
    UncoveredText,
    InvalidRange,
    RangeMismatch,
}

impl fmt::Display for LineageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::DuplicateBlock => "duplicate block id",
            Self::DuplicateLeaf => "duplicate text-leaf path",
            Self::SourceConflict => "source id has conflicting metadata",
            Self::UnknownSource => "lineage span refers to an unknown source",
            Self::UncoveredText => "visible text is not covered by lineage",
            Self::InvalidRange => "semantic ranges are invalid",
            Self::RangeMismatch => "text outside semantic ranges changed",
        })
    }
}

impl std::error::Error for LineageError {}

#[derive(Debug, Clone)]
struct Token {
    text: String,
    location: TextLocation,
}

fn flatten(blocks: &[BlockSnapshot]) -> Vec<Token> {
    let mut tokens = Vec::new();
    for block in blocks {
        for leaf in &block.leaves {
            let mut utf16 = 0_u32;
            for grapheme in leaf.text.graphemes(true) {
                let from = utf16;
                utf16 += grapheme.encode_utf16().count() as u32;
                tokens.push(Token {
                    text: grapheme.to_owned(),
                    location: TextLocation {
                        block_id: block.block_id.clone(),
                        path: leaf.path.clone(),
                        from_utf16: from,
                        to_utf16: utf16,
                    },
                });
            }
        }
    }
    tokens
}

fn block_ranges(tokens: &[Token]) -> HashMap<String, Range<usize>> {
    let mut ranges = HashMap::new();
    for (index, token) in tokens.iter().enumerate() {
        ranges
            .entry(token.location.block_id.clone())
            .and_modify(|range: &mut Range<usize>| range.end = index + 1)
            .or_insert(index..index + 1);
    }
    ranges
}

fn validate_blocks(blocks: &[BlockSnapshot]) -> Result<(), LineageError> {
    let mut block_ids = HashSet::new();
    for block in blocks {
        if !block_ids.insert(&block.block_id) {
            return Err(LineageError::DuplicateBlock);
        }
        let mut paths = HashSet::new();
        for leaf in &block.leaves {
            if !paths.insert(&leaf.path) {
                return Err(LineageError::DuplicateLeaf);
            }
        }
    }
    Ok(())
}

fn validate_ranges(ranges: &[SemanticRange]) -> Result<(), LineageError> {
    let mut before = 0;
    let mut after = 0;
    for range in ranges {
        if range.before.start > range.before.end
            || range.after.start > range.after.end
            || range.before.start < before
            || range.after.start < after
        {
            return Err(LineageError::InvalidRange);
        }
        before = range.before.end;
        after = range.after.end;
    }
    Ok(())
}

fn copy_equal_region(
    before: &[Token],
    after: &[Token],
    before_sources: &[SourceId],
    after_sources: &mut [SourceId],
    before_range: Range<usize>,
    after_range: Range<usize>,
) -> Result<(), LineageError> {
    let old = &before[before_range.clone()];
    let new = &after[after_range.clone()];
    if old.len() != new.len() || old.iter().zip(new).any(|(a, b)| a.text != b.text) {
        return Err(LineageError::RangeMismatch);
    }
    after_sources[after_range].copy_from_slice(&before_sources[before_range]);
    Ok(())
}

fn common_prefix(a: &[Token], b: &[Token]) -> usize {
    a.iter()
        .zip(b)
        .take_while(|(a, b)| a.text == b.text)
        .count()
}

fn common_suffix(a: &[Token], b: &[Token]) -> usize {
    a.iter()
        .rev()
        .zip(b.iter().rev())
        .take_while(|(a, b)| a.text == b.text)
        .count()
}

fn compress(tokens: &[Token], sources: &[SourceId]) -> Vec<LiveLineageSpan> {
    let mut spans = Vec::new();
    let mut start = 0;
    while start < tokens.len() {
        let mut end = start + 1;
        while end < tokens.len()
            && sources[end] == sources[start]
            && tokens[end - 1].location.block_id == tokens[end].location.block_id
            && tokens[end - 1].location.path == tokens[end].location.path
            && tokens[end - 1].location.to_utf16 == tokens[end].location.from_utf16
        {
            end += 1;
        }
        spans.push(LiveLineageSpan {
            location: TextLocation {
                block_id: tokens[start].location.block_id.clone(),
                path: tokens[start].location.path.clone(),
                from_utf16: tokens[start].location.from_utf16,
                to_utf16: tokens[end - 1].location.to_utf16,
            },
            source_id: sources[start],
        });
        start = end;
    }
    spans
}

fn grouped(contributions: &[SourceContribution]) -> Vec<GroupedSourceContribution> {
    let mut groups = BTreeMap::<String, (SourceId, GroupedSourceContribution)>::new();
    for contribution in contributions {
        let entry = groups
            .entry(contribution.source.group_key.clone())
            .or_insert_with(|| {
                (
                    contribution.source.id,
                    GroupedSourceContribution {
                        group: SourceGroup {
                            key: contribution.source.group_key.clone(),
                            label: contribution.source.label.clone(),
                            ingress: contribution.source.ingress,
                            assurance: contribution.source.assurance,
                            alignment: contribution.source.alignment,
                        },
                        event_count: 0,
                        graphemes: 0,
                        non_whitespace_graphemes: 0,
                    },
                )
            });
        if contribution.source.id > entry.0 {
            entry.0 = contribution.source.id;
            entry.1.group.label = contribution.source.label.clone();
        }
        entry.1.event_count += 1;
        entry.1.graphemes += contribution.graphemes;
        entry.1.non_whitespace_graphemes += contribution.non_whitespace_graphemes;
        entry.1.group.alignment = match (entry.1.group.alignment, contribution.source.alignment) {
            (Alignment::Unknown, _) | (_, Alignment::Unknown) => Alignment::Unknown,
            (Alignment::Inferred, _) | (_, Alignment::Inferred) => Alignment::Inferred,
            _ => Alignment::Exact,
        };
    }
    let mut groups = groups
        .into_values()
        .map(|(_, group)| group)
        .collect::<Vec<_>>();
    groups.sort_by(|a, b| {
        b.non_whitespace_graphemes
            .cmp(&a.non_whitespace_graphemes)
            .then_with(|| a.group.key.cmp(&b.group.key))
    });
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(id: u64, label: &str, alignment: Alignment) -> SourceDescriptor {
        SourceDescriptor::new(
            SourceId(id),
            label,
            label,
            Ingress::Entered,
            Assurance::Observed,
            alignment,
        )
    }

    #[test]
    fn a_small_edit_preserves_untouched_text() {
        let before = LineageState::seed(
            vec![BlockSnapshot::plain("a", "cats are nice")],
            source(1, "human", Alignment::Exact),
        )
        .unwrap();
        let after = before
            .reconcile(
                vec![BlockSnapshot::plain("a", "cats are good")],
                source(2, "reviewer", Alignment::Inferred),
            )
            .unwrap();
        let summary = after.current_source_summary().unwrap();
        assert_eq!(
            summary
                .contributions
                .iter()
                .find(|item| item.source.id == SourceId(1))
                .unwrap()
                .graphemes,
            9,
        );
        assert_eq!(
            summary
                .contributions
                .iter()
                .find(|item| item.source.id == SourceId(2))
                .unwrap()
                .graphemes,
            4,
        );
    }

    #[test]
    fn exact_ranges_disambiguate_repeated_text() {
        let first = LineageState::seed(
            vec![BlockSnapshot::plain("a", "yes")],
            source(1, "first", Alignment::Exact),
        )
        .unwrap();
        let second = first
            .reconcile_exact(
                vec![BlockSnapshot::plain("a", "yesyes")],
                source(2, "second", Alignment::Exact),
                &[SemanticRange {
                    before: 3..3,
                    after: 3..6,
                }],
            )
            .unwrap();
        let final_state = second
            .reconcile_exact(
                vec![BlockSnapshot::plain("a", "yes")],
                source(3, "delete", Alignment::Exact),
                &[SemanticRange {
                    before: 3..6,
                    after: 3..3,
                }],
            )
            .unwrap();
        assert_eq!(
            final_state.current_source_summary().unwrap().contributions[0]
                .source
                .id,
            SourceId(1),
        );
    }

    #[test]
    fn exact_ranges_reject_changes_outside_the_operation() {
        let before = LineageState::seed(
            vec![BlockSnapshot::plain("a", "one two")],
            source(1, "human", Alignment::Exact),
        )
        .unwrap();
        assert_eq!(
            before.reconcile_exact(
                vec![BlockSnapshot::plain("a", "ONE TWO")],
                source(2, "edit", Alignment::Exact),
                &[SemanticRange {
                    before: 0..3,
                    after: 0..3,
                }],
            ),
            Err(LineageError::RangeMismatch),
        );
    }

    #[test]
    fn emoji_offsets_use_utf16_and_never_split_graphemes() {
        let state = LineageState::seed(
            vec![BlockSnapshot::plain("a", "a👨‍👩‍👧‍👦b")],
            source(1, "human", Alignment::Exact),
        )
        .unwrap();
        assert_eq!(state.spans()[0].location.to_utf16, 13);
        assert_eq!(state.current_source_summary().unwrap().total_graphemes, 3);
    }
}
