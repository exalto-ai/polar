//! The MCP tool definitions (M1.6).
//!
//! Thin by design: every tool forwards to `thought-mcp`, which is where the
//! behaviour and the tests live. Nothing here should be worth testing, because
//! anything worth testing belongs one layer down where it can be tested without
//! a server.

use rmcp::handler::server::wrapper::{Json, Parameters};
use rmcp::{ErrorData, tool, tool_router};
use std::sync::Arc;
use thought_core::Position;
use thought_mcp::{ActorRef, MutationContext, TextEdit, Workspace};

#[derive(Clone)]
pub struct Thought {
    workspace: Arc<Workspace>,
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

/// Every write names its caller. There is no anonymous edit path, because an
/// unattributed change cannot be shown in the activity feed or reverted as part
/// of a run (AD-6, AD-11).
#[derive(serde::Deserialize, schemars::JsonSchema)]
pub struct Caller {
    /// Stable name for the agent. Reused across reconnects — a per-connection
    /// identity would fragment one agent into many actors.
    pub agent: String,
    #[serde(default)]
    pub model: Option<String>,
    /// Groups one agent turn so it can be reverted as a unit.
    #[serde(default)]
    pub session: Option<String>,
    /// Legacy wire field accepted for older clients. Public MCP always records
    /// and presents its caller as reported AI tool use, regardless of this
    /// value. The local window uses a separate editor-capability route for
    /// observed lifecycle actions.
    #[serde(default)]
    pub kind: Option<String>,
}

impl Caller {
    fn actor(&self) -> ActorRef {
        // Read and deliberately ignore the old field so its continued wire
        // compatibility cannot affect how public MCP activity is classified.
        let _ = self.kind.as_deref();
        ActorRef::agent(&self.agent, self.model.as_deref(), self.session.as_deref())
    }

    /// MCP decides the provenance class at the transport boundary. The legacy
    /// `kind` field cannot promote or hide an external tool call.
    fn mutation_context(&self) -> MutationContext {
        MutationContext::mcp_reported(self.agent.clone(), None, None, self.model.clone())
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

#[tool_router(server_handler)]
impl Thought {
    pub fn new(workspace: Arc<Workspace>) -> Self {
        Thought { workspace }
    }

    #[tool(
        description = "List documents, most recently updated first. Pass `trashed: true` \
                       to list deleted ones instead — deleting is soft, and this is how a \
                       document is found again."
    )]
    fn list_documents(
        &self,
        Parameters(p): Parameters<ListParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        let docs = self
            .workspace
            .list_documents(p.limit, p.trashed)
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
        Parameters(p): Parameters<DocParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        let view = self.workspace.read_document(&p.doc_id).map_err(failed)?;
        Ok(Json(serde_json::to_value(view).map_err(failed)?))
    }

    #[tool(
        description = "Who has worked on a document — humans and agents, with edit counts \
                       and when they last wrote."
    )]
    fn document_actors(
        &self,
        Parameters(p): Parameters<DocParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
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
        Parameters(p): Parameters<DocParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        let blocks = self.workspace.block_provenance(&p.doc_id).map_err(failed)?;
        Ok(Json(serde_json::json!({ "blocks": blocks })))
    }

    #[tool(
        description = "Current wording contribution by source, backed by surviving grapheme \
                       spans and a versioned deterministic alignment. V1 does not carry editor \
                       range anchors, so duplicate equal text from different sources can be \
                       ambiguous; the response identifies its alignment basis."
    )]
    fn document_lineage(
        &self,
        Parameters(p): Parameters<DocParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        let lineage = self.workspace.document_lineage(&p.doc_id).map_err(failed)?;
        Ok(Json(serde_json::to_value(lineage).map_err(failed)?))
    }

    #[tool(description = "Full-text search across documents.")]
    fn search(
        &self,
        Parameters(p): Parameters<SearchParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        let hits = self.workspace.search(&p.query, p.limit).map_err(failed)?;
        Ok(Json(serde_json::json!({ "hits": hits })))
    }

    #[tool(
        description = "Create a document. Pass `initial_markdown` only when importing a new \
                       Markdown snapshot; edits to an existing document must remain block-addressed."
    )]
    fn create_document(
        &self,
        Parameters(p): Parameters<CreateParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        let actor = p.caller.actor();
        let context = p.caller.mutation_context();
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
        Parameters(p): Parameters<ReplaceBlockParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        let actor = p.caller.actor();
        let context = p.caller.mutation_context();
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
        Parameters(p): Parameters<InsertBlocksParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        let position = match p.after.as_deref() {
            None | Some("end") => Position::End,
            Some("start") => Position::Start,
            Some(id) => Position::After(id.to_string()),
        };
        let actor = p.caller.actor();
        let context = p.caller.mutation_context();
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
        Parameters(p): Parameters<ReplaceTextParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        let actor = p.caller.actor();
        let context = p.caller.mutation_context();
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
        Parameters(p): Parameters<DeleteDocumentParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        let actor = p.caller.actor();
        let context = p.caller.mutation_context();
        let out = self
            .workspace
            .set_document_deleted_with_context(&p.doc_id, p.deleted, &actor, &context)
            .map_err(failed)?;
        Ok(Json(serde_json::to_value(out).map_err(failed)?))
    }

    #[tool(description = "Delete a block.")]
    fn delete_block(
        &self,
        Parameters(p): Parameters<DeleteBlockParams>,
    ) -> Result<Json<serde_json::Value>, ErrorData> {
        let actor = p.caller.actor();
        let context = p.caller.mutation_context();
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
