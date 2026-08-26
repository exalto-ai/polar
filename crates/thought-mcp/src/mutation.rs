//! Trusted metadata attached to one canonical document mutation.
//!
//! Actor identity, input ingress, and evidence assurance stay separate. The
//! transport chooses one of these constructors. External tool arguments never
//! select an assurance level directly.

use crate::provenance_hash::EventAction;
use thought_provenance::{Assurance, Ingress, SourceDescriptor, SourceId};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationContext {
    action: EventAction,
    ingress: Ingress,
    assurance: Assurance,
    source_label: String,
    group_key: String,
    connection_id: Option<String>,
    provider: Option<String>,
    requested_model: Option<String>,
    reported_model: Option<String>,
    evidence_ref: Option<String>,
    suggestion_id: Option<String>,
    client_event_id: Option<String>,
}

impl MutationContext {
    pub fn entered() -> Self {
        Self::local(Ingress::Entered, "Written here")
    }

    pub fn pasted() -> Self {
        Self::local(Ingress::Pasted, "Pasted")
    }

    pub fn imported() -> Self {
        Self::local(Ingress::Imported, "Imported")
    }

    pub fn command() -> Self {
        Self::local(Ingress::Command, "Edited here")
    }

    /// A current mutation whose editor interaction could not be classified.
    pub fn unknown() -> Self {
        let source_label = "Unclassified change";
        Self::new(
            EventAction::Edit,
            Ingress::Unknown,
            Assurance::Unknown,
            source_label,
            source_group_key(
                Ingress::Unknown,
                Assurance::Unknown,
                source_label,
                None,
                None,
            ),
        )
    }

    /// A conservative seed for content created before detailed provenance.
    pub fn legacy_seed() -> Self {
        let source_label = "Legacy content";
        Self::new(
            EventAction::LegacySeed,
            Ingress::LegacyUnknown,
            Assurance::Unknown,
            source_label,
            source_group_key(
                Ingress::LegacyUnknown,
                Assurance::Unknown,
                source_label,
                None,
                None,
            ),
        )
    }

    /// A change observed through a configured external reviewer connection.
    pub fn mcp_reported(
        reviewer_label: impl Into<String>,
        connection_id: Option<String>,
        provider: Option<String>,
        reported_model: Option<String>,
    ) -> Self {
        let reviewer_label = reviewer_label.into();
        let source_label = format!("{reviewer_label} (reported)");
        let group_key = source_group_key(
            Ingress::Mcp,
            Assurance::Reported,
            &source_label,
            connection_id.as_deref(),
            provider.as_deref(),
        );
        let mut context = Self::new(
            EventAction::Edit,
            Ingress::Mcp,
            Assurance::Reported,
            source_label,
            group_key,
        );
        context.connection_id = connection_id;
        context.provider = provider;
        context.reported_model = reported_model;
        context
    }

    /// Test-only shape for the future provider verifier boundary. Production
    /// code cannot construct verified provenance until that verifier exists.
    #[cfg(test)]
    pub(crate) fn api_verified(
        provider_label: impl Into<String>,
        provider: impl Into<String>,
        requested_model: Option<String>,
        reported_model: Option<String>,
        evidence_ref: impl Into<String>,
    ) -> Self {
        let provider_label = provider_label.into();
        let provider = provider.into();
        let source_label = format!("{provider_label} (verified)");
        let group_key = source_group_key(
            Ingress::Api,
            Assurance::Verified,
            &source_label,
            None,
            Some(&provider),
        );
        let mut context = Self::new(
            EventAction::Edit,
            Ingress::Api,
            Assurance::Verified,
            source_label,
            group_key,
        );
        context.provider = Some(provider);
        context.requested_model = requested_model;
        context.reported_model = reported_model;
        context.evidence_ref = Some(evidence_ref.into());
        context
    }

    fn local(ingress: Ingress, label: &'static str) -> Self {
        Self::new(
            EventAction::Edit,
            ingress,
            Assurance::Observed,
            label,
            source_group_key(ingress, Assurance::Observed, label, None, None),
        )
    }

    fn new(
        action: EventAction,
        ingress: Ingress,
        assurance: Assurance,
        source_label: impl Into<String>,
        group_key: impl Into<String>,
    ) -> Self {
        Self {
            action,
            ingress,
            assurance,
            source_label: source_label.into(),
            group_key: group_key.into(),
            connection_id: None,
            provider: None,
            requested_model: None,
            reported_model: None,
            evidence_ref: None,
            suggestion_id: None,
            client_event_id: None,
        }
    }

    pub fn source(&self, event_id: SourceId) -> SourceDescriptor {
        SourceDescriptor::new(
            event_id,
            self.group_key.clone(),
            self.source_label.clone(),
            self.ingress,
            self.assurance,
        )
    }

    pub fn action(&self) -> EventAction {
        self.action
    }

    pub fn ingress(&self) -> Ingress {
        self.ingress
    }

    pub fn assurance(&self) -> Assurance {
        self.assurance
    }

    pub fn source_label(&self) -> &str {
        &self.source_label
    }

    pub fn group_key(&self) -> &str {
        &self.group_key
    }

    pub fn connection_id(&self) -> Option<&str> {
        self.connection_id.as_deref()
    }

    pub fn provider(&self) -> Option<&str> {
        self.provider.as_deref()
    }

    pub fn requested_model(&self) -> Option<&str> {
        self.requested_model.as_deref()
    }

    pub fn reported_model(&self) -> Option<&str> {
        self.reported_model.as_deref()
    }

    pub fn evidence_ref(&self) -> Option<&str> {
        self.evidence_ref.as_deref()
    }

    pub fn suggestion_id(&self) -> Option<&str> {
        self.suggestion_id.as_deref()
    }

