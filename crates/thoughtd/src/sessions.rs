//! MCP session lifecycle integrated with daemon authorization state.
//!
//! RMCP owns explicit HTTP DELETE handling and can also close a session when
//! its worker exits. Wrapping its local manager gives both paths one cleanup
//! hook, while shutdown can stop new sessions and drain the existing workers.

use futures::Stream;
use rmcp::{
    model::{ClientJsonRpcMessage, ServerJsonRpcMessage},
    transport::streamable_http_server::session::{
        EventStore, RestoreOutcome, ServerSseMessage, SessionId, SessionManager,
        local::{LocalSessionManager, SessionError},
    },
};
use std::{io, sync::Arc};
use tokio::sync::RwLock;

type SessionClosed = Arc<dyn Fn(&SessionId) + Send + Sync>;

/// A local RMCP session manager with deterministic close notifications.
pub struct LifecycleSessionManager {
    inner: Arc<LocalSessionManager>,
    /// Creation holds the read side through insertion. Shutdown takes the
    /// write side before its snapshot, so no session can appear after drain.
    shutting_down: RwLock<bool>,
    session_closed: SessionClosed,
}

impl LifecycleSessionManager {
    pub fn new(
        inner: Arc<LocalSessionManager>,
        session_closed: impl Fn(&SessionId) + Send + Sync + 'static,
    ) -> Self {
        Self {
            inner,
            shutting_down: RwLock::new(false),
            session_closed: Arc::new(session_closed),
        }
    }

    /// Stop accepting new sessions, close every current worker, and notify
    /// authorization state for each removed session. This remains idempotent.
    pub async fn shutdown(&self) -> usize {
        let mut shutting_down = self.shutting_down.write().await;
        *shutting_down = true;

        let session_ids = self
            .inner
            .sessions
            .read()
            .await
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        for session_id in &session_ids {
            if let Err(error) = self.close_and_notify(session_id).await {
                tracing::warn!(%session_id, %error, "could not close MCP session during shutdown");
            }
        }
        session_ids.len()
    }

    async fn close_and_notify(&self, id: &SessionId) -> Result<(), io::Error> {
        let handle = self.inner.sessions.write().await.remove(id);
        // Notify immediately after the transport entry is gone. This ordering
        // closes the initialize-response race, and cancellation while awaiting
        // the worker cannot leave an authorization-only session behind.
        (self.session_closed)(id);
        let Some(handle) = handle else {
            return Ok(());
        };
        match handle.close().await {
            Ok(()) | Err(SessionError::SessionServiceTerminated) => Ok(()),
            Err(error) => Err(io::Error::other(error)),
        }
    }

    fn shutdown_error() -> io::Error {
        io::Error::new(
            io::ErrorKind::NotConnected,
            "MCP session manager is shutting down",
        )
    }
}

impl SessionManager for LifecycleSessionManager {
    type Error = io::Error;
    type Transport = <LocalSessionManager as SessionManager>::Transport;

    async fn create_session(&self) -> Result<(SessionId, Self::Transport), Self::Error> {
        let shutting_down = self.shutting_down.read().await;
        if *shutting_down {
            return Err(Self::shutdown_error());
        }
        let created = self.inner.create_session().await.map_err(io::Error::other);
        drop(shutting_down);
        created
    }

