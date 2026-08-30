use thought_provenance::{Alignment, Assurance, Ingress, SourceDescriptor, SourceId};

#[derive(Debug, Clone)]
pub struct MutationContext {
    ingress: Ingress,
    assurance: Assurance,
    alignment: Alignment,
    group_key: String,
    source_label: String,
}

impl MutationContext {
    pub fn entered() -> Self {
        Self::local(
            Ingress::Entered,
            "local:written",
            "Written here",
            Alignment::Inferred,
        )
    }

    pub fn imported() -> Self {
        Self::local(
            Ingress::Imported,
            "local:imported",
            "Imported",
            Alignment::Exact,
        )
    }

    pub fn command() -> Self {
        Self::local(
            Ingress::Command,
            "local:command",
            "Edited here",
            Alignment::Inferred,
        )
    }

    pub fn unknown() -> Self {
        Self {
            ingress: Ingress::Unknown,
            assurance: Assurance::Unknown,
            alignment: Alignment::Unknown,
            group_key: "current:unknown".into(),
            source_label: "Unclassified change".into(),
        }
    }

    pub fn legacy_unknown() -> Self {
        let mut context = Self::unknown();
        context.ingress = Ingress::LegacyUnknown;
        context.group_key = "legacy:unknown".into();
        context.source_label = "Legacy content".into();
        context
    }

    pub fn mcp(label: impl Into<String>) -> Self {
        let label = label.into();
        Self {
            ingress: Ingress::Mcp,
            assurance: Assurance::Reported,
            alignment: Alignment::Inferred,
            group_key: format!("mcp:{}", label.to_lowercase().replace(' ', "-")),
            source_label: label,
        }
    }

    pub fn mcp_connection(label: impl Into<String>, connection_id: &str) -> Self {
        Self {
            ingress: Ingress::Mcp,
            assurance: Assurance::Reported,
            alignment: Alignment::Inferred,
            group_key: format!("mcp:connection:{connection_id}"),
            source_label: label.into(),
        }
    }

    fn local(ingress: Ingress, group_key: &str, label: &str, alignment: Alignment) -> Self {
        Self {
            ingress,
            assurance: Assurance::Observed,
            alignment,
            group_key: group_key.into(),
            source_label: label.into(),
        }
    }

    pub fn with_alignment(mut self, alignment: Alignment) -> Self {
        self.alignment = alignment;
        self
    }

    pub fn source(&self, id: SourceId) -> SourceDescriptor {
        SourceDescriptor::new(
            id,
            self.group_key.clone(),
            self.source_label.clone(),
            self.ingress,
            self.assurance,
            self.alignment,
        )
    }

    pub(crate) fn ingress(&self) -> Ingress {
        self.ingress
    }
    pub(crate) fn assurance(&self) -> Assurance {
        self.assurance
    }
    pub(crate) fn alignment(&self) -> Alignment {
        self.alignment
    }
    pub(crate) fn group_key(&self) -> &str {
        &self.group_key
    }
    pub(crate) fn source_label(&self) -> &str {
        &self.source_label
    }
}
