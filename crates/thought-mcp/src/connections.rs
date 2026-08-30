//! Safe reviewer metadata and its SQLite operations.

use crate::{ActorRef, MutationContext, Workspace, WorkspaceError};
use serde::{Deserialize, Serialize};
use thought_store::{NewReviewerConnection, ReviewerConnectionRow};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReviewerClient {
    Chatgpt,
    Codex,
    ClaudeDesktop,
    ClaudeCode,
}

impl ReviewerClient {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chatgpt => "chatgpt",
            Self::Codex => "codex",
            Self::ClaudeDesktop => "claude_desktop",
            Self::ClaudeCode => "claude_code",
        }
    }

    pub const fn provider(self) -> ReviewerProvider {
        match self {
            Self::Chatgpt | Self::Codex => ReviewerProvider::Openai,
            Self::ClaudeDesktop | Self::ClaudeCode => ReviewerProvider::Anthropic,
        }
    }

    pub const fn configured_route_name(self) -> &'static str {
        match self {
            Self::Chatgpt => "ChatGPT desktop",
            Self::Codex => "Codex",
            Self::ClaudeDesktop => "Claude Desktop",
            Self::ClaudeCode => "Claude Code",
        }
    }
}

impl TryFrom<&str> for ReviewerClient {
    type Error = WorkspaceError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "chatgpt" => Ok(Self::Chatgpt),
            "codex" => Ok(Self::Codex),
            "claude_desktop" => Ok(Self::ClaudeDesktop),
            "claude_code" => Ok(Self::ClaudeCode),
            _ => Err(WorkspaceError::NotFound("invalid reviewer client".into())),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewerProvider {
    Openai,
    Anthropic,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewerConnectionStatus {
    Configured,
    Revoked,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ReviewerDocumentScope {
    Current,
    All,
}

impl ReviewerDocumentScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Current => "current",
            Self::All => "all",
        }
    }
}

impl TryFrom<&str> for ReviewerDocumentScope {
    type Error = WorkspaceError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "current" => Ok(Self::Current),
            "all" => Ok(Self::All),
            _ => Err(WorkspaceError::NotFound("invalid reviewer scope".into())),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewerAccess {
    pub document_scope: ReviewerDocumentScope,
    pub document_id: Option<String>,
}

impl ReviewerAccess {
    pub fn current(document_id: impl Into<String>) -> Self {
        Self {
            document_scope: ReviewerDocumentScope::Current,
            document_id: Some(document_id.into()),
        }
    }

    pub fn all() -> Self {
        Self {
            document_scope: ReviewerDocumentScope::All,
            document_id: None,
        }
    }

    pub fn validate(&self) -> Result<(), WorkspaceError> {
        match (self.document_scope, self.document_id.as_deref()) {
            (ReviewerDocumentScope::Current, Some(id)) if !id.trim().is_empty() => Ok(()),
            (ReviewerDocumentScope::All, None) => Ok(()),
            _ => Err(WorkspaceError::NotFound(
                "current access needs one document; all access needs none".into(),
            )),
        }
    }