    async fn initialize_session(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<ServerJsonRpcMessage, Self::Error> {
        self.inner
            .initialize_session(id, message)
            .await
            .map_err(io::Error::other)
    }

    async fn has_session(&self, id: &SessionId) -> Result<bool, Self::Error> {
        self.inner.has_session(id).await.map_err(io::Error::other)
    }

    async fn close_session(&self, id: &SessionId) -> Result<(), Self::Error> {
        self.close_and_notify(id).await
    }

    async fn create_stream(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        self.inner
            .create_stream(id, message)
            .await
            .map_err(io::Error::other)
    }

    async fn accept_message(
        &self,
        id: &SessionId,
        message: ClientJsonRpcMessage,
    ) -> Result<(), Self::Error> {
        self.inner
            .accept_message(id, message)
            .await
            .map_err(io::Error::other)
    }

    async fn create_standalone_stream(
        &self,
        id: &SessionId,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        self.inner
            .create_standalone_stream(id)
            .await
            .map_err(io::Error::other)
    }

    async fn resume(
        &self,
        id: &SessionId,
        last_event_id: String,
    ) -> Result<impl Stream<Item = ServerSseMessage> + Send + Sync + 'static, Self::Error> {
        self.inner
            .resume(id, last_event_id)
            .await
            .map_err(io::Error::other)
    }

    async fn restore_session(
        &self,
        id: SessionId,
    ) -> Result<RestoreOutcome<Self::Transport>, Self::Error> {
        let shutting_down = self.shutting_down.read().await;
        if *shutting_down {
            return Err(Self::shutdown_error());
        }
        let restored = self
            .inner
            .restore_session(id)
            .await
            .map_err(io::Error::other);
        drop(shutting_down);
        restored
    }

    fn event_store(&self) -> Option<Arc<dyn EventStore>> {
        self.inner.event_store()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::connections::ConnectionRegistry;
    use thought_credentials::CredentialStore;
    use thought_mcp::Workspace;

    fn manager_and_registry() -> (
        Arc<LifecycleSessionManager>,
        Arc<ConnectionRegistry>,
        tempfile::TempDir,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let workspace = Arc::new(Workspace::open_in_memory().unwrap());
        let registry = Arc::new(ConnectionRegistry::new(
            workspace,
            CredentialStore::files(directory.path()),
        ));
        let weak_registry = Arc::downgrade(&registry);
        let manager = Arc::new(LifecycleSessionManager::new(
            Arc::new(LocalSessionManager::default()),
            move |session_id| {
                if let Some(registry) = weak_registry.upgrade() {
                    registry
                        .forget_session_binding(session_id.as_ref())
                        .unwrap();
                }
            },
        ));
        (manager, registry, directory)
    }

    #[tokio::test]
    async fn repeated_create_delete_cycles_remove_authorization_bindings() {
        let (manager, registry, _directory) = manager_and_registry();
        let principal = registry.internal_principal();

        // More than the per-principal binding cap proves closed sessions do
        // not accumulate and force unrelated transport eviction.
        for at in 0..80 {
            let (session_id, transport) = manager.create_session().await.unwrap();
            assert!(
                registry
                    .bind_session(session_id.as_ref(), &principal, None, at)
                    .unwrap()
            );
            assert!(
                registry
                    .session_matches(session_id.as_ref(), &principal, None, at)
                    .unwrap()
            );

            manager.close_session(&session_id).await.unwrap();
            assert!(
                !registry
                    .session_matches(session_id.as_ref(), &principal, None, at)
                    .unwrap()
            );
            assert!(!manager.has_session(&session_id).await.unwrap());
            drop(transport);
        }
    }

    #[tokio::test]
    async fn shutdown_drains_a_live_session_and_rejects_new_sessions() {
        let (manager, registry, _directory) = manager_and_registry();
        let principal = registry.internal_principal();
        let (session_id, transport) = manager.create_session().await.unwrap();
        assert!(
            registry
                .bind_session(session_id.as_ref(), &principal, None, 1)
                .unwrap()
        );
        let closed = tokio::time::timeout(std::time::Duration::from_secs(1), manager.shutdown())
            .await
            .expect("live session drain is bounded");
        assert_eq!(closed, 1);
        assert!(!manager.has_session(&session_id).await.unwrap());
        assert!(
            !registry
                .session_matches(session_id.as_ref(), &principal, None, 2)
                .unwrap()
        );
        assert!(manager.create_session().await.is_err());
        drop(transport);
    }
}