    pub fn client_event_id(&self) -> Option<&str> {
        self.client_event_id.as_deref()
    }

    /// Bind one transport-generated idempotency key to this mutation.
    ///
    /// The editor capability owns this value. Public MCP tool arguments never
    /// select it, because reusing another event's key could otherwise turn a
    /// different mutation into a misleading retry.
    pub fn with_client_event_id(mut self, client_event_id: String) -> Self {
        self.client_event_id = Some(client_event_id);
        self
    }
}

pub(crate) fn source_group_key(
    ingress: Ingress,
    assurance: Assurance,
    source_label: &str,
    connection_id: Option<&str>,
    provider: Option<&str>,
) -> String {
    match ingress {
        Ingress::Entered => "local:written".into(),
        Ingress::Command => "local:edited".into(),
        Ingress::Pasted => "local:pasted".into(),
        Ingress::Imported => "local:imported".into(),
        Ingress::Unknown => "local:unclassified".into(),
        Ingress::LegacyUnknown => "legacy:unknown".into(),
        Ingress::Mcp => connection_id
            .and_then(nonempty)
            .map(|id| format!("mcp:connection:{id}"))
            .unwrap_or_else(|| format!("mcp:label:{}", source_label.trim())),
        Ingress::Api => provider
            .and_then(nonempty)
            .map(|name| format!("api:provider:{name}"))
            .unwrap_or_else(|| format!("api:label:{}", source_label.trim())),
        Ingress::Suggestion => {
            let assurance = assurance_name(assurance);
            if let Some(id) = connection_id.and_then(nonempty) {
                format!("suggestion:{assurance}:connection:{id}")
            } else if let Some(name) = provider.and_then(nonempty) {
                format!("suggestion:{assurance}:provider:{name}")
            } else {
                format!("suggestion:{assurance}:label:{}", source_label.trim())
            }
        }
    }
}

fn nonempty(value: &str) -> Option<&str> {
    let value = value.trim();
    (!value.is_empty()).then_some(value)
}

pub(crate) fn action_name(action: EventAction) -> &'static str {
    match action {
        EventAction::Edit => "edit",
        EventAction::Trash => "trash",
        EventAction::Restore => "restore",
        EventAction::LegacySeed => "legacy_seed",
        EventAction::Suggestion => "suggestion",
        EventAction::Accept => "accept",
        EventAction::Reject => "reject",
    }
}

pub(crate) fn ingress_name(ingress: Ingress) -> &'static str {
    match ingress {
        Ingress::Entered => "entered",
        Ingress::Command => "command",
        Ingress::Pasted => "pasted",
        Ingress::Imported => "imported",
        Ingress::Mcp => "mcp",
        Ingress::Api => "api",
        Ingress::Suggestion => "suggestion",
        Ingress::Unknown => "unknown",
        Ingress::LegacyUnknown => "legacy_unknown",
    }
}

pub(crate) fn assurance_name(assurance: Assurance) -> &'static str {
    match assurance {
        Assurance::Observed => "observed",
        Assurance::Reported => "reported",
        Assurance::Verified => "verified",
        Assurance::Unknown => "unknown",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_sources_never_claim_an_ai_identity() {
        let cases = [
            MutationContext::entered(),
            MutationContext::pasted(),
            MutationContext::imported(),
            MutationContext::command(),
            MutationContext::unknown(),
        ];
        assert_eq!(
            cases
                .iter()
                .map(|context| (context.ingress(), context.assurance()))
                .collect::<Vec<_>>(),
            vec![
                (Ingress::Entered, Assurance::Observed),
                (Ingress::Pasted, Assurance::Observed),
                (Ingress::Imported, Assurance::Observed),
                (Ingress::Command, Assurance::Observed),
                (Ingress::Unknown, Assurance::Unknown),
            ]
        );
        assert_eq!(
            cases
                .iter()
                .map(|context| context.group_key())
                .collect::<Vec<_>>(),
            vec![
                "local:written",
                "local:pasted",
                "local:imported",
                "local:edited",
                "local:unclassified",
            ]
        );
    }

    #[test]
    fn external_and_provider_paths_have_different_assurance() {
        let reported = MutationContext::mcp_reported(
            "Claude",
            Some("connection-1".into()),
            Some("anthropic".into()),
            Some("reported-model".into()),
        );
        let verified = MutationContext::api_verified(
            "Claude",
            "anthropic",
            Some("requested-model".into()),
            Some("reported-model".into()),
            "trace-1",
        );

        assert_eq!(reported.source(SourceId(1)).label, "Claude (reported)");
        assert_eq!(reported.source_label(), "Claude (reported)");
        assert_eq!(reported.group_key(), "mcp:connection:connection-1");
        assert_eq!(reported.assurance(), Assurance::Reported);
        assert_eq!(verified.source(SourceId(2)).label, "Claude (verified)");
        assert_eq!(verified.assurance(), Assurance::Verified);
    }

    #[test]
    fn mcp_group_identity_prefers_the_connection_over_the_display_label() {
        let first = MutationContext::mcp_reported("Claude", Some("reviewer-a".into()), None, None);
        let renamed =
            MutationContext::mcp_reported("Claude Code", Some("reviewer-a".into()), None, None);
        let second = MutationContext::mcp_reported("Claude", Some("reviewer-b".into()), None, None);

        assert_eq!(first.group_key(), renamed.group_key());
        assert_ne!(first.group_key(), second.group_key());
    }

    #[test]
    fn client_event_ids_do_not_change_source_identity() {
        let context = MutationContext::entered().with_client_event_id("window-1:42".into());
        assert_eq!(context.client_event_id(), Some("window-1:42"));
        assert_eq!(context.group_key(), "local:written");
        assert_eq!(context.assurance(), Assurance::Observed);
    }
}
