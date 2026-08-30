//! The MCP tool definitions (M1.6).
//!
//! Thin by design: every tool forwards to `thought-mcp`, which is where the
//! behaviour and the tests live. Nothing here should be worth testing, because
//! anything worth testing belongs one layer down where it can be tested without
//! a server.

use axum::http::request::Parts;
use rmcp::handler::server::tool::Extension;
use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{ErrorData, tool, tool_router};
use std::sync::Arc;
use thought_core::Position;
use thought_mcp::{ActorRef, MutationContext, SuggestedChange, TextEdit, Workspace};
use thoughtd::connections::{
    AuthenticatedPrincipal, AuthorizedRequest, ConnectionRegistry, ReviewerOperation,
};

#[derive(Clone)]
pub struct Thought {
    workspace: Arc<Workspace>,
    reviewers: Arc<ConnectionRegistry>,
}

/// Every tool failure passes through here, so this is the one place that can
/// notice them. Without it a failing tool is a JSON-RPC error the agent sees and
/// the daemon has no memory of, which is exactly the case someone reports later
/// as "it just stopped working".
fn failed(e: impl std::fmt::Display) -> ErrorData {
    let message = e.to_string();
    tracing::warn!(error = %message, "tool call failed");
    ErrorData::internal_error(message, None)
}

fn denied(error: impl std::fmt::Display) -> ErrorData {
    ErrorData::invalid_request(format!("reviewer authorization failed: {error}"), None)
}

fn principal(parts: &Parts) -> Result<AuthenticatedPrincipal, ErrorData> {
    parts
        .extensions
        .get::<AuthenticatedPrincipal>()
        .cloned()
        .ok_or_else(|| ErrorData::invalid_request("missing authenticated connection", None))
}

/// Every write names its caller. There is no anonymous edit path, because an
/// unattributed change cannot be shown in the activity feed or reverted as part
/// of a run (AD-6, AD-11).
#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct Caller {
    /// Stable name for the agent. Reused across reconnects — a per-connection
    /// identity would fragment one agent into many actors.
    #[serde(default)]
    pub agent: Option<String>,
    #[serde(default)]
    pub model: Option<String>,
    /// Groups one agent turn so it can be reverted as a unit.
    #[serde(default)]
    pub session: Option<String>,
}

