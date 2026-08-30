//! Native reviewer credential lifecycle and request authorization.
//!
//! Raw credentials live in the platform vault. SQLite contains only hashes,
//! while editor responses and setup commands contain only stable connection
//! IDs. Every tool re-reads the current connection under the authorization
//! gate so permission changes and revocation cannot race an in-flight edit.

use crate::discovery;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex, RwLock, RwLockReadGuard};
use thought_credentials::{CredentialError, CredentialStore};
use thought_mcp::{
    CreateReviewerConnection, ReviewerClient, ReviewerConnection, ReviewerConnectionStatus,
    ReviewerPermissions, UpdateReviewerConnection, Workspace, WorkspaceError,
};

const REVIEWER_LEASE_MS: i64 = 45_000;
const MAX_SESSION_BINDINGS: usize = 1_024;
const MAX_SESSION_BINDINGS_PER_PRINCIPAL: usize = 64;
const MAX_ACTIVE_INSTANCES: usize = 256;
const MAX_ACTIVE_INSTANCES_PER_CONNECTION: usize = 16;
pub const REVIEWER_INSTANCE_HEADER: &str = "thought-reviewer-instance";

type SessionCloser = Arc<dyn Fn(String) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthenticatedPrincipal {
    InternalEditor,
    Reviewer {
        connection_id: String,
        /// One-way hash of the credential that authenticated this request.
        /// Authorization revalidates it under the lifecycle gate so a reset
        /// cannot leave a pre-authenticated request usable.
        credential_hash: [u8; 32],
    },
}

