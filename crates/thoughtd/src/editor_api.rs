//! Editor-only document lifecycle endpoints.
//!
//! MCP and the local window intentionally have different capabilities. A
//! public MCP caller may report its own tool metadata, while only the editor
//! capability can record locally observed New, Open, Trash, and Restore flows.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use std::sync::Arc;
use thought_mcp::{ActorRef, MutationContext, Workspace, WorkspaceError};

use thoughtd::{MAX_DOCUMENT_TITLE_BYTES, MAX_MARKDOWN_IMPORT_BYTES};

#[derive(Clone)]
struct EditorState {
    workspace: Arc<Workspace>,
}

#[derive(Debug, serde::Deserialize)]
struct CreateDocumentRequest {
    #[serde(default)]
    title: String,
    #[serde(default)]
    initial_markdown: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct SetDeletedRequest {
    deleted: bool,
}

#[derive(Debug, serde::Serialize)]
struct ErrorBody {
    error: String,
}

pub fn routes(workspace: Arc<Workspace>) -> Router {
    Router::new()
        .route("/editor/documents", post(create_document))
        .route(
            "/editor/documents/{doc_id}/deleted",
            post(set_document_deleted),
        )
        // Axum's default JSON limit is smaller than the documented Markdown
        // import ceiling. Keep a little room for JSON escaping and metadata,
        // then enforce the decoded Markdown size exactly in the handler.
        .layer(axum::extract::DefaultBodyLimit::max(
            MAX_MARKDOWN_IMPORT_BYTES * 6 + 64 * 1024,
        ))
        .with_state(EditorState { workspace })
}

async fn create_document(
    State(state): State<EditorState>,
    Json(request): Json<CreateDocumentRequest>,
) -> Result<Json<serde_json::Value>, EditorApiError> {
    validate_create_request(&request)?;
    let actor = ActorRef::editor();
    let view = match request.initial_markdown {
        Some(markdown) => state.workspace.create_document_from_markdown_with_context(
            &request.title,
            &markdown,
            &actor,
            &MutationContext::imported(),
        )?,
        None => state.workspace.create_document_with_context(
            &request.title,
            &actor,
            &MutationContext::entered(),
        )?,
    };
    Ok(Json(
        serde_json::to_value(view).map_err(EditorApiError::internal)?,
    ))
}

fn validate_create_request(request: &CreateDocumentRequest) -> Result<(), EditorApiError> {
    if request.title.len() > MAX_DOCUMENT_TITLE_BYTES {
        return Err(EditorApiError::payload_too_large(format!(
            "document title exceeds the {} byte limit",
            MAX_DOCUMENT_TITLE_BYTES
        )));
    }
    if request
        .initial_markdown
        .as_ref()
        .is_some_and(|markdown| markdown.len() > MAX_MARKDOWN_IMPORT_BYTES)
    {
        return Err(EditorApiError::payload_too_large(format!(
            "Markdown import exceeds the {} byte limit",
            MAX_MARKDOWN_IMPORT_BYTES
        )));
    }
    Ok(())
}

async fn set_document_deleted(
    State(state): State<EditorState>,
    Path(doc_id): Path<String>,
    Json(request): Json<SetDeletedRequest>,
) -> Result<Json<serde_json::Value>, EditorApiError> {
    let outcome = state.workspace.set_document_deleted_with_context(
        &doc_id,
        request.deleted,
        &ActorRef::editor(),
        &MutationContext::command(),
    )?;
    Ok(Json(
        serde_json::to_value(outcome).map_err(EditorApiError::internal)?,
    ))
}

struct EditorApiError {
    status: StatusCode,
    message: String,
}

impl EditorApiError {
    fn payload_too_large(message: String) -> Self {
        Self {
            status: StatusCode::PAYLOAD_TOO_LARGE,
            message,
        }
    }

    fn internal(error: impl std::fmt::Display) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        }
    }
}

impl From<WorkspaceError> for EditorApiError {
    fn from(error: WorkspaceError) -> Self {
        let status = match error {
            WorkspaceError::NoSuchDocument(_) => StatusCode::NOT_FOUND,
            WorkspaceError::InvalidMarkdown(_)
            | WorkspaceError::Block(_)
            | WorkspaceError::NotFound(_) => StatusCode::BAD_REQUEST,
            WorkspaceError::Storage(_)
            | WorkspaceError::Snapshot(_)
            | WorkspaceError::Reconcile(_)
            | WorkspaceError::ProvenanceStore(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for EditorApiError {
    fn into_response(self) -> Response {
        (
            self.status,
            Json(ErrorBody {
                error: self.message,
            }),
        )
            .into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::{CreateDocumentRequest, validate_create_request};
    use thoughtd::{MAX_DOCUMENT_TITLE_BYTES, MAX_MARKDOWN_IMPORT_BYTES};

    #[test]
    fn create_limits_are_exact_utf8_byte_boundaries() {
        let exact_title = CreateDocumentRequest {
            title: "é".repeat(MAX_DOCUMENT_TITLE_BYTES / 2),
            initial_markdown: None,
        };
        assert!(validate_create_request(&exact_title).is_ok());
        let long_title = CreateDocumentRequest {
            title: format!("{}a", exact_title.title),
            initial_markdown: None,
        };
        assert!(validate_create_request(&long_title).is_err());

        let exact_markdown = CreateDocumentRequest {
            title: String::new(),
            initial_markdown: Some("x".repeat(MAX_MARKDOWN_IMPORT_BYTES)),
        };
        assert!(validate_create_request(&exact_markdown).is_ok());
        let long_markdown = CreateDocumentRequest {
            title: String::new(),
            initial_markdown: Some("x".repeat(MAX_MARKDOWN_IMPORT_BYTES + 1)),
        };
        assert!(validate_create_request(&long_markdown).is_err());
    }
}
