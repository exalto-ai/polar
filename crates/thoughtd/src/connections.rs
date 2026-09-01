//! Reviewer credentials and per-tool scope checks.

use crate::direct_edit::{
    DirectEditAccess, DirectEditDenial, DirectEditGrant, DirectEditKey, DirectEditRegistry,
    DirectEditRequestOutcome, ReviewerSnapshot,
};
use crate::discovery;
use sha2::{Digest, Sha256};
use std::fs::OpenOptions;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::time::Instant;
use thought_mcp::{
    CreateReviewerConnection, ReviewerAccess, ReviewerClient, ReviewerConnection,
    ReviewerConnectionStatus, UpdateReviewerConnection, Workspace,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthenticatedPrincipal {
    Internal,
    Reviewer {
        connection_id: String,
        credential_hash: [u8; 32],
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewerOperation {
    Read,
    Suggest,
    Edit,
    Create,
    Trash,
}

pub struct AuthorizedRequest {
    connection: Option<ReviewerConnection>,
}

impl AuthorizedRequest {
    pub fn connection(&self) -> Option<&ReviewerConnection> {
        self.connection.as_ref()
    }

    pub fn selected_document(&self) -> Option<&str> {
        self.connection
            .as_ref()
            .and_then(|connection| connection.access.document_id.as_deref())
    }
}

#[derive(Debug)]
pub enum RegistryError {
    InvalidInput(String),
    PermissionDenied(String),
    Credential(std::io::Error),
    Workspace(thought_mcp::WorkspaceError),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::InvalidInput(message) | Self::PermissionDenied(message) => {
                formatter.write_str(message)
            }
            Self::Credential(error) => write!(formatter, "reviewer credential: {error}"),
            Self::Workspace(error) => write!(formatter, "{error}"),
        }
    }
}

impl std::error::Error for RegistryError {}

impl From<std::io::Error> for RegistryError {
    fn from(error: std::io::Error) -> Self {
        Self::Credential(error)
    }
}

impl From<thought_mcp::WorkspaceError> for RegistryError {
    fn from(error: thought_mcp::WorkspaceError) -> Self {
        Self::Workspace(error)
    }
}

#[derive(Clone)]
pub struct CredentialFiles {
    directory: PathBuf,
}

impl CredentialFiles {
    pub fn platform() -> Self {
        Self::new(discovery::home().join("reviewer-credentials"))
    }

    pub fn new(directory: impl Into<PathBuf>) -> Self {
        Self {
            directory: directory.into(),
        }
    }

    pub fn read(&self, connection_id: &str) -> std::io::Result<String> {
        let mut value = String::new();
        OpenOptions::new()
            .read(true)
            .open(self.path(connection_id)?)?
            .read_to_string(&mut value)?;
        let value = value.trim().to_string();
        if !valid_secret(&value) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "stored reviewer credential is invalid",
            ));
        }
        Ok(value)
    }

    pub fn write(&self, connection_id: &str, value: &str) -> std::io::Result<()> {
        if !valid_secret(value) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "reviewer credential is invalid",
            ));
        }
        create_private_directory(&self.directory)?;
        let path = self.path(connection_id)?;
        let temporary = self.directory.join(format!(".{connection_id}.tmp"));
        let mut options = OpenOptions::new();
        options.create(true).truncate(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&temporary)?;
        file.write_all(value.as_bytes())?;
        file.sync_all()?;
        std::fs::rename(temporary, path)
    }

    pub fn remove(&self, connection_id: &str) -> std::io::Result<()> {
        match std::fs::remove_file(self.path(connection_id)?) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn path(&self, connection_id: &str) -> std::io::Result<PathBuf> {
        if !valid_connection_id(connection_id) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "invalid reviewer connection id",
            ));
        }
        Ok(self.directory.join(format!("{connection_id}.token")))
    }
}

