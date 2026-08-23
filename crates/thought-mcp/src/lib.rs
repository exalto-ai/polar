//! The agent-facing tool surface (M1.6), independent of transport.
//!
//! Keeping this layer free of HTTP is what lets M1's acceptance criteria be
//! tested with no window, no server, and no client — which is the whole claim
//! of AD-2.

mod workspace;

/// The actor id every editor window writes under, named here so the window can
/// be told who it is rather than hardcoding a string that `sync.rs` owns.
pub const EDITOR_ACTOR_ID: &str = "human:editor";

pub use workspace::{
    ActorRef, ActorSummary, BlockAttribution, BlockSpan, DocumentSummary, DocumentView,
    EditOutcome, SearchHit, TextEdit, Workspace, WorkspaceError,
};

#[cfg(test)]
mod tests {
    use super::{ActorRef, EDITOR_ACTOR_ID};

    /// A constant that has to match a value built somewhere else is a drift
    /// waiting to happen, so it is checked rather than trusted.
    #[test]
    fn the_editor_constant_matches_the_actor_it_names() {
        assert_eq!(ActorRef::editor().id, EDITOR_ACTOR_ID);
    }
}
