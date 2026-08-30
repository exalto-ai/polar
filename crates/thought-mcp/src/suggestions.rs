//! Transport-neutral reviewer suggestion requests and outcomes.

use serde::Serialize;
use thought_core::SuggestionRecord;

pub const MAX_SUGGESTION_REQUEST_ID_BYTES: usize = 128;
pub const MAX_SUGGESTION_EXPLANATION_BYTES: usize = 16 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuggestedChange {
    ReplaceBlock {
        block_id: String,
        markdown: String,
    },
    InsertBlocks {
        after: Option<String>,
        markdown: String,
    },
    ReplaceText {
        block_id: String,
        find: String,
        replace: String,
        occurrence: Option<usize>,
    },
    DeleteBlock {
        block_id: String,
    },
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SuggestionOutcome {
    pub suggestion: SuggestionRecord,
    pub content_revision: String,
    pub replayed: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SuggestionList {
    pub content_revision: String,
    pub suggestions: Vec<SuggestionRecord>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct DecisionOutcome {
    pub suggestion: SuggestionRecord,
    pub content_revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SuggestionError {
    InvalidInput(String),
    NotFound(String),
    BaseRevisionMismatch { expected: String, actual: String },
    AlreadyDecided(String),
    CorruptStored(String),
}

impl std::fmt::Display for SuggestionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput(message) => write!(f, "invalid suggestion: {message}"),
            Self::NotFound(id) => write!(f, "suggestion `{id}` was not found"),
            Self::BaseRevisionMismatch { expected, actual } => write!(
                f,
                "suggestion is stale: expected content revision `{expected}`, current `{actual}`"
            ),
            Self::AlreadyDecided(id) => write!(f, "suggestion `{id}` was already decided"),
            Self::CorruptStored(message) => write!(f, "invalid stored suggestion: {message}"),
        }
    }
}

impl std::error::Error for SuggestionError {}