fn create_private_directory(path: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

pub struct ConnectionRegistry {
    workspace: Arc<Workspace>,
    credentials: CredentialFiles,
    credential_updates: Mutex<()>,
    direct_edits: DirectEditRegistry,
}

impl ConnectionRegistry {
    pub fn platform(workspace: Arc<Workspace>) -> Self {
        Self::new(workspace, CredentialFiles::platform())
    }

    pub fn new(workspace: Arc<Workspace>, credentials: CredentialFiles) -> Self {
        Self {
            workspace,
            credentials,
            credential_updates: Mutex::new(()),
            direct_edits: DirectEditRegistry::default(),
        }
    }

    pub fn internal_principal(&self) -> AuthenticatedPrincipal {
        AuthenticatedPrincipal::Internal
    }

    pub fn list(&self) -> Result<Vec<ReviewerConnection>, RegistryError> {
        Ok(self.workspace.list_reviewer_connections()?)
    }

    pub fn connection(&self, id: &str) -> Result<ReviewerConnection, RegistryError> {
        Ok(self.workspace.reviewer_connection(id)?)
    }

    pub fn create(
        &self,
        client: ReviewerClient,
        display_label: String,
        access: ReviewerAccess,
        at: i64,
    ) -> Result<ReviewerConnection, RegistryError> {
        let _guard = self
            .credential_updates
            .lock()
            .map_err(|_| RegistryError::InvalidInput("credential store unavailable".into()))?;
        let display_label = valid_label(display_label)?;
        access.validate()?;
        let id = discovery::random_token()?[..20].to_string();
        let credential = discovery::random_token()?;
        self.credentials.write(&id, &credential)?;
        let created = self
            .workspace
            .create_reviewer_connection(&CreateReviewerConnection {
                id: id.clone(),
                client,
                display_label,
                access,
                credential_hash: credential_hash(credential.as_bytes()),
                created_at: at,
            });
        if created.is_err() {
            let _ = self.credentials.remove(&id);
        }
        Ok(created?)
    }

    pub fn update(
        &self,
        id: &str,
        expected_revision: i64,
        display_label: String,
        access: ReviewerAccess,
        at: i64,
    ) -> Result<ReviewerConnection, RegistryError> {
        let _guard = self
            .credential_updates
            .lock()
            .map_err(|_| RegistryError::InvalidInput("connection state is unavailable".into()))?;
        let display_label = valid_label(display_label)?;
        access.validate()?;
        let connection = self.workspace.update_reviewer_connection(
            id,
            &UpdateReviewerConnection {
                expected_revision,
                display_label,
                access,
                updated_at: at,
            },
        )?;
        let _ = self.direct_edits.revoke_connection(id);
        Ok(connection)
    }

    pub fn reset(
        &self,
        id: &str,
        expected_revision: i64,
        at: i64,
    ) -> Result<ReviewerConnection, RegistryError> {
        let _guard = self
            .credential_updates
            .lock()
            .map_err(|_| RegistryError::InvalidInput("credential store unavailable".into()))?;
        let previous = self.credentials.read(id)?;
        let replacement = discovery::random_token()?;
        self.credentials.write(id, &replacement)?;
        match self.workspace.rotate_reviewer_credential(
            id,
            expected_revision,
            &credential_hash(replacement.as_bytes()),
            at,
        ) {
            Ok(connection) => {
                let _ = self.direct_edits.revoke_connection(id);
                Ok(connection)
            }
            Err(error) => {
                let _ = self.credentials.write(id, &previous);
                Err(error.into())
            }
        }
    }

    pub fn revoke(
        &self,
        id: &str,
        expected_revision: i64,
        at: i64,
    ) -> Result<ReviewerConnection, RegistryError> {
        let _guard = self
            .credential_updates
            .lock()
            .map_err(|_| RegistryError::InvalidInput("credential store unavailable".into()))?;
        let connection = self
            .workspace
            .revoke_reviewer_connection(id, expected_revision, at)?;
        let _ = self.direct_edits.revoke_connection(id);
        let _ = self.credentials.remove(id);
        Ok(connection)
    }

    pub fn authenticate(
        &self,
        bearer: &str,
        at: i64,
    ) -> Result<Option<AuthenticatedPrincipal>, RegistryError> {
        if !valid_secret(bearer) {
            return Ok(None);
        }
        let credential_hash = credential_hash(bearer.as_bytes());
        let Some(connection) = self
            .workspace
            .reviewer_connection_by_credential_hash(&credential_hash)?
        else {
            return Ok(None);
        };
        self.workspace.mark_reviewer_seen(&connection.id, at)?;
        Ok(Some(AuthenticatedPrincipal::Reviewer {
            connection_id: connection.id,
            credential_hash,
        }))
    }

    pub fn authorize(
        &self,
        principal: &AuthenticatedPrincipal,
        operation: ReviewerOperation,
        document_id: Option<&str>,
    ) -> Result<AuthorizedRequest, RegistryError> {
        self.authorize_session(principal, operation, document_id, None)
    }

    pub fn authorize_session(
        &self,
        principal: &AuthenticatedPrincipal,
        operation: ReviewerOperation,
        document_id: Option<&str>,
        session_id: Option<&str>,
    ) -> Result<AuthorizedRequest, RegistryError> {
        match principal {
            AuthenticatedPrincipal::Internal => Ok(AuthorizedRequest { connection: None }),
            AuthenticatedPrincipal::Reviewer {
                connection_id,
                credential_hash,
            } => {
                let connection = self
                    .workspace
                    .reviewer_connection_by_credential_hash(credential_hash)?
                    .filter(|connection| connection.id == *connection_id)
                    .ok_or_else(|| {
                        RegistryError::PermissionDenied(
                            "reviewer connection was reset or removed".into(),
                        )
                    })?;
                if connection.status != ReviewerConnectionStatus::Configured
                    || document_id.is_some_and(|id| !connection.allows_document(id))
                {
                    return Err(RegistryError::PermissionDenied(
                        "document is outside this reviewer connection".into(),
                    ));
                }
                match operation {
                    ReviewerOperation::Read | ReviewerOperation::Suggest => {}
                    ReviewerOperation::Edit => {
                        let document_id = document_id.ok_or_else(|| {
                            RegistryError::PermissionDenied(
                                "direct editing requires a document".into(),
                            )
                        })?;
                        let session_id = valid_session_id(session_id)?;
                        let key = DirectEditKey {
                            connection_id: connection_id.clone(),
                            credential_hash: *credential_hash,
                            session_id: session_id.to_string(),
                            document_id: document_id.to_string(),
                        };
                        if !self
                            .direct_edits
                            .is_active(&key, Instant::now())
                            .map_err(|message| RegistryError::PermissionDenied(message.into()))?
                        {
                            return Err(RegistryError::PermissionDenied(
                                "direct editing requires an active user-approved grant for this document; call request_direct_edit first".into(),
                            ));
                        }
                    }
                    ReviewerOperation::Create | ReviewerOperation::Trash => {
                        return Err(RegistryError::PermissionDenied(
                            "reviewer connections cannot create, trash, or restore documents"
                                .into(),
                        ));
                    }
                }
                Ok(AuthorizedRequest {
                    connection: Some(connection),
                })
            }
        }
    }

    pub fn request_direct_edit(
        &self,
        principal: &AuthenticatedPrincipal,
        document_id: &str,
        session_id: &str,
        reported_model: Option<&str>,
        at: i64,
    ) -> Result<DirectEditRequestOutcome, RegistryError> {
        let _guard = self
            .credential_updates
            .lock()
            .map_err(|_| RegistryError::InvalidInput("connection state is unavailable".into()))?;
        let authorized = self.authorize_session(
            principal,
            ReviewerOperation::Read,
            Some(document_id),
            Some(session_id),
        )?;
        let connection = authorized.connection().ok_or_else(|| {
            RegistryError::PermissionDenied(
                "the native editor does not need a direct-edit grant".into(),
            )
        })?;
        let document_title = safe_document_title(&self.workspace.read_document(document_id)?.title);
        let model = valid_reported_model(reported_model)?;
        self.note_reported_model(principal, model.as_deref())?;
        let key = direct_edit_key(principal, document_id, session_id)?;
        let request_id = discovery::random_token()?[..20].to_string();
        self.direct_edits
            .request(
                key,
                ReviewerSnapshot {
                    display_label: connection.display_label.clone(),
                    client: connection.client,
                    reported_model: model,
                    document_title,
                },
                request_id,
                Instant::now(),
                at,
            )
            .map_err(|message| RegistryError::InvalidInput(message.into()))
    }

    pub fn direct_edit_access(&self, document_id: &str) -> Result<DirectEditAccess, RegistryError> {
        self.direct_edits
            .access(document_id, Instant::now())
            .map_err(|message| RegistryError::InvalidInput(message.into()))
    }

    pub fn all_direct_edit_access(&self) -> Result<DirectEditAccess, RegistryError> {
        self.direct_edits
            .all_access(Instant::now())
            .map_err(|message| RegistryError::InvalidInput(message.into()))
    }

    pub fn approve_direct_edit(
        &self,
        document_id: &str,
        request_id: &str,
        at: i64,
    ) -> Result<DirectEditGrant, RegistryError> {
        let _guard = self
            .credential_updates
            .lock()
            .map_err(|_| RegistryError::InvalidInput("connection state is unavailable".into()))?;
        let identity = self
            .direct_edits
            .pending_identity(document_id, request_id, Instant::now())
            .map_err(|message| RegistryError::InvalidInput(message.into()))?;
        let connection = self
            .workspace
            .reviewer_connection_by_credential_hash(&identity.credential_hash)?
            .filter(|connection| connection.id == identity.connection_id)
            .ok_or_else(|| {
                RegistryError::PermissionDenied("reviewer connection was reset or removed".into())
            })?;
        if !connection.allows_document(document_id) {
            let _ = self.direct_edits.revoke_connection(&identity.connection_id);
            return Err(RegistryError::PermissionDenied(
                "document is outside this reviewer connection".into(),
            ));
        }
        let grant_id = discovery::random_token()?[..20].to_string();
        self.direct_edits
            .approve(document_id, request_id, grant_id, Instant::now(), at)
            .map_err(|message| RegistryError::InvalidInput(message.into()))
    }

    pub fn deny_direct_edit(
        &self,
        document_id: &str,
        request_id: &str,
    ) -> Result<DirectEditDenial, RegistryError> {
        let _guard = self
            .credential_updates
            .lock()
            .map_err(|_| RegistryError::InvalidInput("connection state is unavailable".into()))?;
        self.direct_edits
            .deny(document_id, request_id, Instant::now())
            .map_err(|message| RegistryError::InvalidInput(message.into()))
    }

    pub fn revoke_direct_edit(
        &self,
        document_id: &str,
        grant_id: &str,
    ) -> Result<DirectEditGrant, RegistryError> {
        let _guard = self
            .credential_updates
            .lock()
            .map_err(|_| RegistryError::InvalidInput("connection state is unavailable".into()))?;
        self.direct_edits
            .revoke(document_id, grant_id, Instant::now())
            .map_err(|message| RegistryError::InvalidInput(message.into()))
    }

    /// Ends every pending request and active grant bound to one daemon-issued
    /// MCP transport session. The session id is never supplied by a tool
    /// caller; the streamable HTTP transport owns it.
    pub fn revoke_mcp_session(&self, session_id: &str) -> Result<(), RegistryError> {
        let session_id = valid_session_id(Some(session_id))?;
        self.direct_edits
            .revoke_session(session_id)
            .map_err(|message| RegistryError::InvalidInput(message.into()))
    }

    pub fn note_reported_model(
        &self,
        principal: &AuthenticatedPrincipal,
        model: Option<&str>,
    ) -> Result<(), RegistryError> {
        if let AuthenticatedPrincipal::Reviewer { connection_id, .. } = principal {
            let model = valid_reported_model(model)?;
            self.workspace
                .update_reviewer_reported_model(connection_id, model.as_deref())?;
        }
        Ok(())
    }
}

fn direct_edit_key(
    principal: &AuthenticatedPrincipal,
    document_id: &str,
    session_id: &str,
) -> Result<DirectEditKey, RegistryError> {
    let AuthenticatedPrincipal::Reviewer {
        connection_id,
        credential_hash,
    } = principal
    else {
        return Err(RegistryError::PermissionDenied(
            "the native editor does not need a direct-edit grant".into(),
        ));
    };
    let session_id = valid_session_id(Some(session_id))?;
    Ok(DirectEditKey {
        connection_id: connection_id.clone(),
        credential_hash: *credential_hash,
        session_id: session_id.to_string(),
        document_id: document_id.to_string(),
    })
}

fn valid_session_id(value: Option<&str>) -> Result<&str, RegistryError> {
    value
        .filter(|value| !value.is_empty() && value.len() <= 256)
        .ok_or_else(|| {
            RegistryError::PermissionDenied(
                "direct editing requires the daemon-issued MCP session".into(),
            )
        })
}

fn valid_reported_model(value: Option<&str>) -> Result<Option<String>, RegistryError> {
    let value = value.map(str::trim).filter(|value| !value.is_empty());
    if value.is_some_and(|value| value.len() > 256 || value.chars().any(char::is_control)) {
        return Err(RegistryError::InvalidInput(
            "reported model is invalid".into(),
        ));
    }
    Ok(value.map(str::to_string))
}

fn safe_document_title(value: &str) -> String {
    let normalized = value
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let title: String = normalized.chars().take(120).collect();
    if title.is_empty() {
        "Untitled".into()
    } else {
        title
    }
}

fn valid_label(value: String) -> Result<String, RegistryError> {
    let value = value.trim().to_string();
    if value.is_empty() || value.len() > 80 {
        return Err(RegistryError::InvalidInput(
            "reviewer name must be 1 to 80 characters".into(),
        ));
    }
    Ok(value)
}

pub fn valid_connection_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'-')
}

