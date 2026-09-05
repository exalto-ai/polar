//! MCP session lifecycle coupled to ephemeral reviewer direct-edit grants.

use crate::connections::ConnectionRegistry;
use futures::Stream;
use rmcp::model::{ClientJsonRpcMessage, ServerJsonRpcMessage};
use rmcp::transport::streamable_http_server::session::local::{
    LocalSessionManager, LocalSessionManagerError, SessionConfig, SessionTransport,
};
use rmcp::transport::streamable_http_server::session::{
    EventStore, RestoreOutcome, ServerSseMessage, SessionId, SessionManager,
};
use std::sync::Arc;
use std::time::Duration;

#[derive(Debug)]
pub enum ReviewerSessionError {
    Session(LocalSessionManagerError),
    GrantCleanup(String),
}

impl std::fmt::Display for ReviewerSessionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Session(error) => write!(formatter, "{error}"),
            Self::GrantCleanup(error) => write!(formatter, "direct-edit cleanup: {error}"),
        }
    }
}

impl std::error::Error for ReviewerSessionError {}

impl From<LocalSessionManagerError> for ReviewerSessionError {
    fn from(error: LocalSessionManagerError) -> Self {
        Self::Session(error)
    }
}

/// Delegates MCP transport behavior to rmcp and ties direct-edit authority to
/// the same lifecycle. rmcp calls `close_session` for explicit DELETE, worker
/// cancellation, and idle termination, so a grant cannot outlive its transport
/// session even when a client disappears without a clean shutdown.
pub struct ReviewerSessionManager {
    inner: LocalSessionManager,
    reviewers: Arc<ConnectionRegistry>,
}

impl ReviewerSessionManager {
    pub fn new(reviewers: Arc<ConnectionRegistry>) -> Self {
        Self {
            inner: LocalSessionManager::default(),
            reviewers,
        }
    }

    /// Shortens rmcp's idle timeout for lifecycle tests. Production callers
    /// cannot use this to keep an abandoned session alive longer than rmcp's
    /// five-minute safety limit.
    pub fn with_shorter_idle_timeout(
        reviewers: Arc<ConnectionRegistry>,
        keep_alive: Duration,
    ) -> Self {
        let mut inner = LocalSessionManager::default();
        inner.session_config.keep_alive = Some(keep_alive.min(SessionConfig::DEFAULT_KEEP_ALIVE));
        Self { inner, reviewers }
    }
}

impl SessionManager for ReviewerSessionManager {
    type Error = ReviewerSessionError;
    type Transport = SessionTransport;

    async fn create_session(&self) -> Result<(SessionId, Self::Transport), Self::Error> {
        Ok(self.inner.create_session().await?)
    }

    async fn initialize_session(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<ServerJsonRpcMessage, Self::Error> {
        Ok(self.inner.initialize_session(id, message).await?)
    }

    async fn has_session(&self, id: &SessionId) -> Result<bool, Self::Error> {
        Ok(self.inner.has_session(id).await?)
    }

    async fn close_session(&self, id: &SessionId) -> Result<(), Self::Error> {
        let cleanup = self
            .reviewers
            .revoke_mcp_session(id.as_ref())
            .map_err(|error| ReviewerSessionError::GrantCleanup(error.to_string()));
        let close = self.inner.close_session(id).await.map_err(Into::into);
        cleanup.and(close)
    }

    async fn create_stream(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        Ok(self.inner.create_stream(id, message).await?)
    }

    async fn accept_message(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<(), Self::Error> {
        Ok(self.inner.accept_message(id, message).await?)
    }

    async fn create_standalone_stream(
        &self,
        id: &SessionId,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        Ok(self.inner.create_standalone_stream(id).await?)
    }

    async fn resume(
        &self,
        id: &SessionId,
        last_event_id: String,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        Ok(self.inner.resume(id, last_event_id).await?)
    }

    async fn restore_session(
        &self,
        id: SessionId,
    ) -> Result<RestoreOutcome<Self::Transport>, Self::Error> {
        Ok(match self.inner.restore_session(id).await? {
            RestoreOutcome::Restored(transport) => RestoreOutcome::Restored(transport),
            RestoreOutcome::AlreadyPresent => RestoreOutcome::AlreadyPresent,
            RestoreOutcome::NotSupported => RestoreOutcome::NotSupported,
            _ => RestoreOutcome::NotSupported,
        })
    }

    fn event_store(&self) -> Option<Arc<dyn EventStore>> {
        self.inner.event_store()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::{CredentialFiles, ReviewerOperation};
    use crate::direct_edit::DirectEditRequestOutcome;
    use thought_mcp::{ActorRef, ReviewerAccess, ReviewerClient, Workspace};

    #[tokio::test]
    async fn closing_the_transport_session_revokes_only_its_grants() {
        let workspace = Arc::new(Workspace::open_in_memory().unwrap());
        let document = workspace
            .create_document("Draft", &ActorRef::editor())
            .unwrap();
        let directory = tempfile::tempdir().unwrap();
        let credentials = CredentialFiles::new(directory.path());
        let reviewers = Arc::new(ConnectionRegistry::new(workspace, credentials.clone()));
        let connection = reviewers
            .create(
                ReviewerClient::Codex,
                "Reviewer".into(),
                ReviewerAccess::all(),
                10,
            )
            .unwrap();
        let credential = credentials.read(&connection.id).unwrap();
        let principal = reviewers.authenticate(&credential, 11).unwrap().unwrap();
        let sessions = ReviewerSessionManager::new(reviewers.clone());
        let (first_session, _first_transport) = sessions.create_session().await.unwrap();
        let (second_session, _second_transport) = sessions.create_session().await.unwrap();

        let requested = reviewers
            .request_direct_edit(
                &principal,
                &document.doc_id,
                first_session.as_ref(),
                Some("reported-model"),
                12,
            )
            .unwrap();
        let DirectEditRequestOutcome::Pending { request } = requested else {
            panic!("expected a pending request")
        };
        reviewers
            .approve_direct_edit(&document.doc_id, &request.request_id, 13)
            .unwrap();
        let second_request = reviewers
            .request_direct_edit(
                &principal,
                &document.doc_id,
                second_session.as_ref(),
                Some("reported-model"),
                14,
            )
            .unwrap();
        assert!(matches!(
            second_request,
            DirectEditRequestOutcome::Pending { .. }
        ));

        assert!(
            reviewers
                .authorize_session(
                    &principal,
                    ReviewerOperation::Edit,
                    Some(&document.doc_id),
                    Some(first_session.as_ref()),
                )
                .is_ok()
        );
        assert!(
            reviewers
                .authorize_session(
                    &principal,
                    ReviewerOperation::Edit,
                    Some(&document.doc_id),
                    Some(second_session.as_ref()),
                )
                .is_err()
        );

        sessions.close_session(&first_session).await.unwrap();
        assert!(
            reviewers
                .authorize_session(
                    &principal,
                    ReviewerOperation::Edit,
                    Some(&document.doc_id),
                    Some(first_session.as_ref()),
                )
                .is_err()
        );
        assert!(
            reviewers
                .all_direct_edit_access()
                .unwrap()
                .grants
                .is_empty()
        );
        assert_eq!(
            reviewers.all_direct_edit_access().unwrap().requests.len(),
            1,
            "closing one session must not remove another session's request"
        );

        sessions.close_session(&second_session).await.unwrap();
        let access = reviewers.all_direct_edit_access().unwrap();
        assert!(access.requests.is_empty());
        assert!(access.grants.is_empty());
    }
}