impl AuthenticatedPrincipal {
    pub fn reviewer_id(&self) -> Option<&str> {
        match self {
            Self::InternalEditor => None,
            Self::Reviewer { connection_id, .. } => Some(connection_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct SessionBinding {
    principal: AuthenticatedPrincipal,
    instance_id: Option<String>,
    last_seen_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FailureReporterBinding {
    credential_version: i64,
    expires_at: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReviewerOperation {
    Read,
    Suggest,
    Edit,
    Create,
    Trash,
}

pub struct AuthorizedRequest<'a> {
    _gate: RwLockReadGuard<'a, ()>,
    connection: Option<ReviewerConnection>,
}

impl AuthorizedRequest<'_> {
    pub fn connection(&self) -> Option<&ReviewerConnection> {
        self.connection.as_ref()
    }

    pub fn allows_document(&self, document_id: &str) -> bool {
        self.connection
            .as_ref()
            .is_none_or(|connection| connection.allows_document(document_id))
    }
}

pub struct ConnectionRegistry {
    workspace: Arc<Workspace>,
    credentials: CredentialStore,
    authorization_gate: RwLock<()>,
    sessions: Mutex<HashMap<String, SessionBinding>>,
    active_instances: Mutex<HashMap<String, HashMap<String, i64>>>,
    failure_reporters: Mutex<HashMap<String, HashMap<String, FailureReporterBinding>>>,
    session_closer: Option<SessionCloser>,
}

impl ConnectionRegistry {
    pub fn platform(workspace: Arc<Workspace>) -> Self {
        Self::new(workspace, CredentialStore::platform(discovery::home()))
    }

    pub fn new(workspace: Arc<Workspace>, credentials: CredentialStore) -> Self {
        let registry = Self {
            workspace,
            credentials,
            authorization_gate: RwLock::new(()),
            sessions: Mutex::new(HashMap::new()),
            active_instances: Mutex::new(HashMap::new()),
            failure_reporters: Mutex::new(HashMap::new()),
            session_closer: None,
        };
        registry.reconcile_startup();
        registry
    }

    /// Arrange for authorization-binding cleanup to close the corresponding
    /// transport session too. The daemon installs this before sharing the
    /// registry; unit tests can keep the transport-independent default.
    pub fn with_session_closer(mut self, closer: impl Fn(String) + Send + Sync + 'static) -> Self {
        self.session_closer = Some(Arc::new(closer));
        self
    }

    pub fn internal_principal(&self) -> AuthenticatedPrincipal {
        AuthenticatedPrincipal::InternalEditor
    }

    pub fn authenticate_reviewer(
        &self,
        bearer: &str,
        at: i64,
    ) -> Result<Option<AuthenticatedPrincipal>, RegistryError> {
        let hash = credential_hash(bearer.as_bytes());
        Ok(self
            .workspace
            .reviewer_connection_by_credential_hash(&hash, at)?
            .map(|connection| AuthenticatedPrincipal::Reviewer {
                connection_id: connection.id,
                credential_hash: hash,
            }))
    }

    /// Successful authenticated traffic is the reviewer heartbeat. Anonymous
    /// failures never affect visible status.
    pub fn note_authenticated(
        &self,
        principal: &AuthenticatedPrincipal,
        instance_id: Option<&str>,
        at: i64,
    ) -> Result<(), RegistryError> {
        let _gate = self
            .authorization_gate
            .read()
            .map_err(|_| RegistryError::Unavailable)?;
        if let AuthenticatedPrincipal::Reviewer { connection_id, .. } = principal {
            self.revalidated_connection(principal, at)?;
            let credential_version = self.workspace.reviewer_credential_version(connection_id)?;
            // Use one lock order everywhere a process can change state. This
            // also refreshes the failure reporter only after this exact
            // credential was revalidated, so Reset cannot bless an old shim
            // for the replacement generation.
            let mut failure_reporters = self
                .failure_reporters
                .lock()
                .map_err(|_| RegistryError::Unavailable)?;
            prune_expired_failure_reporters(&mut failure_reporters, at);
            let mut instances = self
                .active_instances
                .lock()
                .map_err(|_| RegistryError::Unavailable)?;
            prune_expired_instances(&mut instances, at);
            if let Some(instance_id) = normalized_instance_id(instance_id) {
                let already_present = instances
                    .get(connection_id)
                    .is_some_and(|active| active.contains_key(instance_id));
                let connection_count = instances.get(connection_id).map_or(0, HashMap::len);
                let total_count = active_instance_count(&instances);
                if !already_present
                    && (connection_count >= MAX_ACTIVE_INSTANCES_PER_CONNECTION
                        || total_count >= MAX_ACTIVE_INSTANCES)
                {
                    return Err(RegistryError::PermissionDenied(
                        "reviewer connection has too many active processes".to_string(),
                    ));
                }
                ensure_instance_capacity(&failure_reporters, connection_id, instance_id)?;
            }

            // Keep the instance map locked through the durable transition. A
            // disconnect and a heartbeat must observe one total order or the
            // row can say disconnected while a live instance is registered.
            self.workspace.mark_reviewer_seen(
                connection_id,
                at,
                at.saturating_add(REVIEWER_LEASE_MS),
                None,
            )?;
            if let Some(instance_id) = normalized_instance_id(instance_id) {
                failure_reporters
                    .entry(connection_id.clone())
                    .or_default()
                    .insert(
                        instance_id.to_string(),
                        FailureReporterBinding {
                            credential_version,
                            expires_at: at.saturating_add(REVIEWER_LEASE_MS),
                        },
                    );
                instances
                    .entry(connection_id.clone())
                    .or_default()
                    .insert(instance_id.to_string(), at);
            }
        }
        Ok(())
    }

    pub fn note_disconnected(
        &self,
        principal: &AuthenticatedPrincipal,
        instance_id: Option<&str>,
        at: i64,
    ) -> Result<(), RegistryError> {
        let _gate = self
            .authorization_gate
            .read()
            .map_err(|_| RegistryError::Unavailable)?;
        self.revalidated_connection(principal, at)?;
        let id = principal
            .reviewer_id()
            .ok_or(RegistryError::PermissionDenied(
                "the platform bearer is not a reviewer connection".to_string(),
            ))?;

        let instance_id = normalized_instance_id(instance_id);
        // Keep the reporter map locked through the visible disconnect. A new
        // bootstrap for the same process cannot slip between invalidation and
        // the durable status transition.
        let mut failure_reporters = self
            .failure_reporters
            .lock()
            .map_err(|_| RegistryError::Unavailable)?;
        if let Some(instance_id) = instance_id {
            if let Some(reporters) = failure_reporters.get_mut(id) {
                reporters.remove(instance_id);
                if reporters.is_empty() {
                    failure_reporters.remove(id);
                }
            }
        } else {
            failure_reporters.remove(id);
        }
        let mut instances = self
            .active_instances
            .lock()
            .map_err(|_| RegistryError::Unavailable)?;
        prune_expired_instances(&mut instances, at);
        if let Some(instance_id) = instance_id {
            let another_instance_is_live = instances.get(id).is_some_and(|active| {
                active
                    .keys()
                    .any(|active_instance| active_instance != instance_id)
            });
            if another_instance_is_live {
                if let Some(connection_instances) = instances.get_mut(id) {
                    connection_instances.remove(instance_id);
                }
                // Close only this process's transport sessions while another
                // process keeps the durable connection live.
                self.remove_instance_sessions(principal, Some(instance_id))?;
                return Ok(());
            }
        }

        // The active-instance lock serializes this durable transition with
        // heartbeats. Commit the durable state before the infallible map
        // removal so a storage failure cannot leave the two states inverted.
        self.workspace.mark_reviewer_disconnected(id, at)?;
        // Old launchers have no process identity, so their explicit disconnect
        // retains the historical all-instances behavior. This is also correct
        // for the final identified process because no other live ID remains.
        instances.remove(id);
        self.clear_reviewer_sessions(id)?;
        Ok(())
    }

    pub fn note_reported_model(
        &self,
        principal: &AuthenticatedPrincipal,
        model: Option<&str>,
        at: i64,
    ) -> Result<(), RegistryError> {
        if let Some(id) = principal.reviewer_id()
            && model.is_some()
        {
            self.workspace
                .update_reviewer_reported_model(id, model, at)?;
        }
        Ok(())
    }

    /// Validate the current row while holding a read gate for the complete
    /// document operation. Management changes take the write side of this
    /// gate, so a revoke cannot land halfway through an authorized mutation.
    pub fn authorize<'a>(
        &'a self,
        principal: &AuthenticatedPrincipal,
        operation: ReviewerOperation,
        document_id: Option<&str>,
    ) -> Result<AuthorizedRequest<'a>, RegistryError> {
        let gate = self
            .authorization_gate
            .read()
            .map_err(|_| RegistryError::Unavailable)?;
        match principal {
            AuthenticatedPrincipal::InternalEditor => {
                if operation != ReviewerOperation::Read {
                    return Err(RegistryError::PermissionDenied(
                        "the platform MCP principal is read-only".to_string(),
                    ));
                }
                Ok(AuthorizedRequest {
                    _gate: gate,
                    connection: None,
                })
            }
            AuthenticatedPrincipal::Reviewer { .. } => {
                let connection = self.revalidated_connection(principal, now_ms())?;
                let allowed = match operation {
                    ReviewerOperation::Read | ReviewerOperation::Suggest => {
                        connection.permissions.can_read
                    }
                    ReviewerOperation::Edit
                    | ReviewerOperation::Create
                    | ReviewerOperation::Trash => false,
                };
                if !allowed {
                    return Err(RegistryError::PermissionDenied(format!(
                        "reviewer `{}` is not allowed to {} documents",
                        connection.display_label,
                        operation.verb()
                    )));
                }
                if let Some(document_id) = document_id
                    && !connection.allows_document(document_id)
                {
                    return Err(RegistryError::PermissionDenied(format!(
                        "reviewer `{}` cannot access this document",
                        connection.display_label
                    )));
                }
                Ok(AuthorizedRequest {
                    _gate: gate,
                    connection: Some(connection),
                })
            }
        }
    }

    pub fn list(&self, at: i64) -> Result<Vec<ReviewerConnection>, RegistryError> {
        Ok(self.workspace.list_reviewer_connections(at)?)
    }

    pub fn connection(&self, id: &str) -> Result<ReviewerConnection, RegistryError> {
        Ok(self.workspace.reviewer_connection(id)?)
    }

    pub fn create(
        &self,
        client: ReviewerClient,
        display_label: String,
        permissions: ReviewerPermissions,
        at: i64,
    ) -> Result<ReviewerConnection, RegistryError> {
        let _gate = self
            .authorization_gate
            .write()
            .map_err(|_| RegistryError::Unavailable)?;
        let connection_id = new_connection_id()?;
        let credential = discovery::random_token()?;
        let hash = credential_hash(credential.as_bytes());

        self.credentials
            .set(&connection_id, credential.as_bytes())?;
        let result = self
            .workspace
            .create_reviewer_connection(&CreateReviewerConnection {
                id: connection_id.clone(),
                client,
                display_label,
                permissions,
                credential_hash: hash,
                credential_expires_at: None,
                created_at: at,
            });
        match result {
            Ok(connection) => Ok(connection),
            Err(error) => {
                let _ = self.credentials.delete(&connection_id);
                Err(error.into())
            }
        }
    }

    pub fn update(
        &self,
        id: &str,
        input: &UpdateReviewerConnection,
    ) -> Result<ReviewerConnection, RegistryError> {
        let _gate = self
            .authorization_gate
            .write()
            .map_err(|_| RegistryError::Unavailable)?;
        Ok(self.workspace.update_reviewer_connection(id, input)?)
    }

    /// Bind one native launcher process to the current non-secret credential
    /// generation before it reads the credential. This permits reporting a
    /// missing native credential without making an old process authoritative
    /// after Reset.
    pub fn prepare_failure_reporter(
        &self,
        id: &str,
        instance_id: &str,
        at: i64,
    ) -> Result<i64, RegistryError> {
        let _gate = self
            .authorization_gate
            .read()
            .map_err(|_| RegistryError::Unavailable)?;
        let instance_id = normalized_instance_id(Some(instance_id)).ok_or_else(|| {
            RegistryError::PermissionDenied("invalid reviewer process identity".into())
        })?;
        let connection = self.workspace.reviewer_connection(id)?;
        if !connection.status.is_active() {
            return Err(RegistryError::StaleFailureReport);
        }
        let credential_version = self.workspace.reviewer_credential_version(id)?;
        let mut reporters = self
            .failure_reporters
            .lock()
            .map_err(|_| RegistryError::Unavailable)?;
        prune_expired_failure_reporters(&mut reporters, at);
        ensure_instance_capacity(&reporters, id, instance_id)?;
        reporters.entry(id.to_string()).or_default().insert(
            instance_id.to_string(),
            FailureReporterBinding {
                credential_version,
                expires_at: at.saturating_add(REVIEWER_LEASE_MS),
            },
        );
        Ok(credential_version)
    }

    /// Serialize native launcher failures with reset and revoke. The report is
    /// accepted only for the process and credential generation registered
    /// above. Reset clears every registration while holding this write gate.
    pub fn mark_failed(
        &self,
        id: &str,
        instance_id: &str,
        failure_code: &str,
        at: i64,
    ) -> Result<ReviewerConnection, RegistryError> {
        let _gate = self
            .authorization_gate
            .write()
            .map_err(|_| RegistryError::Unavailable)?;
        let instance_id =
            normalized_instance_id(Some(instance_id)).ok_or(RegistryError::StaleFailureReport)?;
        // Resolve the durable row first so an unknown ID retains its precise
        // 404 response instead of being confused with a stale process.
        let current_credential_version = self.workspace.reviewer_credential_version(id)?;
        let mut reporters = self
            .failure_reporters
            .lock()
            .map_err(|_| RegistryError::Unavailable)?;
        prune_expired_failure_reporters(&mut reporters, at);
        // A report is a one-shot capability. Successful authenticated traffic
        // refreshes it, while failed or abandoned launches cannot fill the
        // bounded registry indefinitely.
        let binding = reporters
            .get_mut(id)
            .and_then(|instances| instances.remove(instance_id));
        if reporters.get(id).is_some_and(HashMap::is_empty) {
            reporters.remove(id);
        }
        if !binding.is_some_and(|binding| {
            binding.credential_version == current_credential_version && binding.expires_at > at
        }) {
            return Err(RegistryError::StaleFailureReport);
        }
        let connection = self.workspace.mark_reviewer_failed(id, failure_code, at)?;
        drop(reporters);
        Ok(connection)
    }

    pub fn reset_credential(
        &self,
        id: &str,
        expected_revision: i64,
        at: i64,
    ) -> Result<ReviewerConnection, RegistryError> {
        let _gate = self
            .authorization_gate
            .write()
            .map_err(|_| RegistryError::Unavailable)?;
        if let Some(completed) = self.recover_pending_rotation(id, expected_revision, at)? {
            self.clear_runtime_state(id)?;
            return Ok(completed);
        }
        let credential = discovery::random_token()?;
        let hash = credential_hash(credential.as_bytes());
        self.workspace
            .begin_reviewer_credential_rotation(id, expected_revision, &hash, at)?;

        if let Err(error) = self.credentials.set(id, credential.as_bytes()) {
            let _ =
                self.workspace
                    .cancel_reviewer_credential_rotation(id, expected_revision, now_ms());
            return Err(error.into());
        }

        // If the process stops after the Keychain write, both current and
        // pending hashes authenticate. A later reset can safely finish this
        // same row before attempting another rotation.
        let connection =
            self.workspace
                .finish_reviewer_credential_rotation(id, expected_revision, now_ms())?;
        self.clear_runtime_state(id)?;
        Ok(connection)
    }

    pub fn revoke(
        &self,
        id: &str,
        expected_revision: i64,
        at: i64,
    ) -> Result<ReviewerConnection, RegistryError> {
        let _gate = self
            .authorization_gate
            .write()
            .map_err(|_| RegistryError::Unavailable)?;
        let connection = self
            .workspace
            .revoke_reviewer_connection(id, expected_revision, at)?;
        if let Err(error) = self.credentials.delete(id) {
            tracing::warn!(connection_id = id, error = %error, "revoked reviewer credential could not be removed from native storage");
        }
        self.clear_runtime_state(id)?;
        Ok(connection)
    }

    pub fn session_matches(
        &self,
        session_id: &str,
        principal: &AuthenticatedPrincipal,
        instance_id: Option<&str>,
        at: i64,
    ) -> Result<bool, RegistryError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| RegistryError::Unavailable)?;
        let Some(bound) = sessions.get_mut(session_id) else {
            return Ok(false);
        };
        let matches = bound.principal == *principal
            && bound.instance_id.as_deref() == normalized_instance_id(instance_id);
        if matches {
            bound.last_seen_at = at;
        }
        Ok(matches)
    }