fn valid_secret(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn credential_hash(value: &[u8]) -> [u8; 32] {
    Sha256::digest(value).into()
}

pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn direct_edit_document_titles_are_safe_editor_metadata() {
        assert_eq!(
            safe_document_title("  Quarterly\n\0\tplan  "),
            "Quarterly plan"
        );
        assert_eq!(safe_document_title("\0\n"), "Untitled");
        assert_eq!(safe_document_title(&"x".repeat(121)).chars().count(), 120);
    }

    #[test]
    fn reset_invalidates_the_previous_credential() {
        let workspace = Arc::new(Workspace::open_in_memory().unwrap());
        let directory = tempfile::tempdir().unwrap();
        let credentials = CredentialFiles::new(directory.path());
        let registry = ConnectionRegistry::new(workspace, credentials.clone());
        let connection = registry
            .create(
                ReviewerClient::Codex,
                "Reviewer".into(),
                ReviewerAccess::all(),
                10,
            )
            .unwrap();
        let previous = credentials.read(&connection.id).unwrap();
        let previous_principal = registry.authenticate(&previous, 15).unwrap().unwrap();
        registry
            .note_reported_model(&previous_principal, Some("reported-model"))
            .unwrap();
        let reset = registry
            .reset(&connection.id, connection.revision, 20)
            .unwrap();
        let replacement = credentials.read(&connection.id).unwrap();

        assert_ne!(previous, replacement);
        assert!(registry.authenticate(&previous, 30).unwrap().is_none());
        assert!(registry.authenticate(&replacement, 30).unwrap().is_some());
        assert_eq!(reset.revision, connection.revision + 1);
        assert_eq!(reset.last_seen_at, None);
        assert_eq!(reset.reported_model, None);
    }

    #[cfg(unix)]
    #[test]
    fn credential_files_are_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().unwrap();
        let credentials = CredentialFiles::new(directory.path().join("credentials"));
        credentials.write("reviewer-1", &"a".repeat(64)).unwrap();
        let file = directory.path().join("credentials/reviewer-1.token");
        assert_eq!(
            std::fs::metadata(file).unwrap().permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            std::fs::metadata(directory.path().join("credentials"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
    }

    #[test]
    fn current_scope_blocks_other_documents() {
        let workspace = Arc::new(Workspace::open_in_memory().unwrap());
        let first = workspace
            .create_document("First", &thought_mcp::ActorRef::editor())
            .unwrap();
        let second = workspace
            .create_document("Second", &thought_mcp::ActorRef::editor())
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let credentials = CredentialFiles::new(directory.path());
        let registry = ConnectionRegistry::new(workspace, credentials.clone());
        let connection = registry
            .create(
                ReviewerClient::ClaudeCode,
                "Reviewer".into(),
                ReviewerAccess::current(first.doc_id.clone()),
                10,
            )
            .unwrap();
        let credential = credentials.read(&connection.id).unwrap();
        let principal = registry.authenticate(&credential, 11).unwrap().unwrap();
        assert!(
            registry
                .authorize(&principal, ReviewerOperation::Read, Some(&first.doc_id))
                .is_ok()
        );
        assert!(
            registry
                .authorize(&principal, ReviewerOperation::Suggest, Some(&first.doc_id))
                .is_ok()
        );
        assert!(
            registry
                .authorize(&principal, ReviewerOperation::Read, Some(&second.doc_id))
                .is_err()
        );
    }

    #[test]
    fn direct_edit_grant_is_session_bound_and_revoked_by_connection_changes() {
        let workspace = Arc::new(Workspace::open_in_memory().unwrap());
        let document = workspace
            .create_document("Draft", &thought_mcp::ActorRef::editor())
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let credentials = CredentialFiles::new(directory.path());
        let registry = ConnectionRegistry::new(workspace, credentials.clone());
        let connection = registry
            .create(
                ReviewerClient::Codex,
                "Reviewer".into(),
                ReviewerAccess::current(document.doc_id.clone()),
                10,
            )
            .unwrap();
        let credential = credentials.read(&connection.id).unwrap();
        let principal = registry.authenticate(&credential, 11).unwrap().unwrap();

        let request = registry
            .request_direct_edit(
                &principal,
                &document.doc_id,
                "daemon-session-1",
                Some("reported-model"),
                12,
            )
            .unwrap();
        let crate::direct_edit::DirectEditRequestOutcome::Pending { request } = request else {
            panic!("expected a pending request")
        };
        assert_eq!(request.document_title, "Draft");
        registry
            .approve_direct_edit(&document.doc_id, &request.request_id, 13)
            .unwrap();

        assert!(
            registry
                .authorize_session(
                    &principal,
                    ReviewerOperation::Edit,
                    Some(&document.doc_id),
                    Some("daemon-session-1"),
                )
                .is_ok()
        );
        assert!(
            registry
                .authorize_session(
                    &principal,
                    ReviewerOperation::Edit,
                    Some(&document.doc_id),
                    Some("daemon-session-2"),
                )
                .is_err()
        );
        assert!(
            registry
                .authorize_session(
                    &principal,
                    ReviewerOperation::Edit,
                    Some(&document.doc_id),
                    None,
                )
                .is_err()
        );
        let forged_principal = match &principal {
            AuthenticatedPrincipal::Reviewer { connection_id, .. } => {
                AuthenticatedPrincipal::Reviewer {
                    connection_id: connection_id.clone(),
                    credential_hash: [9; 32],
                }
            }
            AuthenticatedPrincipal::Internal => unreachable!(),
        };
        assert!(
            registry
                .authorize_session(
                    &forged_principal,
                    ReviewerOperation::Edit,
                    Some(&document.doc_id),
                    Some("daemon-session-1"),
                )
                .is_err()
        );
        assert!(
            registry
                .authorize_session(
                    &principal,
                    ReviewerOperation::Create,
                    None,
                    Some("daemon-session-1"),
                )
                .is_err()
        );

        let updated = registry
            .update(
                &connection.id,
                connection.revision,
                "Renamed reviewer".into(),
                ReviewerAccess::current(document.doc_id.clone()),
                14,
            )
            .unwrap();
        assert!(
            registry
                .authorize_session(
                    &principal,
                    ReviewerOperation::Edit,
                    Some(&document.doc_id),
                    Some("daemon-session-1"),
                )
                .is_err()
        );

        let request = registry
            .request_direct_edit(
                &principal,
                &document.doc_id,
                "daemon-session-1",
                Some("reported-model"),
                15,
            )
            .unwrap();
        let crate::direct_edit::DirectEditRequestOutcome::Pending { request } = request else {
            panic!("expected a pending request")
        };
        registry
            .approve_direct_edit(&document.doc_id, &request.request_id, 16)
            .unwrap();
        let reset = registry
            .reset(&connection.id, updated.revision, 17)
            .unwrap();
        assert!(registry.all_direct_edit_access().unwrap().grants.is_empty());
        assert!(
            registry
                .authorize_session(
                    &principal,
                    ReviewerOperation::Edit,
                    Some(&document.doc_id),
                    Some("daemon-session-1"),
                )
                .is_err()
        );

        let replacement = credentials.read(&connection.id).unwrap();
        let replacement_principal = registry.authenticate(&replacement, 18).unwrap().unwrap();
        let request = registry
            .request_direct_edit(
                &replacement_principal,
                &document.doc_id,
                "daemon-session-2",
                None,
                19,
            )
            .unwrap();
        let crate::direct_edit::DirectEditRequestOutcome::Pending { request } = request else {
            panic!("expected a pending request")
        };
        registry
            .approve_direct_edit(&document.doc_id, &request.request_id, 20)
            .unwrap();
        registry.revoke(&connection.id, reset.revision, 21).unwrap();
        assert!(registry.all_direct_edit_access().unwrap().grants.is_empty());
    }
}
