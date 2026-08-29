//! Transport-independent reviewer connection models.
//!
//! The editor API may serialize [`ReviewerConnection`], but credential
//! material only ever enters the native workspace methods as fixed-size
//! hashes. It is intentionally absent from every serializable type here.

use crate::{ActorRef, MutationContext, Workspace, WorkspaceError};
use serde::{Deserialize, Serialize};
use thought_store::{
    NewReviewerConnectionInput, ReviewerConnectionRow, ReviewerConnectionUpdateInput,
};

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

    /// Consumer-facing name for the route the person chose during setup.
    /// This is configuration, not evidence that the named app made a call.
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
    type Error = ReviewerConnectionModelError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "chatgpt" => Ok(Self::Chatgpt),
            "codex" => Ok(Self::Codex),
            "claude_desktop" => Ok(Self::ClaudeDesktop),
            "claude_code" => Ok(Self::ClaudeCode),
            _ => Err(ReviewerConnectionModelError::invalid_value("client", value)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewerProvider {
    Openai,
    Anthropic,
}

impl ReviewerProvider {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Openai => "openai",
            Self::Anthropic => "anthropic",
        }
    }
}

impl TryFrom<&str> for ReviewerProvider {
    type Error = ReviewerConnectionModelError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "openai" => Ok(Self::Openai),
            "anthropic" => Ok(Self::Anthropic),
            _ => Err(ReviewerConnectionModelError::invalid_value(
                "provider", value,
            )),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReviewerConnectionStatus {
    Configured,
    Connected,
    Disconnected,
    Failed,
    Revoked,
}

impl ReviewerConnectionStatus {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Configured => "configured",
            Self::Connected => "connected",
            Self::Disconnected => "disconnected",
            Self::Failed => "failed",
            Self::Revoked => "revoked",
        }
    }

    pub const fn is_active(self) -> bool {
        !matches!(self, Self::Revoked)
    }
}

impl TryFrom<&str> for ReviewerConnectionStatus {
    type Error = ReviewerConnectionModelError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "configured" => Ok(Self::Configured),
            "connected" => Ok(Self::Connected),
            "disconnected" => Ok(Self::Disconnected),
            "failed" => Ok(Self::Failed),
            "revoked" => Ok(Self::Revoked),
            _ => Err(ReviewerConnectionModelError::invalid_value("status", value)),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ReviewerDocumentScope {
    #[serde(rename = "current")]
    Selected,
    #[serde(rename = "all")]
    All,
}

impl ReviewerDocumentScope {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Selected => "selected",
            Self::All => "all",
        }
    }
}