    /// Forget authorization state after the transport manager has closed a
    /// session itself, for example after an MCP DELETE or worker shutdown.
    /// This deliberately does not invoke the transport closer again.
    pub fn forget_session_binding(&self, session_id: &str) -> Result<bool, RegistryError> {
        Ok(self
            .sessions
            .lock()
            .map_err(|_| RegistryError::Unavailable)?
            .remove(session_id)
            .is_some())
    }

    pub fn bind_session(
        &self,
        session_id: &str,
        principal: &AuthenticatedPrincipal,
        instance_id: Option<&str>,
        at: i64,
    ) -> Result<bool, RegistryError> {
        let _gate = self
            .authorization_gate
            .read()
            .map_err(|_| RegistryError::Unavailable)?;
        if matches!(principal, AuthenticatedPrincipal::Reviewer { .. })
            && let Err(error) = self.revalidated_connection(principal, at)
        {
            self.close_sessions([session_id.to_string()]);
            return Err(error);
        }

        // A disconnect holds this same lock while removing its sessions. If it
        // won the race, do not recreate a session for an instance that is no
        // longer live. Reset and revoke are serialized by the gate above.
        let mut instances = self
            .active_instances
            .lock()
            .map_err(|_| RegistryError::Unavailable)?;
        prune_expired_instances(&mut instances, at);
        if let AuthenticatedPrincipal::Reviewer { connection_id, .. } = principal
            && let Some(instance_id) = normalized_instance_id(instance_id)
            && !instances
                .get(connection_id)
                .is_some_and(|active| active.contains_key(instance_id))
        {
            drop(instances);
            self.close_sessions([session_id.to_string()]);
            return Ok(false);
        }

        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| RegistryError::Unavailable)?;
        let instance_id = normalized_instance_id(instance_id).map(str::to_string);
        match sessions.get_mut(session_id) {
            Some(bound) => {
                let matches = bound.principal == *principal && bound.instance_id == instance_id;
                if matches {
                    bound.last_seen_at = at;
                }
                Ok(matches)
            }
            None => {
                let principal_count = sessions
                    .values()
                    .filter(|binding| binding.principal == *principal)
                    .count();
                let evicted = if principal_count >= MAX_SESSION_BINDINGS_PER_PRINCIPAL {
                    sessions
                        .iter()
                        .filter(|(_, binding)| binding.principal == *principal)
                        .min_by_key(|(session, binding)| (binding.last_seen_at, *session))
                        .map(|(session, _)| session.clone())
                } else {
                    None
                };
                if let Some(oldest) = evicted.as_ref() {
                    sessions.remove(oldest);
                } else if sessions.len() >= MAX_SESSION_BINDINGS {
                    drop(sessions);
                    drop(instances);
                    self.close_sessions([session_id.to_string()]);
                    return Ok(false);
                }
                sessions.insert(
                    session_id.to_string(),
                    SessionBinding {
                        principal: principal.clone(),
                        instance_id,
                        last_seen_at: at,
                    },
                );
                drop(sessions);
                drop(instances);
                self.close_sessions(evicted);
                Ok(true)
            }
        }
    }

    pub fn credential_for_shim(&self, connection_id: &str) -> Result<Vec<u8>, RegistryError> {
        Ok(self.credentials.get(connection_id)?)
    }

    fn recover_pending_rotation(
        &self,
        id: &str,
        expected_revision: i64,
        at: i64,
    ) -> Result<Option<ReviewerConnection>, RegistryError> {
        if !self.workspace.reviewer_has_pending_credential(id)? {
            return Ok(None);
        }
        let credential = match self.credentials.get(id) {
            Ok(credential) => credential,
            Err(_) => {
                // An explicit Reset can repair a missing native item. Clear the
                // uncommitted stage, then let the caller create a fresh one.
                self.workspace
                    .cancel_reviewer_credential_rotation(id, expected_revision, at)?;
                return Ok(None);
            }
        };
        let hash = credential_hash(&credential);
        match self
            .workspace
            .reviewer_pending_credential_matches(id, &hash)?
        {
            Some(true) => Ok(Some(self.workspace.finish_reviewer_credential_rotation(
                id,
                expected_revision,
                at,
            )?)),
            Some(false) => {
                self.workspace
                    .cancel_reviewer_credential_rotation(id, expected_revision, at)?;
                Ok(None)
            }
            None => Ok(None),
        }
    }

    fn revalidated_connection(
        &self,
        principal: &AuthenticatedPrincipal,
        at: i64,
    ) -> Result<ReviewerConnection, RegistryError> {
        let AuthenticatedPrincipal::Reviewer {
            connection_id,
            credential_hash,
        } = principal
        else {
            return Err(RegistryError::PermissionDenied(
                "the platform bearer is not a reviewer connection".to_string(),
            ));
        };
        self.workspace
            .reviewer_connection_by_credential_hash(credential_hash, at)?
            .filter(|connection| connection.id == *connection_id)
            .ok_or(RegistryError::Unauthorized)
    }

    fn remove_instance_sessions(
        &self,
        principal: &AuthenticatedPrincipal,
        instance_id: Option<&str>,
    ) -> Result<(), RegistryError> {
        let removed = self.remove_sessions(|binding| {
            binding.principal == *principal && binding.instance_id.as_deref() == instance_id
        })?;
        self.close_sessions(removed);
        Ok(())
    }

    fn clear_reviewer_sessions(&self, id: &str) -> Result<(), RegistryError> {
        let removed =
            self.remove_sessions(|binding| binding.principal.reviewer_id() == Some(id))?;
        self.close_sessions(removed);
        Ok(())
    }

    fn remove_sessions(
        &self,
        predicate: impl Fn(&SessionBinding) -> bool,
    ) -> Result<Vec<String>, RegistryError> {
        let mut sessions = self
            .sessions
            .lock()
            .map_err(|_| RegistryError::Unavailable)?;
        let removed = sessions
            .iter()
            .filter(|(_, binding)| predicate(binding))
            .map(|(session_id, _)| session_id.clone())
            .collect::<Vec<_>>();
        for session_id in &removed {
            sessions.remove(session_id);
        }
        Ok(removed)
    }

    fn close_sessions(&self, session_ids: impl IntoIterator<Item = String>) {
        let Some(closer) = self.session_closer.as_ref() else {
            return;
        };
        for session_id in session_ids {
            closer(session_id);
        }
    }

    fn clear_runtime_state(&self, id: &str) -> Result<(), RegistryError> {
        self.clear_reviewer_sessions(id)?;
        self.active_instances
            .lock()
            .map_err(|_| RegistryError::Unavailable)?
            .remove(id);
        self.failure_reporters
            .lock()
            .map_err(|_| RegistryError::Unavailable)?
            .remove(id);
        Ok(())
    }

    fn reconcile_startup(&self) {
        let at = now_ms();
        let Ok(connections) = self.workspace.list_reviewer_connections(at) else {
            tracing::warn!("could not reconcile reviewer connection state at startup");
            return;
        };
        for connection in connections {
            if connection.status == ReviewerConnectionStatus::Connected
                && let Err(error) = self
                    .workspace
                    .mark_reviewer_disconnected(&connection.id, at)
            {
                tracing::warn!(connection_id = %connection.id, error = %error, "could not clear a stale reviewer lease");
            }
            let Ok(true) = self
                .workspace
                .reviewer_has_pending_credential(&connection.id)
            else {
                continue;
            };
            let credential = match self.credentials.get(&connection.id) {
                Ok(credential) => credential,
                Err(error) => {
                    tracing::warn!(connection_id = %connection.id, error = %error, "pending reviewer credential is missing from native storage");
                    let _ = self.workspace.mark_reviewer_failed(
                        &connection.id,
                        "credential_missing",
                        at,
                    );
                    continue;
                }
            };
            let hash = credential_hash(&credential);
            match self
                .workspace
                .reviewer_pending_credential_matches(&connection.id, &hash)
            {
                Ok(Some(true)) => {
                    if let Err(error) = self.workspace.finish_reviewer_credential_rotation(
                        &connection.id,
                        connection.revision,
                        at,
                    ) {
                        tracing::warn!(connection_id = %connection.id, error = %error, "could not finish reviewer credential rotation");
                    }
                }
                Ok(Some(false)) => {
                    let native_is_current = self
                        .workspace
                        .reviewer_connection_by_credential_hash(&hash, at)
                        .ok()
                        .flatten()
                        .is_some_and(|resolved| resolved.id == connection.id);
                    if native_is_current {
                        let _ = self.workspace.cancel_reviewer_credential_rotation(
                            &connection.id,
                            connection.revision,
                            at,
                        );
                    } else {
                        let _ = self.workspace.mark_reviewer_failed(
                            &connection.id,
                            "credential_store",
                            at,
                        );
                    }
                }
                Ok(None) => {}
                Err(error) => {
                    tracing::warn!(connection_id = %connection.id, error = %error, "could not inspect reviewer credential rotation");
                }
            }
        }
    }
}

