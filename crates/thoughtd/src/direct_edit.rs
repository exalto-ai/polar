//! Ephemeral, user-approved direct-edit access for configured reviewer sessions.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};
use thought_mcp::ReviewerClient;

const REQUEST_LIFETIME: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub(crate) struct DirectEditKey {
    pub connection_id: String,
    pub credential_hash: [u8; 32],
    pub session_id: String,
    pub document_id: String,
}

#[derive(Debug, Clone)]
pub(crate) struct ReviewerSnapshot {
    pub display_label: String,
    pub client: ReviewerClient,
    pub reported_model: Option<String>,
    pub document_title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingIdentity {
    pub connection_id: String,
    pub credential_hash: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DirectEditRequest {
    pub request_id: String,
    pub connection_id: String,
    pub document_id: String,
    pub document_title: String,
    pub display_label: String,
    pub client: ReviewerClient,
    pub reported_model: Option<String>,
    pub requested_at: i64,
    pub expires_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DirectEditGrant {
    pub grant_id: String,
    pub connection_id: String,
    pub document_id: String,
    pub document_title: String,
    pub display_label: String,
    pub client: ReviewerClient,
    pub reported_model: Option<String>,
    pub granted_at: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DirectEditAccess {
    pub requests: Vec<DirectEditRequest>,
    pub grants: Vec<DirectEditGrant>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum DirectEditRequestOutcome {
    Pending { request: DirectEditRequest },
    Active { grant: DirectEditGrant },
    Denied { retry_at: i64 },
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct DirectEditDenial {
    pub request_id: String,
    pub retry_at: i64,
}

#[derive(Debug, Clone)]
enum Entry {
    Pending {
        request: DirectEditRequest,
        until: Instant,
    },
    Active {
        grant: DirectEditGrant,
    },
    Denied {
        retry_at: i64,
        until: Instant,
    },
}

impl Entry {
    fn expired(&self, now: Instant) -> bool {
        match self {
            Self::Pending { until, .. } | Self::Denied { until, .. } => *until <= now,
            Self::Active { .. } => false,
        }
    }
}

#[derive(Default)]
struct State {
    entries: HashMap<DirectEditKey, Entry>,
}

impl State {
    fn prune(&mut self, now: Instant) {
        self.entries.retain(|_, entry| !entry.expired(now));
    }
}

#[derive(Default)]
pub(crate) struct DirectEditRegistry {
    state: Mutex<State>,
}

impl DirectEditRegistry {
    pub fn request(
        &self,
        key: DirectEditKey,
        reviewer: ReviewerSnapshot,
        request_id: String,
        now: Instant,
        now_ms: i64,
    ) -> Result<DirectEditRequestOutcome, &'static str> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "direct edit access is unavailable")?;
        state.prune(now);
        if let Some(existing) = state.entries.get(&key) {
            return Ok(match existing {
                Entry::Pending { request, .. } => DirectEditRequestOutcome::Pending {
                    request: request.clone(),
                },
                Entry::Active { grant, .. } => DirectEditRequestOutcome::Active {
                    grant: grant.clone(),
                },
                Entry::Denied { retry_at, .. } => DirectEditRequestOutcome::Denied {
                    retry_at: *retry_at,
                },
            });
        }
        let expires_at = now_ms.saturating_add(REQUEST_LIFETIME.as_millis() as i64);
        let request = DirectEditRequest {
            request_id,
            connection_id: key.connection_id.clone(),
            document_id: key.document_id.clone(),
            document_title: reviewer.document_title,
            display_label: reviewer.display_label,
            client: reviewer.client,
            reported_model: reviewer.reported_model,
            requested_at: now_ms,
            expires_at,
        };
        state.entries.insert(
            key,
            Entry::Pending {
                request: request.clone(),
                until: now + REQUEST_LIFETIME,
            },
        );
        Ok(DirectEditRequestOutcome::Pending { request })
    }

    pub fn access(
        &self,
        document_id: &str,
        now: Instant,
    ) -> Result<DirectEditAccess, &'static str> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "direct edit access is unavailable")?;
        state.prune(now);
        let mut requests = Vec::new();
        let mut grants = Vec::new();
        for (key, entry) in &state.entries {
            if key.document_id != document_id {
                continue;
            }
            match entry {
                Entry::Pending { request, .. } => requests.push(request.clone()),
                Entry::Active { grant, .. } => grants.push(grant.clone()),
                Entry::Denied { .. } => {}
            }
        }
        requests.sort_by(|left, right| {
            (left.requested_at, &left.request_id).cmp(&(right.requested_at, &right.request_id))
        });
        grants.sort_by(|left, right| {
            (left.granted_at, &left.grant_id).cmp(&(right.granted_at, &right.grant_id))
        });
        Ok(DirectEditAccess { requests, grants })
    }

    pub fn all_access(&self, now: Instant) -> Result<DirectEditAccess, &'static str> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "direct edit access is unavailable")?;
        state.prune(now);
        let mut requests = Vec::new();
        let mut grants = Vec::new();
        for entry in state.entries.values() {
            match entry {
                Entry::Pending { request, .. } => requests.push(request.clone()),
                Entry::Active { grant, .. } => grants.push(grant.clone()),
                Entry::Denied { .. } => {}
            }
        }
        requests.sort_by(|left, right| {
            (left.requested_at, &left.request_id).cmp(&(right.requested_at, &right.request_id))
        });
        grants.sort_by(|left, right| {
            (left.granted_at, &left.grant_id).cmp(&(right.granted_at, &right.grant_id))
        });
        Ok(DirectEditAccess { requests, grants })
    }

    pub fn pending_identity(
        &self,
        document_id: &str,
        request_id: &str,
        now: Instant,
    ) -> Result<PendingIdentity, &'static str> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "direct edit access is unavailable")?;
        state.prune(now);
        state
            .entries
            .iter()
            .find_map(|(key, entry)| match entry {
                Entry::Pending { request, .. }
                    if key.document_id == document_id && request.request_id == request_id =>
                {
                    Some(PendingIdentity {
                        connection_id: key.connection_id.clone(),
                        credential_hash: key.credential_hash,
                    })
                }
                _ => None,
            })
            .ok_or("direct edit request was not found or expired")
    }

    pub fn approve(
        &self,
        document_id: &str,
        request_id: &str,
        grant_id: String,
        now: Instant,
        now_ms: i64,
    ) -> Result<DirectEditGrant, &'static str> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "direct edit access is unavailable")?;
        state.prune(now);
        let key = state
            .entries
            .iter()
            .find_map(|(key, entry)| match entry {
                Entry::Pending { request, .. }
                    if key.document_id == document_id && request.request_id == request_id =>
                {
                    Some(key.clone())
                }
                _ => None,
            })
            .ok_or("direct edit request was not found or expired")?;
        let Entry::Pending { request, .. } = state
            .entries
            .remove(&key)
            .ok_or("direct edit request was not found or expired")?
        else {
            return Err("direct edit request is no longer pending");
        };
        let grant = DirectEditGrant {
            grant_id,
            connection_id: request.connection_id,
            document_id: request.document_id,
            document_title: request.document_title,
            display_label: request.display_label,
            client: request.client,
            reported_model: request.reported_model,
            granted_at: now_ms,
        };
        state.entries.insert(
            key,
            Entry::Active {
                grant: grant.clone(),
            },
        );
        Ok(grant)
    }

    pub fn deny(
        &self,
        document_id: &str,
        request_id: &str,
        now: Instant,
    ) -> Result<DirectEditDenial, &'static str> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "direct edit access is unavailable")?;
        state.prune(now);
        let key = state
            .entries
            .iter()
            .find_map(|(key, entry)| match entry {
                Entry::Pending { request, .. }
                    if key.document_id == document_id && request.request_id == request_id =>
                {
                    Some(key.clone())
                }
                _ => None,
            })
            .ok_or("direct edit request was not found or expired")?;
        let Entry::Pending { request, until } = state
            .entries
            .remove(&key)
            .ok_or("direct edit request was not found or expired")?
        else {
            return Err("direct edit request is no longer pending");
        };
        let denial = DirectEditDenial {
            request_id: request.request_id,
            retry_at: request.expires_at,
        };
        state.entries.insert(
            key,
            Entry::Denied {
                retry_at: denial.retry_at,
                until,
            },
        );
        Ok(denial)
    }