impl Caller {
    fn internal_identity(&self) -> Result<(ActorRef, MutationContext), ErrorData> {
        let agent = self
            .agent
            .as_deref()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| ErrorData::invalid_params("agent is required", None))?;
        Ok((
            ActorRef::agent(agent, self.model.as_deref(), self.session.as_deref()),
            MutationContext::mcp(agent),
        ))
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct ListParams {
    #[serde(default = "default_limit")]
    pub limit: usize,
    /// List trashed documents instead of live ones. Deleting is soft, so this
    /// is how a document comes back.
    #[serde(default)]
    pub trashed: bool,
}

fn default_limit() -> usize {
    50
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct SearchParams {
    pub query: String,
    #[serde(default = "default_limit")]
    pub limit: usize,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct DocParams {
    pub doc_id: String,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct CreateParams {
    pub title: String,
    /// Optional Markdown used only while constructing a brand-new document.
    /// Existing documents still require block-addressed edits.
    #[serde(default)]
    pub initial_markdown: Option<String>,
    #[serde(flatten)]
    pub caller: Caller,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct ReplaceBlockParams {
    pub doc_id: String,
    pub block_id: String,
    /// CommonMark + GFM. Refused if it would produce an invalid document.
    pub markdown: String,
    /// The `version` from your last read. A stale value warns rather than
    /// failing — the CRDT merges regardless.
    #[serde(default)]
    pub version: Option<String>,
    #[serde(flatten)]
    pub caller: Caller,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct InsertBlocksParams {
    pub doc_id: String,
    /// A `block_id` to insert after, or `"start"` / `"end"`.
    #[serde(default)]
    pub after: Option<String>,
    pub markdown: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(flatten)]
    pub caller: Caller,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct DeleteBlockParams {
    pub doc_id: String,
    pub block_id: String,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(flatten)]
    pub caller: Caller,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct ReplaceTextParams {
    pub doc_id: String,
    pub block_id: String,
    pub find: String,
    pub replace: String,
    /// 1-based. Omit to replace every match.
    #[serde(default)]
    pub occurrence: Option<usize>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(flatten)]
    pub caller: Caller,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct DeleteDocumentParams {
    pub doc_id: String,
    /// `true` trashes it, `false` restores it.
    pub deleted: bool,
    #[serde(flatten)]
    pub caller: Caller,
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SuggestedChangeParams {
    ReplaceBlock {
        block_id: String,
        markdown: String,
    },
    InsertBlocks {
        /// A `block_id` to insert after, or `"start"` / `"end"`.
        #[serde(default)]
        after: Option<String>,
        markdown: String,
    },
    ReplaceText {
        block_id: String,
        find: String,
        replace: String,
        #[serde(default)]
        occurrence: Option<usize>,
    },
    DeleteBlock {
        block_id: String,
    },
}

impl From<SuggestedChangeParams> for SuggestedChange {
    fn from(value: SuggestedChangeParams) -> Self {
        match value {
            SuggestedChangeParams::ReplaceBlock { block_id, markdown } => {
                Self::ReplaceBlock { block_id, markdown }
            }
            SuggestedChangeParams::InsertBlocks { after, markdown } => {
                Self::InsertBlocks { after, markdown }
            }
            SuggestedChangeParams::ReplaceText {
                block_id,
                find,
                replace,
                occurrence,
            } => Self::ReplaceText {
                block_id,
                find,
                replace,
                occurrence,
            },
            SuggestedChangeParams::DeleteBlock { block_id } => Self::DeleteBlock { block_id },
        }
    }
}

#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct SuggestParams {
    pub doc_id: String,
    /// Stable retry key chosen by the caller. Reusing it returns the original proposal.
    pub request_id: String,
    /// The exact `content_revision` from `read_document`.
    pub content_revision: String,
    pub change: SuggestedChangeParams,
    #[serde(default)]
    pub explanation: Option<String>,
    #[serde(flatten)]
    pub caller: Caller,
}

#[tool_router(server_handler)]
impl Thought {
    pub fn new(workspace: Arc<Workspace>, reviewers: Arc<ConnectionRegistry>) -> Self {
        Thought {
            workspace,
            reviewers,
        }
    }

    fn authorize(
        &self,
        parts: &Parts,
        operation: ReviewerOperation,
        document_id: Option<&str>,
    ) -> Result<(AuthenticatedPrincipal, AuthorizedRequest), ErrorData> {
        let principal = principal(parts)?;
        let authorized = self
            .reviewers
            .authorize(&principal, operation, document_id)
            .map_err(denied)?;
        Ok((principal, authorized))
    }

    fn mutation_identity(
        &self,
        principal: &AuthenticatedPrincipal,
        authorized: &AuthorizedRequest,
        caller: &Caller,
    ) -> Result<(ActorRef, MutationContext), ErrorData> {
        if let Some(connection) = authorized.connection() {
            self.reviewers
                .note_reported_model(principal, caller.model.as_deref())
                .map_err(denied)?;
            return Ok((
                connection.reported_actor(caller.session.as_deref(), caller.model.as_deref()),
                connection.reported_mutation_context(),
            ));
        }
        caller.internal_identity()
    }

    #[tool(
        description = "List documents, most recently updated first. Pass `trashed: true` \
                       to list deleted ones instead — deleting is soft, and this is how a \
                       document is found again."
    )]
    fn list_documents(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(p): Parameters<ListParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        let (_, authorized) = self.authorize(&parts, ReviewerOperation::Read, None)?;
        let docs = self
            .workspace
            .list_documents_scoped(p.limit, p.trashed, authorized.selected_document())
            .map_err(failed)?;
        Ok(Json(serde_json::json!({ "documents": docs })))
    }

    #[tool(
        description = "Read a document as markdown, with a block_id and line range for each \
                       top-level block. Use those block_ids to edit; never rewrite the whole \
                       document."
    )]
    fn read_document(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(p): Parameters<DocParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        self.authorize(&parts, ReviewerOperation::Read, Some(&p.doc_id))?;
        let view = self.workspace.read_document(&p.doc_id).map_err(failed)?;
        Ok(Json(serde_json::to_value(view).map_err(failed)?))
    }

    #[tool(description = "List reviewer suggestions for one document and their current state.")]
    fn list_suggestions(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(p): Parameters<DocParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        self.authorize(&parts, ReviewerOperation::Read, Some(&p.doc_id))?;
        let suggestions = self.workspace.list_suggestions(&p.doc_id).map_err(failed)?;
        Ok(Json(serde_json::to_value(suggestions).map_err(failed)?))
    }

    #[tool(
        description = "Propose one block-addressed change for the user to review. This never edits document content directly. Read the document first and pass its exact content_revision."
    )]
    fn suggest_change(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(p): Parameters<SuggestParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        let (principal, authorized) =
            self.authorize(&parts, ReviewerOperation::Suggest, Some(&p.doc_id))?;
        let connection_id = authorized
            .connection()
            .ok_or_else(|| {
                ErrorData::invalid_request(
                    "suggest_change requires a configured reviewer connection",
                    None,
                )
            })?
            .id
            .clone();
        let reported_model = p.caller.model.clone();
        let (actor, context) = self.mutation_identity(&principal, &authorized, &p.caller)?;
        let outcome = self
            .workspace
            .propose_suggestion(
                &p.doc_id,
                &p.request_id,
                &p.content_revision,
                &SuggestedChange::from(p.change),
                p.explanation.as_deref(),
                reported_model.as_deref(),
                &connection_id,
                &actor,
                &context,
            )
            .map_err(failed)?;
        Ok(Json(serde_json::to_value(outcome).map_err(failed)?))
    }

