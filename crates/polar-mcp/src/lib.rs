//! The agent-facing tool surface (M1.6), independent of transport.
//!
//! Keeping this layer free of HTTP is what lets M1's acceptance criteria be
//! tested with no window, no server, and no client — which is the whole claim
//! of AD-2.

mod workspace;

pub use workspace::{
    ActorRef, ActorSummary, BlockSpan, DocumentSummary, DocumentView, EditOutcome, SearchHit, TextEdit,
    Workspace, WorkspaceError,
};