impl TryFrom<&str> for ReviewerDocumentScope {
    type Error = ReviewerConnectionModelError;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        match value {
            "selected" => Ok(Self::Selected),
            "all" => Ok(Self::All),
            _ => Err(ReviewerConnectionModelError::invalid_value(
                "document_scope",
                value,
            )),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewerPermissions {
    pub document_scope: ReviewerDocumentScope,
    pub can_read: bool,
    pub can_edit: bool,
    pub can_create: bool,
    pub can_trash: bool,
    pub document_ids: Vec<String>,
}

impl ReviewerPermissions {
    pub fn all(can_edit: bool, can_create: bool, can_trash: bool) -> Self {
        Self {
            document_scope: ReviewerDocumentScope::All,
            can_read: true,
            can_edit,
            can_create,
            can_trash,
            document_ids: Vec::new(),
        }
    }

    pub fn selected(
        document_ids: impl IntoIterator<Item = String>,
        can_edit: bool,
        can_trash: bool,
    ) -> Self {
        Self {
            document_scope: ReviewerDocumentScope::Selected,
            can_read: true,
            can_edit,
            can_create: false,
            can_trash,
            document_ids: document_ids.into_iter().collect(),
        }
    }

    /// Return a storage-ready permission set. Reviewer connections are
    /// durable, so this release deliberately narrows every durable route to
    /// read-only access. Direct writes require a separate per-session grant
    /// that does not exist yet.
    pub fn normalized(&self) -> Result<Self, ReviewerConnectionModelError> {
        if !self.can_read {
            return Err(ReviewerConnectionModelError::InvalidPermissions(
                "reviewer connections must retain read access".to_string(),
            ));
        }

        let mut normalized = self.clone();
        normalized.can_edit = false;
        normalized.can_create = false;
        normalized.can_trash = false;
        match normalized.document_scope {
            ReviewerDocumentScope::All => {
                if !normalized.document_ids.is_empty() {
                    return Err(ReviewerConnectionModelError::InvalidPermissions(
                        "all-document access cannot include selected document ids".to_string(),
                    ));
                }
            }
            ReviewerDocumentScope::Selected => {
                if normalized
                    .document_ids
                    .iter()
                    .any(|id| id.trim().is_empty())
                {
                    return Err(ReviewerConnectionModelError::InvalidPermissions(
                        "selected document ids cannot be empty".to_string(),
                    ));
                }
                normalized.document_ids.sort();
                normalized.document_ids.dedup();
                if normalized.document_ids.len() != 1 {
                    return Err(ReviewerConnectionModelError::InvalidPermissions(
                        "current-document access requires exactly one document".to_string(),
                    ));
                }
            }
        }
        Ok(normalized)
    }

    pub fn allows_document(&self, document_id: &str) -> bool {
        match self.document_scope {
            ReviewerDocumentScope::All => true,
            ReviewerDocumentScope::Selected => self
                .document_ids
                .iter()
                .any(|candidate| candidate == document_id),
        }
    }
}

/// Safe representation returned to the editor and used by the daemon's
/// authorization boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewerConnection {
    pub id: String,
    pub client: ReviewerClient,
    pub provider: ReviewerProvider,
    pub display_label: String,
    pub status: ReviewerConnectionStatus,
    pub permissions: ReviewerPermissions,
    pub revision: i64,
    pub created_at: i64,
    pub first_connected_at: Option<i64>,
    pub last_seen_at: Option<i64>,
    pub failure_code: Option<String>,
    pub revoked_at: Option<i64>,
    pub reported_model: Option<String>,
}

impl ReviewerConnection {
    pub fn allows_document(&self, document_id: &str) -> bool {
        self.status.is_active() && self.permissions.allows_document(document_id)
    }

    /// Build the stable actor identity used by update attribution.
    pub fn reported_actor(
        &self,
        session_id: Option<&str>,
        reported_model: Option<&str>,
    ) -> ActorRef {
        ActorRef::reviewer(&self.id, &self.display_label, reported_model, session_id)
    }

