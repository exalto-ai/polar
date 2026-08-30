use thought_core::SuggestionState;
use thought_mcp::{
    ActorRef, MutationContext, SuggestedChange, SuggestionError, Workspace, WorkspaceError,
};

fn reviewer() -> (ActorRef, MutationContext) {
    (
        ActorRef::reviewer(
            "reviewer-one",
            "Review bot",
            Some("reported-model"),
            Some("run-1"),
        ),
        MutationContext::mcp_connection("Configured for Codex (reported)", "reviewer-one"),
    )
}

fn propose_replace(
    workspace: &Workspace,
    doc_id: &str,
    block_id: &str,
    revision: &str,
) -> thought_mcp::SuggestionOutcome {
    let (actor, context) = reviewer();
    workspace
        .propose_suggestion(
            doc_id,
            "request-one",
            revision,
            &SuggestedChange::ReplaceText {
                block_id: block_id.to_string(),
                find: "Draft".into(),
                replace: "Final".into(),
                occurrence: Some(1),
            },
            Some("Use firmer wording"),
            Some("reported-model"),
            "reviewer-one",
            &actor,
            &context,
        )
        .unwrap()
}

#[test]
fn proposals_are_replicated_metadata_and_retry_by_request_id() {
    let workspace = Workspace::open_in_memory().unwrap();
    let document = workspace
        .create_document_from_markdown("", "# Draft\n\nBody", &ActorRef::editor())
        .unwrap();
    let before_version = document.version.clone();
    let proposal = propose_replace(
        &workspace,
        &document.doc_id,
        &document.blocks[0].block_id,
        &document.content_revision,
    );
    assert!(!proposal.replayed);
    assert_eq!(proposal.suggestion.state, SuggestionState::Pending);
    assert_eq!(proposal.content_revision, document.content_revision);

    let after = workspace.read_document(&document.doc_id).unwrap();
    assert_eq!(after.markdown, document.markdown);
    assert_eq!(after.content_revision, document.content_revision);
    assert_ne!(after.version, before_version);

    let retry = propose_replace(
        &workspace,
        &document.doc_id,
        &document.blocks[0].block_id,
        &document.content_revision,
    );
    assert!(retry.replayed);
    assert_eq!(
        retry.suggestion.suggestion_id,
        proposal.suggestion.suggestion_id
    );
    assert_eq!(
        workspace
            .list_suggestions(&document.doc_id)
            .unwrap()
            .suggestions
            .len(),
        1
    );
}

#[test]
fn acceptance_applies_the_normalized_patch_and_attributes_the_reviewer() {
    let workspace = Workspace::open_in_memory().unwrap();
    let document = workspace
        .create_document_from_markdown("", "# Draft\n\nBody", &ActorRef::editor())
        .unwrap();
    let proposal = propose_replace(
        &workspace,
        &document.doc_id,
        &document.blocks[0].block_id,
        &document.content_revision,
    );

    let accepted = workspace
        .accept_suggestion(
            &document.doc_id,
            &proposal.suggestion.suggestion_id,
            &ActorRef::editor(),
        )
        .unwrap();
    assert_eq!(accepted.suggestion.state, SuggestionState::Accepted);
    assert_eq!(
        workspace.read_document(&document.doc_id).unwrap().markdown,
        "# Final\n\nBody"
    );
    let attribution = workspace.block_provenance(&document.doc_id).unwrap();
    let heading = attribution
        .iter()
        .find(|block| block.block_id == document.blocks[0].block_id)
        .unwrap();
    assert_eq!(heading.touched_by, "reviewer:reviewer-one");

    assert!(matches!(
        workspace.reject_suggestion(
            &document.doc_id,
            &proposal.suggestion.suggestion_id,
            &ActorRef::editor(),
        ),
        Err(WorkspaceError::Suggestion(SuggestionError::AlreadyDecided(
            _
        )))
    ));
}

#[test]
fn content_changes_make_pending_suggestions_stale_without_a_merge_engine() {
    let workspace = Workspace::open_in_memory().unwrap();
    let document = workspace
        .create_document_from_markdown("", "# Draft\n\nBody", &ActorRef::editor())
        .unwrap();
    let proposal = propose_replace(
        &workspace,
        &document.doc_id,
        &document.blocks[0].block_id,
        &document.content_revision,
    );
    workspace
        .replace_block(
            &document.doc_id,
            &document.blocks[1].block_id,
            "Changed elsewhere",
            None,
            &ActorRef::editor(),
        )
        .unwrap();

    let listed = workspace.list_suggestions(&document.doc_id).unwrap();
    assert_eq!(listed.suggestions[0].state, SuggestionState::Stale);
    assert!(matches!(
        workspace.accept_suggestion(
            &document.doc_id,
            &proposal.suggestion.suggestion_id,
            &ActorRef::editor(),
        ),
        Err(WorkspaceError::Suggestion(
            SuggestionError::BaseRevisionMismatch { .. }
        ))
    ));
    assert!(
        workspace
            .read_document(&document.doc_id)
            .unwrap()
            .markdown
            .contains("Draft")
    );
}

#[test]
fn rejection_and_proposals_survive_a_cold_start() {
    let directory = tempfile::tempdir().unwrap();
    let database = directory.path().join("workspace.sqlite");
    let (doc_id, suggestion_id) = {
        let workspace = Workspace::open(&database).unwrap();
        let document = workspace
            .create_document_from_markdown("", "# Draft", &ActorRef::editor())
            .unwrap();
        let proposal = propose_replace(
            &workspace,
            &document.doc_id,
            &document.blocks[0].block_id,
            &document.content_revision,
        );
        workspace
            .reject_suggestion(
                &document.doc_id,
                &proposal.suggestion.suggestion_id,
                &ActorRef::editor(),
            )
            .unwrap();
        (document.doc_id, proposal.suggestion.suggestion_id)
    };

    let reopened = Workspace::open(&database).unwrap();
    let suggestions = reopened.list_suggestions(&doc_id).unwrap().suggestions;
    assert_eq!(suggestions.len(), 1);
    assert_eq!(suggestions[0].suggestion_id, suggestion_id);
    assert_eq!(suggestions[0].state, SuggestionState::Rejected);
}