fn normalized_instance_id(instance_id: Option<&str>) -> Option<&str> {
    instance_id
        .map(str::trim)
        .filter(|instance_id| !instance_id.is_empty() && instance_id.len() <= 128)
}

fn prune_expired_instances(instances: &mut HashMap<String, HashMap<String, i64>>, at: i64) {
    instances.retain(|_, connection_instances| {
        connection_instances.retain(|_, seen_at| seen_at.saturating_add(REVIEWER_LEASE_MS) > at);
        !connection_instances.is_empty()
    });
}

fn active_instance_count<T>(instances: &HashMap<String, HashMap<String, T>>) -> usize {
    instances.values().map(HashMap::len).sum()
}

fn prune_expired_failure_reporters(
    reporters: &mut HashMap<String, HashMap<String, FailureReporterBinding>>,
    at: i64,
) {
    reporters.retain(|_, connection_reporters| {
        connection_reporters.retain(|_, binding| binding.expires_at > at);
        !connection_reporters.is_empty()
    });
}

fn ensure_instance_capacity(
    instances: &HashMap<String, HashMap<String, FailureReporterBinding>>,
    connection_id: &str,
    instance_id: &str,
) -> Result<(), RegistryError> {
    let already_present = instances
        .get(connection_id)
        .is_some_and(|active| active.contains_key(instance_id));
    if already_present {
        return Ok(());
    }
    if active_instance_count(instances) >= MAX_ACTIVE_INSTANCES
        || instances.get(connection_id).map_or(0, HashMap::len)
            >= MAX_ACTIVE_INSTANCES_PER_CONNECTION
    {
        return Err(RegistryError::Unavailable);
    }
    Ok(())
}