    /// Freeze the configured route into a reported MCP source while grouping
    /// all later calls by the same durable connection id. The configured app
    /// is a routing label, not proof that the named app made this call.
    pub fn reported_mutation_context(&self, reported_model: Option<&str>) -> MutationContext {
        let _ = reported_model;
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
    type Error = ReviewerConnectionModelError;

    fn try_from(row: ReviewerConnectionRow) -> Result<Self, Self::Error> {
        let client = ReviewerClient::try_from(row.client.as_str())?;
        let provider = ReviewerProvider::try_from(row.provider.as_str())?;
        if provider != client.provider() {
            return Err(ReviewerConnectionModelError::InvalidProviderForClient {
                client,
                provider,
            });
        }
        let permissions = ReviewerPermissions {
            document_scope: ReviewerDocumentScope::try_from(row.document_scope.as_str())?,
            can_read: row.can_read,
            can_edit: row.can_edit,
            can_create: row.can_create,
            can_trash: row.can_trash,
            document_ids: row.document_ids,
        }
        .normalized()?;

        Ok(Self {
            id: row.id,
            client,
            provider,
            display_label: row.display_label,
            status: ReviewerConnectionStatus::try_from(row.status.as_str())?,
            permissions,
            revision: row.revision,
            created_at: row.created_at,
            first_connected_at: row.first_connected_at,
            last_seen_at: row.last_seen_at,
            failure_code: row.failure_code,
            revoked_at: row.revoked_at,
            reported_model: row.reported_model,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReviewerConnectionModelError {
    InvalidStoredValue {
        field: &'static str,
        value: String,
    },
    InvalidProviderForClient {
        client: ReviewerClient,
        provider: ReviewerProvider,
    },
    InvalidPermissions(String),
}

impl ReviewerConnectionModelError {
    fn invalid_value(field: &'static str, value: &str) -> Self {
        Self::InvalidStoredValue {
            field,
            value: value.to_string(),
        }
    }
}

impl std::fmt::Display for ReviewerConnectionModelError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidStoredValue { field, value } => {
                write!(formatter, "unknown reviewer {field} `{value}`")
            }
            Self::InvalidProviderForClient { client, provider } => write!(
                formatter,
                "reviewer client `{}` cannot use provider `{}`",
                client.as_str(),
                provider.as_str()
            ),
            Self::InvalidPermissions(message) => {
                write!(formatter, "invalid reviewer permissions: {message}")
            }
        }
    }
}

impl std::error::Error for ReviewerConnectionModelError {}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreateReviewerConnection {
    pub id: String,
    pub client: ReviewerClient,
    pub display_label: String,
    pub permissions: ReviewerPermissions,
    pub credential_hash: [u8; 32],
    pub credential_expires_at: Option<i64>,
    pub created_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UpdateReviewerConnection {
    pub expected_revision: i64,
    pub display_label: String,
    pub permissions: ReviewerPermissions,
    pub updated_at: i64,
}

impl Workspace {
    pub fn create_reviewer_connection(
        &self,
        input: &CreateReviewerConnection,
    ) -> Result<ReviewerConnection, WorkspaceError> {
        let permissions = input.permissions.normalized()?;
        let row = self.with_store(|store| {
            store.create_reviewer_connection(&NewReviewerConnectionInput {
                id: &input.id,
                client: input.client.as_str(),
                provider: input.client.provider().as_str(),
                display_label: &input.display_label,
                document_scope: permissions.document_scope.as_str(),
                can_edit: permissions.can_edit,
                can_create: permissions.can_create,
                can_trash: permissions.can_trash,
                credential_hash: &input.credential_hash,
                credential_expires_at: input.credential_expires_at,
                document_ids: &permissions.document_ids,
                created_at: input.created_at,
            })
        })?;
        Ok(row.try_into()?)
    }

    pub fn reviewer_connection(&self, id: &str) -> Result<ReviewerConnection, WorkspaceError> {
        let row = self.with_store(|store| store.reviewer_connection(id))?;
        Ok(row.try_into()?)
    }

    /// Current non-secret credential generation for native lifecycle guards.
    /// It is intentionally not part of the serializable connection model.
    pub fn reviewer_credential_version(&self, id: &str) -> Result<i64, WorkspaceError> {
        Ok(self.with_store(|store| store.reviewer_credential_version(id))?)
    }

    pub fn list_reviewer_connections(
        &self,
        at: i64,
    ) -> Result<Vec<ReviewerConnection>, WorkspaceError> {
        self.with_store(|store| store.list_reviewer_connections(at))?
            .into_iter()
            .map(|row| row.try_into().map_err(WorkspaceError::from))
            .collect()
    }

    pub fn reviewer_connection_by_credential_hash(
        &self,
        credential_hash: &[u8; 32],
        at: i64,
    ) -> Result<Option<ReviewerConnection>, WorkspaceError> {
        self.with_store(|store| store.reviewer_connection_by_credential_hash(credential_hash, at))?
            .map(TryInto::try_into)
            .transpose()
            .map_err(WorkspaceError::from)
    }

    pub fn reviewer_pending_credential_matches(
        &self,
        id: &str,
        credential_hash: &[u8; 32],
    ) -> Result<Option<bool>, WorkspaceError> {
        Ok(self
            .with_store(|store| store.reviewer_pending_credential_matches(id, credential_hash))?)
    }

    pub fn reviewer_has_pending_credential(&self, id: &str) -> Result<bool, WorkspaceError> {
        Ok(self.with_store(|store| store.reviewer_has_pending_credential(id))?)
    }

    pub fn update_reviewer_connection(
        &self,
        id: &str,
        input: &UpdateReviewerConnection,
    ) -> Result<ReviewerConnection, WorkspaceError> {
        let permissions = input.permissions.normalized()?;
        let row = self.with_store(|store| {
            store.update_reviewer_connection(&ReviewerConnectionUpdateInput {
                id,
                expected_revision: input.expected_revision,
                display_label: &input.display_label,
                document_scope: permissions.document_scope.as_str(),
                can_edit: permissions.can_edit,
                can_create: permissions.can_create,
                can_trash: permissions.can_trash,
                document_ids: &permissions.document_ids,
                updated_at: input.updated_at,
            })
        })?;
        Ok(row.try_into()?)
    }

    pub fn begin_reviewer_credential_rotation(
        &self,
        id: &str,
        expected_revision: i64,
        pending_credential_hash: &[u8; 32],
        updated_at: i64,
    ) -> Result<(), WorkspaceError> {
        self.with_store(|store| {
            store.begin_reviewer_credential_rotation(
                id,
                expected_revision,
                pending_credential_hash,
                updated_at,
            )
        })?;
        Ok(())
    }

    pub fn finish_reviewer_credential_rotation(
        &self,
        id: &str,
        expected_revision: i64,
        updated_at: i64,
    ) -> Result<ReviewerConnection, WorkspaceError> {
        let row = self.with_store(|store| {
            store.finish_reviewer_credential_rotation(id, expected_revision, updated_at)
        })?;
        Ok(row.try_into()?)
    }

    pub fn cancel_reviewer_credential_rotation(
        &self,
        id: &str,
        expected_revision: i64,
        updated_at: i64,
    ) -> Result<(), WorkspaceError> {
        self.with_store(|store| {
            store.cancel_reviewer_credential_rotation(id, expected_revision, updated_at)
        })?;
        Ok(())
    }

    pub fn revoke_reviewer_connection(
        &self,
        id: &str,
        expected_revision: i64,
        revoked_at: i64,
    ) -> Result<ReviewerConnection, WorkspaceError> {
        let row = self.with_store(|store| {
            store.revoke_reviewer_connection(id, expected_revision, revoked_at)
        })?;
        Ok(row.try_into()?)
    }

    pub fn mark_reviewer_seen(
        &self,
        id: &str,
        seen_at: i64,
        lease_expires_at: i64,
        reported_model: Option<&str>,
    ) -> Result<ReviewerConnection, WorkspaceError> {
        let row = self.with_store(|store| {
            store.mark_reviewer_seen(id, seen_at, lease_expires_at, reported_model)
        })?;
        Ok(row.try_into()?)
    }

    pub fn mark_reviewer_disconnected(
        &self,
        id: &str,
        disconnected_at: i64,
    ) -> Result<ReviewerConnection, WorkspaceError> {
        let row = self.with_store(|store| store.mark_reviewer_disconnected(id, disconnected_at))?;
        Ok(row.try_into()?)
    }

    pub fn mark_reviewer_failed(
        &self,
        id: &str,
        failure_code: &str,
        failed_at: i64,
    ) -> Result<ReviewerConnection, WorkspaceError> {
        let row =
            self.with_store(|store| store.mark_reviewer_failed(id, failure_code, failed_at))?;
        Ok(row.try_into()?)
    }

    pub fn expire_reviewer_leases(&self, at: i64) -> Result<usize, WorkspaceError> {
        Ok(self.with_store(|store| store.expire_reviewer_leases(at))?)
    }

    pub fn update_reviewer_reported_model(
        &self,
        id: &str,
        reported_model: Option<&str>,
        updated_at: i64,
    ) -> Result<ReviewerConnection, WorkspaceError> {
        let row = self.with_store(|store| {
            store.update_reviewer_reported_model(id, reported_model, updated_at)
        })?;
        Ok(row.try_into()?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use thought_provenance::{Assurance, Ingress, SourceId};

    fn create_input() -> CreateReviewerConnection {
        CreateReviewerConnection {
            id: "reviewer-1".to_string(),
            client: ReviewerClient::ClaudeCode,
            display_label: "Claude reviewer".to_string(),
            permissions: ReviewerPermissions::all(true, true, false),
            credential_hash: [7; 32],
            credential_expires_at: None,
            created_at: 10,
        }
    }

    #[test]
    fn serialized_connection_is_safe_and_matches_the_editor_shape() {
        let workspace = Workspace::open_in_memory().unwrap();
        let connection = workspace
            .create_reviewer_connection(&create_input())
            .unwrap();
        let json = serde_json::to_value(&connection).unwrap();

        assert_eq!(json["client"], "claude-code");
        assert_eq!(json["provider"], "anthropic");
        assert_eq!(json["status"], "configured");
        assert_eq!(json["permissions"]["document_scope"], "all");
        assert_eq!(json["permissions"]["can_read"], true);
        assert!(json.get("credential_hash").is_none());
        assert!(json.get("credential_version").is_none());
        assert!(json.get("lease_expires_at").is_none());
    }

    #[test]
    fn reviewer_identity_and_provenance_stay_stable_across_renames() {
        let workspace = Workspace::open_in_memory().unwrap();
        let original = workspace
            .create_reviewer_connection(&create_input())
            .unwrap();
        let renamed = workspace
            .update_reviewer_connection(
                &original.id,
                &UpdateReviewerConnection {
                    expected_revision: original.revision,
                    display_label: "Research reviewer".to_string(),
                    permissions: original.permissions.clone(),
                    updated_at: 20,
                },
            )
            .unwrap();

        let before_actor = original.reported_actor(Some("turn-1"), Some("reported-a"));
        let after_actor = renamed.reported_actor(Some("turn-2"), Some("reported-b"));
        assert_eq!(before_actor.id, "reviewer:reviewer-1");
        assert_eq!(before_actor.id, after_actor.id);
        assert_ne!(before_actor.display_name, after_actor.display_name);

        let before_source = original
            .reported_mutation_context(Some("reported-a"))
            .source(SourceId(1));
        let after_source = renamed
            .reported_mutation_context(Some("reported-b"))
            .source(SourceId(2));
        assert_eq!(before_source.group_key, "mcp:connection:reviewer-1");
        assert_eq!(before_source.group_key, after_source.group_key);
        assert_eq!(before_source.label, "Configured for Claude Code (reported)");
        assert_eq!(after_source.label, before_source.label);
        assert_eq!(before_source.ingress, Ingress::Mcp);
        assert_eq!(before_source.assurance, Assurance::Reported);
    }

    #[test]
    fn event_identity_never_reuses_a_model_from_an_earlier_call() {
        let workspace = Workspace::open_in_memory().unwrap();
        workspace
            .create_reviewer_connection(&create_input())
            .unwrap();
        let connection = workspace
            .update_reviewer_reported_model("reviewer-1", Some("earlier-model"), 20)
            .unwrap();

        let actor = connection.reported_actor(Some("turn-2"), None);
        let context = connection.reported_mutation_context(None);

        assert_eq!(connection.reported_model.as_deref(), Some("earlier-model"));
        assert_eq!(actor.model, None);
        assert_eq!(
            context.source(SourceId(2)).group_key,
            "mcp:connection:reviewer-1"
        );
    }

    #[test]
    fn credential_lookup_and_revocation_use_sanitized_models() {
        let workspace = Workspace::open_in_memory().unwrap();
        let created = workspace
            .create_reviewer_connection(&create_input())
            .unwrap();
        assert_eq!(
            workspace
                .reviewer_connection_by_credential_hash(&[7; 32], 11)
                .unwrap()
                .unwrap()
                .id,
            created.id
        );

        let revoked = workspace
            .revoke_reviewer_connection(&created.id, created.revision, 12)
            .unwrap();
        assert_eq!(revoked.status, ReviewerConnectionStatus::Revoked);
        assert!(
            workspace
                .reviewer_connection_by_credential_hash(&[7; 32], 13)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn permissions_require_one_current_document_and_read_access() {
        let permissions =
            ReviewerPermissions::selected(["doc-a".to_string(), "doc-a".to_string()], true, false)
                .normalized()
                .unwrap();
        assert_eq!(permissions.document_ids, ["doc-a"]);
        assert!(permissions.allows_document("doc-a"));
        assert!(!permissions.allows_document("doc-c"));

        let legacy_write_flags = ReviewerPermissions {
            can_edit: true,
            can_create: true,
            can_trash: true,
            ..permissions.clone()
        };
        let narrowed = legacy_write_flags.normalized().unwrap();
        assert!(!narrowed.can_edit);
        assert!(!narrowed.can_create);
        assert!(!narrowed.can_trash);

        let multiple_documents =
            ReviewerPermissions::selected(["doc-a".to_string(), "doc-b".to_string()], true, false);
        assert!(matches!(
            multiple_documents.normalized(),
            Err(ReviewerConnectionModelError::InvalidPermissions(_))
        ));

        let without_required_read = ReviewerPermissions {
            can_read: false,
            ..permissions
        };
        assert!(matches!(
            without_required_read.normalized(),
            Err(ReviewerConnectionModelError::InvalidPermissions(_))
        ));
    }
}