    pub fn allows(&self, document_id: &str) -> bool {
        self.document_scope == ReviewerDocumentScope::All
            || self.document_id.as_deref() == Some(document_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewerConnection {
    pub id: String,
    pub client: ReviewerClient,
    pub provider: ReviewerProvider,
    pub display_label: String,
    pub status: ReviewerConnectionStatus,
    pub access: ReviewerAccess,
    pub revision: i64,
    pub created_at: i64,
    pub last_seen_at: Option<i64>,
    pub revoked_at: Option<i64>,
    pub reported_model: Option<String>,
}

impl ReviewerConnection {
    pub fn allows_document(&self, document_id: &str) -> bool {
        self.status == ReviewerConnectionStatus::Configured && self.access.allows(document_id)
    }

    pub fn reported_actor(
        &self,
        session_id: Option<&str>,
        reported_model: Option<&str>,
    ) -> ActorRef {
        ActorRef::reviewer(&self.id, &self.display_label, reported_model, session_id)
    }

    pub fn reported_mutation_context(&self) -> MutationContext {
        MutationContext::mcp_connection(
            format!(
                "Configured for {} (reported)",
                self.client.configured_route_name()
            ),
            &self.id,
        )
    }
}

impl TryFrom<ReviewerConnectionRow> for ReviewerConnection {
    type Error = WorkspaceError;

    fn try_from(row: ReviewerConnectionRow) -> Result<Self, Self::Error> {
        let client = ReviewerClient::try_from(row.client.as_str())?;
        let access = ReviewerAccess {
            document_scope: ReviewerDocumentScope::try_from(row.document_scope.as_str())?,
            document_id: row.document_id,
        };
        access.validate()?;
        Ok(Self {
            id: row.id,
            client,
            provider: client.provider(),
            display_label: row.display_label,
            status: if row.revoked_at.is_some() {
                ReviewerConnectionStatus::Revoked
            } else {
                ReviewerConnectionStatus::Configured
            },
            access,
            revision: row.revision,
            created_at: row.created_at,
            last_seen_at: row.last_seen_at,
            revoked_at: row.revoked_at,
            reported_model: row.reported_model,
        })
    }
}

pub struct CreateReviewerConnection {
    pub id: String,
    pub client: ReviewerClient,
    pub display_label: String,
    pub access: ReviewerAccess,
    pub credential_hash: [u8; 32],
    pub created_at: i64,
}

pub struct UpdateReviewerConnection {
    pub expected_revision: i64,
    pub display_label: String,
    pub access: ReviewerAccess,
    pub updated_at: i64,
}

impl Workspace {
    pub fn create_reviewer_connection(
        &self,
        input: &CreateReviewerConnection,
    ) -> Result<ReviewerConnection, WorkspaceError> {
        input.access.validate()?;
        let row = self.with_store(|store| {
            store.create_reviewer_connection(&NewReviewerConnection {
                id: &input.id,
                client: input.client.as_str(),
                display_label: &input.display_label,
                document_scope: input.access.document_scope.as_str(),
                document_id: input.access.document_id.as_deref(),
                credential_hash: &input.credential_hash,
                created_at: input.created_at,
            })
        })?;
        row.try_into()
    }

    pub fn reviewer_connection(&self, id: &str) -> Result<ReviewerConnection, WorkspaceError> {
        self.with_store(|store| store.reviewer_connection(id))?
            .ok_or_else(|| WorkspaceError::NotFound(format!("no reviewer connection `{id}`")))?
            .try_into()
    }

    pub fn list_reviewer_connections(&self) -> Result<Vec<ReviewerConnection>, WorkspaceError> {
        self.with_store(|store| store.list_reviewer_connections())?
            .into_iter()
            .map(TryInto::try_into)
            .collect()
    }

    pub fn reviewer_connection_by_credential_hash(
        &self,
        hash: &[u8; 32],
    ) -> Result<Option<ReviewerConnection>, WorkspaceError> {
        self.with_store(|store| store.reviewer_connection_by_credential_hash(hash))?
            .map(TryInto::try_into)
            .transpose()
    }

    pub fn update_reviewer_connection(
        &self,
        id: &str,
        input: &UpdateReviewerConnection,
    ) -> Result<ReviewerConnection, WorkspaceError> {
        input.access.validate()?;
        self.with_store(|store| {
            store.update_reviewer_connection(
                id,
                input.expected_revision,
                &input.display_label,
                input.access.document_scope.as_str(),
                input.access.document_id.as_deref(),
                input.updated_at,
            )
        })?
        .ok_or_else(|| WorkspaceError::NotFound("reviewer changed; refresh and try again".into()))?
        .try_into()
    }

    pub fn rotate_reviewer_credential(
        &self,
        id: &str,
        expected_revision: i64,
        hash: &[u8; 32],
        updated_at: i64,
    ) -> Result<ReviewerConnection, WorkspaceError> {
        self.with_store(|store| {
            store.rotate_reviewer_credential(id, expected_revision, hash, updated_at)
        })?
        .ok_or_else(|| WorkspaceError::NotFound("reviewer changed; refresh and try again".into()))?
        .try_into()
    }

    pub fn revoke_reviewer_connection(
        &self,
        id: &str,
        expected_revision: i64,
        revoked_at: i64,
    ) -> Result<ReviewerConnection, WorkspaceError> {
        self.with_store(|store| {
            store.revoke_reviewer_connection(id, expected_revision, revoked_at)
        })?
        .ok_or_else(|| WorkspaceError::NotFound("reviewer changed; refresh and try again".into()))?
        .try_into()
    }

    pub fn mark_reviewer_seen(
        &self,
        id: &str,
        seen_at: i64,
    ) -> Result<ReviewerConnection, WorkspaceError> {
        self.with_store(|store| store.mark_reviewer_seen(id, seen_at))?
            .ok_or_else(|| WorkspaceError::NotFound("reviewer was removed".into()))?
            .try_into()
    }

    pub fn update_reviewer_reported_model(
        &self,
        id: &str,
        model: Option<&str>,
    ) -> Result<ReviewerConnection, WorkspaceError> {
        self.with_store(|store| store.update_reviewer_reported_model(id, model))?
            .ok_or_else(|| WorkspaceError::NotFound("reviewer was removed".into()))?
            .try_into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn connection_serialization_never_contains_credentials() {
        let workspace = Workspace::open_in_memory().unwrap();
        let connection = workspace
            .create_reviewer_connection(&CreateReviewerConnection {
                id: "reviewer-1".into(),
                client: ReviewerClient::Codex,
                display_label: "Codex reviewer".into(),
                access: ReviewerAccess::all(),
                credential_hash: [7; 32],
                created_at: 10,
            })
            .unwrap();
        let value = serde_json::to_value(connection).unwrap();
        assert_eq!(value["client"], "codex");
        assert_eq!(value["access"]["document_scope"], "all");
        assert!(value.get("credential_hash").is_none());
    }

    #[test]
    fn current_scope_allows_only_its_document() {
        let access = ReviewerAccess::current("doc-a");
        assert!(access.allows("doc-a"));
        assert!(!access.allows("doc-b"));
    }
}