    pub fn revoke(
        &self,
        document_id: &str,
        grant_id: &str,
        now: Instant,
    ) -> Result<DirectEditGrant, &'static str> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "direct edit access is unavailable")?;
        state.prune(now);
        let key = state
            .entries
            .iter()
            .find_map(|(key, entry)| match entry {
                Entry::Active { grant, .. }
                    if key.document_id == document_id && grant.grant_id == grant_id =>
                {
                    Some(key.clone())
                }
                _ => None,
            })
            .ok_or("direct edit grant was not found or expired")?;
        let Entry::Active { grant, .. } = state
            .entries
            .remove(&key)
            .ok_or("direct edit grant was not found or expired")?
        else {
            return Err("direct edit grant is no longer active");
        };
        Ok(grant)
    }

    pub fn is_active(&self, key: &DirectEditKey, now: Instant) -> Result<bool, &'static str> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "direct edit access is unavailable")?;
        state.prune(now);
        Ok(matches!(state.entries.get(key), Some(Entry::Active { .. })))
    }

    pub fn revoke_connection(&self, connection_id: &str) -> Result<(), &'static str> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "direct edit access is unavailable")?;
        state
            .entries
            .retain(|key, _| key.connection_id != connection_id);
        Ok(())
    }

    pub fn revoke_session(&self, session_id: &str) -> Result<(), &'static str> {
        let mut state = self
            .state
            .lock()
            .map_err(|_| "direct edit access is unavailable")?;
        state.entries.retain(|key, _| key.session_id != session_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(session: &str) -> DirectEditKey {
        DirectEditKey {
            connection_id: "reviewer-1".into(),
            credential_hash: [7; 32],
            session_id: session.into(),
            document_id: "doc-1".into(),
        }
    }

    fn reviewer() -> ReviewerSnapshot {
        ReviewerSnapshot {
            display_label: "Writing partner".into(),
            client: ReviewerClient::Codex,
            reported_model: Some("reported-model".into()),
            document_title: "Draft".into(),
        }
    }

    #[test]
    fn grant_is_bound_to_one_session_until_that_session_closes() {
        let registry = DirectEditRegistry::default();
        let now = Instant::now();
        let first = registry
            .request(key("session-1"), reviewer(), "request-1".into(), now, 1_000)
            .unwrap();
        let duplicate = registry
            .request(key("session-1"), reviewer(), "request-2".into(), now, 1_000)
            .unwrap();
        assert_eq!(first, duplicate);

        registry
            .approve("doc-1", "request-1", "grant-1".into(), now, 2_000)
            .unwrap();
        assert!(registry.is_active(&key("session-1"), now).unwrap());
        assert!(!registry.is_active(&key("session-2"), now).unwrap());
        let mut wrong_document = key("session-1");
        wrong_document.document_id = "doc-2".into();
        assert!(!registry.is_active(&wrong_document, now).unwrap());
        let mut wrong_credential = key("session-1");
        wrong_credential.credential_hash = [8; 32];
        assert!(!registry.is_active(&wrong_credential, now).unwrap());
        assert!(
            registry
                .is_active(
                    &key("session-1"),
                    now + Duration::from_secs(365 * 24 * 60 * 60),
                )
                .unwrap()
        );
        registry.revoke_session("session-1").unwrap();
        assert!(!registry.is_active(&key("session-1"), now).unwrap());
    }

    #[test]
    fn denial_suppresses_repeat_requests_until_the_request_window_ends() {
        let registry = DirectEditRegistry::default();
        let now = Instant::now();
        registry
            .request(key("session-1"), reviewer(), "request-1".into(), now, 1_000)
            .unwrap();
        registry.deny("doc-1", "request-1", now).unwrap();
        assert_eq!(
            registry
                .request(key("session-1"), reviewer(), "request-2".into(), now, 1_000)
                .unwrap(),
            DirectEditRequestOutcome::Denied { retry_at: 301_000 }
        );
        assert!(matches!(
            registry
                .request(
                    key("session-1"),
                    reviewer(),
                    "request-3".into(),
                    now + REQUEST_LIFETIME,
                    301_000,
                )
                .unwrap(),
            DirectEditRequestOutcome::Pending { .. }
        ));
    }

    #[test]
    fn connection_changes_revoke_pending_and_active_access() {
        let registry = DirectEditRegistry::default();
        let now = Instant::now();
        registry
            .request(key("session-1"), reviewer(), "request-1".into(), now, 1_000)
            .unwrap();
        registry
            .approve("doc-1", "request-1", "grant-1".into(), now, 2_000)
            .unwrap();
        registry.revoke_connection("reviewer-1").unwrap();
        assert!(!registry.is_active(&key("session-1"), now).unwrap());
        assert_eq!(
            registry.access("doc-1", now).unwrap(),
            DirectEditAccess {
                requests: vec![],
                grants: vec![]
            }
        );
    }
}