impl ReviewerOperation {
    fn verb(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::Suggest => "suggest changes to",
            Self::Edit => "edit",
            Self::Create => "create",
            Self::Trash => "trash or restore",
        }
    }
}

fn credential_hash(credential: &[u8]) -> [u8; 32] {
    Sha256::digest(credential).into()
}

fn new_connection_id() -> Result<String, std::io::Error> {
    let random = discovery::random_token()?;
    Ok(format!("reviewer-{}", &random[..48]))
}

pub fn now_ms() -> i64 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as i64)
        .unwrap_or(0)
}

#[derive(Debug)]
pub enum RegistryError {
    Unauthorized,
    StaleFailureReport,
    PermissionDenied(String),
    Unavailable,
    Workspace(WorkspaceError),
    Credential(CredentialError),
    Io(std::io::Error),
}

impl std::fmt::Display for RegistryError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Unauthorized => formatter.write_str("reviewer connection is not authorized"),
            Self::StaleFailureReport => {
                formatter.write_str("reviewer failure report is stale or unregistered")
            }
            Self::PermissionDenied(message) => formatter.write_str(message),
            Self::Unavailable => formatter.write_str("reviewer connection state is unavailable"),
            Self::Workspace(error) => error.fmt(formatter),
            Self::Credential(error) => error.fmt(formatter),
            Self::Io(error) => error.fmt(formatter),
        }
    }
}

impl std::error::Error for RegistryError {}

impl From<WorkspaceError> for RegistryError {
    fn from(error: WorkspaceError) -> Self {
        Self::Workspace(error)
    }
}

impl From<CredentialError> for RegistryError {
    fn from(error: CredentialError) -> Self {
        Self::Credential(error)
    }
}