    #[tool(
        description = "Who has worked on a document — humans and agents, with edit counts \
                       and when they last wrote."
    )]
    fn document_actors(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(p): Parameters<DocParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        self.authorize(&parts, ReviewerOperation::Read, Some(&p.doc_id))?;
        let actors = self.workspace.document_actors(&p.doc_id).map_err(failed)?;
        Ok(Json(serde_json::json!({ "actors": actors })))
    }

    #[tool(
        description = "Who wrote each block of a document — which human, which agent, when, \
                       and in which run. `created_by` is who first wrote the block and \
                       `touched_by` who last changed it; they differ when one actor edits \
                       another's text. Blocks with no entry are unattributed, which is not \
                       the same as yours."
    )]
    fn block_provenance(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(p): Parameters<DocParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        self.authorize(&parts, ReviewerOperation::Read, Some(&p.doc_id))?;
        let blocks = self.workspace.block_provenance(&p.doc_id).map_err(failed)?;
        Ok(Json(serde_json::json!({ "blocks": blocks })))
    }

    #[tool(
        description = "Current wording contribution by source, bound to a normalized visible-\
                       wording revision. V1 does not carry editor range anchors, so duplicate \
                       equal text from different sources can be ambiguous; the response \
                       identifies its alignment basis."
    )]
    fn document_lineage(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(p): Parameters<DocParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        self.authorize(&parts, ReviewerOperation::Read, Some(&p.doc_id))?;
        let lineage = self.workspace.document_lineage(&p.doc_id).map_err(failed)?;
        Ok(Json(serde_json::to_value(lineage).map_err(failed)?))
    }

    #[tool(description = "Full-text search across documents.")]
    fn search(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(p): Parameters<SearchParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        let (_, authorized) = self.authorize(&parts, ReviewerOperation::Read, None)?;
        let hits = self
            .workspace
            .search_scoped(&p.query, p.limit, authorized.selected_document())
            .map_err(failed)?;
        Ok(Json(serde_json::json!({ "hits": hits })))
    }

    #[tool(
        description = "Create a document. Pass `initial_markdown` only when importing a new \
                       Markdown snapshot; edits to an existing document must remain block-addressed."
    )]
    fn create_document(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(p): Parameters<CreateParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        let (principal, authorized) = self.authorize(&parts, ReviewerOperation::Create, None)?;
        let (actor, context) = self.mutation_identity(&principal, &authorized, &p.caller)?;
        let view = match p.initial_markdown {
            Some(markdown) => self
                .workspace
                .create_document_from_markdown_with_context(&p.title, &markdown, &actor, &context),
            None => self
                .workspace
                .create_document_with_context(&p.title, &actor, &context),
        }
        .map_err(failed)?;
        Ok(Json(serde_json::to_value(view).map_err(failed)?))
    }

    #[tool(description = "Replace one block's content with markdown.")]
    fn replace_block(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(p): Parameters<ReplaceBlockParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        let (principal, authorized) =
            self.authorize(&parts, ReviewerOperation::Edit, Some(&p.doc_id))?;
        let (actor, context) = self.mutation_identity(&principal, &authorized, &p.caller)?;
        let out = self
            .workspace
            .replace_block_with_context(
                &p.doc_id,
                &p.block_id,
                &p.markdown,
                p.version.as_deref(),
                &actor,
                &context,
            )
            .map_err(failed)?;
        Ok(Json(serde_json::to_value(out).map_err(failed)?))
    }

    #[tool(description = "Insert new blocks after a block, or at the start or end.")]
    fn insert_blocks(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(p): Parameters<InsertBlocksParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        let position = match p.after.as_deref() {
            None | Some("end") => Position::End,
            Some("start") => Position::Start,
            Some(id) => Position::After(id.to_string()),
        };
        let (principal, authorized) =
            self.authorize(&parts, ReviewerOperation::Edit, Some(&p.doc_id))?;
        let (actor, context) = self.mutation_identity(&principal, &authorized, &p.caller)?;
        let out = self
            .workspace
            .insert_blocks_with_context(
                &p.doc_id,
                &position,
                &p.markdown,
                p.version.as_deref(),
                &actor,
                &context,
            )
            .map_err(failed)?;
        Ok(Json(serde_json::to_value(out).map_err(failed)?))
    }

    #[tool(
        description = "Find and replace within one block. `find` matches the block's markdown \
                       (what read_document returned), so include any emphasis syntax. Omit \
                       `occurrence` to replace every match, or pass a 1-based index."
    )]
    fn replace_text(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(p): Parameters<ReplaceTextParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        let (principal, authorized) =
            self.authorize(&parts, ReviewerOperation::Edit, Some(&p.doc_id))?;
        let (actor, context) = self.mutation_identity(&principal, &authorized, &p.caller)?;
        let out = self
            .workspace
            .replace_text_with_context(
                &p.doc_id,
                &p.block_id,
                &TextEdit {
                    find: &p.find,
                    replace: &p.replace,
                    occurrence: p.occurrence,
                },
                p.version.as_deref(),
                &actor,
                &context,
            )
            .map_err(failed)?;
        Ok(Json(serde_json::to_value(out).map_err(failed)?))
    }

    #[tool(
        description = "Move a document to the trash, or restore it. Soft delete: the \
                       document and its history remain, and the tombstone replicates."
    )]
    fn set_document_deleted(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(p): Parameters<DeleteDocumentParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        let (principal, authorized) =
            self.authorize(&parts, ReviewerOperation::Trash, Some(&p.doc_id))?;
        let (actor, context) = self.mutation_identity(&principal, &authorized, &p.caller)?;
        let out = self
            .workspace
            .set_document_deleted_with_context(&p.doc_id, p.deleted, &actor, &context)
            .map_err(failed)?;
        Ok(Json(serde_json::to_value(out).map_err(failed)?))
    }

    #[tool(description = "Delete a block.")]
    fn delete_block(
        &self,
        Extension(parts): Extension<Parts>,
        Parameters(p): Parameters<DeleteBlockParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        let (principal, authorized) =
            self.authorize(&parts, ReviewerOperation::Edit, Some(&p.doc_id))?;
        let (actor, context) = self.mutation_identity(&principal, &authorized, &p.caller)?;
        let out = self
            .workspace
            .delete_block_with_context(
                &p.doc_id,
                &p.block_id,
                p.version.as_deref(),
                &actor,
                &context,
            )
            .map_err(failed)?;
        Ok(Json(serde_json::to_value(out).map_err(failed)?))
    }
}