impl From<std::io::Error> for RegistryError {
    fn from(error: std::io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn registry() -> (Arc<Workspace>, ConnectionRegistry, tempfile::TempDir) {
        let workspace = Arc::new(Workspace::open_in_memory().unwrap());
        let directory = tempfile::tempdir().unwrap();
        let registry =
            ConnectionRegistry::new(workspace.clone(), CredentialStore::files(directory.path()));
        (workspace, registry, directory)
    }

    fn registry_with_session_log() -> (
        Arc<Workspace>,
        ConnectionRegistry,
        tempfile::TempDir,
        Arc<Mutex<Vec<String>>>,
    ) {
        let workspace = Arc::new(Workspace::open_in_memory().unwrap());
        let directory = tempfile::tempdir().unwrap();
        let closed = Arc::new(Mutex::new(Vec::new()));
        let closed_for_callback = closed.clone();
        let registry =
            ConnectionRegistry::new(workspace.clone(), CredentialStore::files(directory.path()))
                .with_session_closer(move |session_id| {
                    closed_for_callback.lock().unwrap().push(session_id);
                });
        (workspace, registry, directory, closed)
    }

    fn authenticated(
        registry: &ConnectionRegistry,
        connection_id: &str,
        at: i64,
    ) -> AuthenticatedPrincipal {
        let credential = registry.credential_for_shim(connection_id).unwrap();
        registry
            .authenticate_reviewer(std::str::from_utf8(&credential).unwrap(), at)
            .unwrap()
            .unwrap()
    }

    #[test]
    fn raw_credentials_never_enter_the_returned_connection() {
        let (_workspace, registry, _directory) = registry();
        let connection = registry
            .create(
                ReviewerClient::Codex,
                "Codex review".to_string(),
                ReviewerPermissions::all(true, true, false),
                10,
            )
            .unwrap();
        let credential = registry.credential_for_shim(&connection.id).unwrap();
        let bearer = std::str::from_utf8(&credential).unwrap();
        assert_eq!(
            registry
                .authenticate_reviewer(bearer, 11)
                .unwrap()
                .unwrap()
                .reviewer_id(),
            Some(connection.id.as_str())
        );
        let serialized = serde_json::to_string(&connection).unwrap();
        assert!(!serialized.contains(bearer));
    }

    #[test]
    fn internal_capability_is_read_only_and_revoke_wins() {
        let (_workspace, registry, _directory) = registry();
        assert!(
            registry
                .authorize(
                    &AuthenticatedPrincipal::InternalEditor,
                    ReviewerOperation::Read,
                    None,
                )
                .is_ok()
        );
        assert!(matches!(
            registry.authorize(
                &AuthenticatedPrincipal::InternalEditor,
                ReviewerOperation::Edit,
                None,
            ),
            Err(RegistryError::PermissionDenied(_))
        ));

        let connection = registry
            .create(
                ReviewerClient::ClaudeCode,
                "Claude review".to_string(),
                ReviewerPermissions::all(true, true, true),
                20,
            )
            .unwrap();
        let principal = authenticated(&registry, &connection.id, 20);
        assert!(matches!(
            registry.authorize(&principal, ReviewerOperation::Edit, None),
            Err(RegistryError::PermissionDenied(_))
        ));
        assert!(
            registry
                .authorize(&principal, ReviewerOperation::Read, None)
                .is_ok()
        );
        registry
            .revoke(&connection.id, connection.revision, 21)
            .unwrap();
        assert!(matches!(
            registry.authorize(&principal, ReviewerOperation::Edit, None),
            Err(RegistryError::Unauthorized)
        ));
    }

    #[test]
    fn sessions_cannot_cross_connection_identities() {
        let (_workspace, registry, _directory) = registry();
        let first_connection = registry
            .create(
                ReviewerClient::Codex,
                "First review".to_string(),
                ReviewerPermissions::all(true, true, false),
                10,
            )
            .unwrap();
        let second_connection = registry
            .create(
                ReviewerClient::ClaudeCode,
                "Second review".to_string(),
                ReviewerPermissions::all(true, true, false),
                10,
            )
            .unwrap();
        let first = authenticated(&registry, &first_connection.id, 10);
        let second = authenticated(&registry, &second_connection.id, 10);
        assert!(registry.bind_session("session", &first, None, 10).unwrap());
        assert!(
            registry
                .session_matches("session", &first, None, 11)
                .unwrap()
        );
        assert!(
            !registry
                .session_matches("session", &second, None, 11)
                .unwrap()
        );
        assert!(!registry.bind_session("session", &second, None, 11).unwrap());
    }

    #[test]
    fn restart_clears_live_status_and_finishes_a_native_rotation() {
        let workspace = Arc::new(Workspace::open_in_memory().unwrap());
        let directory = tempfile::tempdir().unwrap();
        let credentials = CredentialStore::files(directory.path());
        let registry = ConnectionRegistry::new(workspace.clone(), credentials.clone());
        let connection = registry
            .create(
                ReviewerClient::Codex,
                "Codex review".to_string(),
                ReviewerPermissions::all(true, true, false),
                10,
            )
            .unwrap();
        let original = registry.credential_for_shim(&connection.id).unwrap();
        let principal = authenticated(&registry, &connection.id, 20);
        registry
            .note_authenticated(&principal, Some("restart-test"), 20)
            .unwrap();

        let replacement = b"replacement-reviewer-secret";
        let replacement_hash = credential_hash(replacement);
        workspace
            .begin_reviewer_credential_rotation(
                &connection.id,
                connection.revision,
                &replacement_hash,
                21,
            )
            .unwrap();
        credentials.set(&connection.id, replacement).unwrap();
        drop(registry);

        let restarted = ConnectionRegistry::new(workspace, credentials);
        let recovered = restarted.connection(&connection.id).unwrap();
        assert_eq!(recovered.status, ReviewerConnectionStatus::Disconnected);
        assert_eq!(recovered.revision, 2);
        assert!(
            restarted
                .authenticate_reviewer(std::str::from_utf8(replacement).unwrap(), 22)
                .unwrap()
                .is_some()
        );
        assert!(
            restarted
                .authenticate_reviewer(std::str::from_utf8(&original).unwrap(), 22)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn reset_invalidates_a_principal_authenticated_before_rotation() {
        let (_workspace, registry, _directory, closed) = registry_with_session_log();
        let connection = registry
            .create(
                ReviewerClient::Codex,
                "Codex review".to_string(),
                ReviewerPermissions::all(true, true, false),
                10,
            )
            .unwrap();
        let stale = authenticated(&registry, &connection.id, 11);
        registry
            .note_authenticated(&stale, Some("old-process"), 12)
            .unwrap();
        assert!(
            registry
                .bind_session("old-session", &stale, Some("old-process"), 12)
                .unwrap()
        );

        let reset = registry
            .reset_credential(&connection.id, connection.revision, 13)
            .unwrap();
        assert_eq!(reset.status, ReviewerConnectionStatus::Disconnected);
        assert!(matches!(
            registry.authorize(&stale, ReviewerOperation::Edit, None),
            Err(RegistryError::Unauthorized)
        ));
        assert!(
            !registry
                .session_matches("old-session", &stale, Some("old-process"), 14)
                .unwrap()
        );
        assert!(closed.lock().unwrap().contains(&"old-session".to_string()));
        assert!(matches!(
            registry.note_authenticated(&stale, Some("old-process"), 14),
            Err(RegistryError::Unauthorized)
        ));
        assert!(matches!(
            registry.bind_session("late-session", &stale, Some("old-process"), 14),
            Err(RegistryError::Unauthorized)
        ));
        assert!(closed.lock().unwrap().contains(&"late-session".to_string()));

        let replacement = authenticated(&registry, &connection.id, 14);
        assert!(matches!(
            registry.authorize(&replacement, ReviewerOperation::Edit, None),
            Err(RegistryError::PermissionDenied(_))
        ));
        assert!(
            registry
                .authorize(&replacement, ReviewerOperation::Read, None)
                .is_ok()
        );
    }

    #[test]
    fn disconnecting_one_of_two_active_instances_keeps_connection_live() {
        let (_workspace, registry, _directory, closed) = registry_with_session_log();
        let connection = registry
            .create(
                ReviewerClient::ClaudeCode,
                "Claude review".to_string(),
                ReviewerPermissions::all(true, true, false),
                10,
            )
            .unwrap();
        let principal = authenticated(&registry, &connection.id, 11);

        registry
            .note_authenticated(&principal, Some("process-one"), 20)
            .unwrap();
        registry
            .note_authenticated(&principal, Some("process-two"), 21)
            .unwrap();
        registry
            .bind_session("one", &principal, Some("process-one"), 20)
            .unwrap();
        registry
            .bind_session("two", &principal, Some("process-two"), 21)
            .unwrap();

        registry
            .note_disconnected(&principal, Some("process-one"), 22)
            .unwrap();
        assert_eq!(
            registry.connection(&connection.id).unwrap().status,
            ReviewerConnectionStatus::Connected
        );
        assert!(
            !registry
                .session_matches("one", &principal, Some("process-one"), 22)
                .unwrap()
        );
        assert!(
            registry
                .session_matches("two", &principal, Some("process-two"), 22)
                .unwrap()
        );
        assert!(closed.lock().unwrap().contains(&"one".to_string()));
        assert!(!closed.lock().unwrap().contains(&"two".to_string()));

        registry
            .note_disconnected(&principal, Some("process-two"), 23)
            .unwrap();
        assert_eq!(
            registry.connection(&connection.id).unwrap().status,
            ReviewerConnectionStatus::Disconnected
        );
        assert!(closed.lock().unwrap().contains(&"two".to_string()));
    }

    #[test]
    fn revocation_closes_live_reviewer_sessions() {
        let (_workspace, registry, _directory, closed) = registry_with_session_log();
        let connection = registry
            .create(
                ReviewerClient::Codex,
                "Codex review".to_string(),
                ReviewerPermissions::all(true, true, false),
                10,
            )
            .unwrap();
        let principal = authenticated(&registry, &connection.id, 11);
        registry
            .note_authenticated(&principal, Some("process"), 12)
            .unwrap();
        assert!(
            registry
                .bind_session("live-session", &principal, Some("process"), 12)
                .unwrap()
        );

        let revoked = registry
            .revoke(&connection.id, connection.revision, 13)
            .unwrap();
        assert_eq!(revoked.status, ReviewerConnectionStatus::Revoked);
        assert!(closed.lock().unwrap().contains(&"live-session".to_string()));
        assert!(
            !registry
                .session_matches("live-session", &principal, Some("process"), 14)
                .unwrap()
        );
    }

    #[test]
    fn one_principal_cannot_evict_another_principals_session() {
        let (_workspace, registry, _directory, closed) = registry_with_session_log();
        let connection = registry
            .create(
                ReviewerClient::ClaudeCode,
                "Claude review".to_string(),
                ReviewerPermissions::all(true, true, false),
                10,
            )
            .unwrap();
        let reviewer = authenticated(&registry, &connection.id, 11);
        assert!(
            registry
                .bind_session("reviewer-live", &reviewer, None, 11)
                .unwrap()
        );

        let editor = AuthenticatedPrincipal::InternalEditor;
        for index in 0..=MAX_SESSION_BINDINGS_PER_PRINCIPAL {
            assert!(
                registry
                    .bind_session(&format!("editor-{index:04}"), &editor, None, index as i64)
                    .unwrap()
            );
        }

        assert!(
            registry
                .session_matches("reviewer-live", &reviewer, None, 2_000)
                .unwrap()
        );
        let closed = closed.lock().unwrap();
        assert!(closed.contains(&"editor-0000".to_string()));
        assert!(!closed.contains(&"reviewer-live".to_string()));
    }

    #[test]
    fn active_instance_tracking_is_bounded_and_expiry_reclaims_capacity() {
        let (_workspace, registry, _directory) = registry();
        let connection = registry
            .create(
                ReviewerClient::Codex,
                "Codex review".to_string(),
                ReviewerPermissions::all(true, true, false),
                10,
            )
            .unwrap();
        let principal = authenticated(&registry, &connection.id, 11);
        for index in 0..MAX_ACTIVE_INSTANCES_PER_CONNECTION {
            registry
                .note_authenticated(&principal, Some(&format!("process-{index}")), 20)
                .unwrap();
        }
        assert!(matches!(
            registry.note_authenticated(&principal, Some("one-too-many"), 20),
            Err(RegistryError::PermissionDenied(_))
        ));
        assert_eq!(
            registry
                .active_instances
                .lock()
                .unwrap()
                .get(&connection.id)
                .unwrap()
                .len(),
            MAX_ACTIVE_INSTANCES_PER_CONNECTION
        );

        let after_expiry = 20 + REVIEWER_LEASE_MS + 1;
        registry
            .note_authenticated(&principal, Some("replacement"), after_expiry)
            .unwrap();
        let instances = registry.active_instances.lock().unwrap();
        assert_eq!(active_instance_count(&instances), 1);
        assert!(
            instances
                .get(&connection.id)
                .unwrap()
                .contains_key("replacement")
        );
    }

    #[test]
    fn global_active_instance_limit_rejects_growth_without_evicting_live_entries() {
        let (_workspace, registry, _directory) = registry();
        let connection = registry
            .create(
                ReviewerClient::ClaudeCode,
                "Claude review".to_string(),
                ReviewerPermissions::all(true, true, false),
                10,
            )
            .unwrap();
        let principal = authenticated(&registry, &connection.id, 11);
        {
            let mut instances = registry.active_instances.lock().unwrap();
            for index in 0..MAX_ACTIVE_INSTANCES {
                instances
                    .entry(format!("connection-{index}"))
                    .or_default()
                    .insert("process".to_string(), 20);
            }
        }

        assert!(matches!(
            registry.note_authenticated(&principal, Some("new-process"), 20),
            Err(RegistryError::PermissionDenied(_))
        ));
        assert_eq!(
            active_instance_count(&registry.active_instances.lock().unwrap()),
            MAX_ACTIVE_INSTANCES
        );
    }

    #[test]
    fn concurrent_heartbeat_and_disconnect_leave_live_instance_connected() {
        use std::sync::Barrier;

        let (_workspace, registry, _directory) = registry();
        let registry = Arc::new(registry);
        let connection = registry
            .create(
                ReviewerClient::Codex,
                "Codex review".to_string(),
                ReviewerPermissions::all(true, true, false),
                10,
            )
            .unwrap();
        let principal = authenticated(&registry, &connection.id, 11);

        for round in 0..32 {
            let at = 20 + round;
            let old_instance = format!("old-{round}");
            let live_instance = format!("live-{round}");
            registry
                .note_authenticated(&principal, Some(&old_instance), at)
                .unwrap();

            let start = Arc::new(Barrier::new(3));
            let disconnect_registry = registry.clone();
            let disconnect_principal = principal.clone();
            let disconnect_start = start.clone();
            let disconnect = std::thread::spawn(move || {
                disconnect_start.wait();
                disconnect_registry.note_disconnected(
                    &disconnect_principal,
                    Some(&old_instance),
                    at + 1,
                )
            });
            let heartbeat_registry = registry.clone();
            let heartbeat_principal = principal.clone();
            let heartbeat_start = start.clone();
            let heartbeat = std::thread::spawn(move || {
                heartbeat_start.wait();
                heartbeat_registry.note_authenticated(
                    &heartbeat_principal,
                    Some(&live_instance),
                    at + 1,
                )
            });
            start.wait();
            disconnect.join().unwrap().unwrap();
            heartbeat.join().unwrap().unwrap();

            assert_eq!(
                registry.connection(&connection.id).unwrap().status,
                ReviewerConnectionStatus::Connected
            );
            let live_instance = format!("live-{round}");
            assert!(
                registry
                    .active_instances
                    .lock()
                    .unwrap()
                    .get(&connection.id)
                    .is_some_and(|active| active.contains_key(&live_instance))
            );
            registry
                .note_disconnected(&principal, Some(&live_instance), at + 2)
                .unwrap();
        }
    }

    #[test]
    fn failure_reporters_are_consumed_and_expiry_reclaims_capacity() {
        let (_workspace, registry, _directory) = registry();
        let connection = registry
            .create(
                ReviewerClient::Codex,
                "Codex review".to_string(),
                ReviewerPermissions::all(true, true, false),
                10,
            )
            .unwrap();

        for index in 0..MAX_ACTIVE_INSTANCES_PER_CONNECTION {
            registry
                .prepare_failure_reporter(&connection.id, &format!("abandoned-{index}"), 20)
                .unwrap();
        }
        assert!(matches!(
            registry.prepare_failure_reporter(&connection.id, "one-too-many", 20),
            Err(RegistryError::Unavailable)
        ));

        let after_expiry = 20 + REVIEWER_LEASE_MS + 1;
        registry
            .prepare_failure_reporter(&connection.id, "reported", after_expiry)
            .unwrap();
        registry
            .mark_failed(&connection.id, "reported", "transport", after_expiry + 1)
            .unwrap();
        assert!(registry.failure_reporters.lock().unwrap().is_empty());

        registry
            .prepare_failure_reporter(&connection.id, "replacement", after_expiry + 2)
            .unwrap();
    }

    #[test]
    fn authenticated_traffic_refreshes_only_the_current_failure_generation() {
        let (_workspace, registry, _directory) = registry();
        let connection = registry
            .create(
                ReviewerClient::ClaudeCode,
                "Claude review".to_string(),
                ReviewerPermissions::all(true, true, false),
                10,
            )
            .unwrap();
        let stale = authenticated(&registry, &connection.id, 11);
        registry
            .note_authenticated(&stale, Some("same-process"), 12)
            .unwrap();

        registry
            .reset_credential(&connection.id, connection.revision, 13)
            .unwrap();
        assert!(matches!(
            registry.note_authenticated(&stale, Some("same-process"), 14),
            Err(RegistryError::Unauthorized)
        ));
        assert!(matches!(
            registry.mark_failed(&connection.id, "same-process", "transport", 14),
            Err(RegistryError::StaleFailureReport)
        ));

        let current = authenticated(&registry, &connection.id, 15);
        registry
            .note_authenticated(&current, Some("same-process"), 15)
            .unwrap();
        let failed = registry
            .mark_failed(&connection.id, "same-process", "protocol", 16)
            .unwrap();
        assert_eq!(failed.status, ReviewerConnectionStatus::Failed);
        assert_eq!(failed.failure_code.as_deref(), Some("protocol"));
    }

    #[test]
    fn session_bindings_have_a_deterministic_per_principal_bound() {
        let (_workspace, registry, _directory, closed) = registry_with_session_log();
        let principal = AuthenticatedPrincipal::InternalEditor;
        for index in 0..=MAX_SESSION_BINDINGS_PER_PRINCIPAL {
            assert!(
                registry
                    .bind_session(
                        &format!("session-{index:04}"),
                        &principal,
                        None,
                        index as i64
                    )
                    .unwrap()
            );
        }

        assert_eq!(
            registry.sessions.lock().unwrap().len(),
            MAX_SESSION_BINDINGS_PER_PRINCIPAL
        );
        assert!(
            !registry
                .session_matches("session-0000", &principal, None, 2_000)
                .unwrap()
        );
        assert!(
            registry
                .session_matches("session-0064", &principal, None, 2_000)
                .unwrap()
        );
        assert_eq!(closed.lock().unwrap().as_slice(), ["session-0000"]);
    }
}
