//! Editor-only document lifecycle endpoints.
//!
//! These routes are for the bundled window. Reviewer credentials authorize the
//! MCP surface only, so reviewers cannot claim locally observed lifecycle events.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use std::sync::Arc;
use thought_mcp::{
    ActorRef, MutationContext, ReviewerClient, ReviewerPermissions, UpdateReviewerConnection,
    Workspace, WorkspaceError,
};
use thought_store::StoreError;

use thoughtd::connections::{ConnectionRegistry, RegistryError, now_ms};
use thoughtd::{MAX_DOCUMENT_TITLE_BYTES, MAX_MARKDOWN_IMPORT_BYTES};

#[derive(Clone)]
struct EditorState {
    workspace: Arc<Workspace>,
    reviewers: Arc<ConnectionRegistry>,
}

#[derive(Debug, serde::Deserialize)]
struct CreateDocumentRequest {
    #[serde(default)]
    title: String,
    #[serde(default)]
    markdown: Option<String>,
}

#[derive(Debug, serde::Deserialize)]
struct SetDeletedRequest {
    deleted: bool,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateReviewerRequest {
    client: ReviewerClient,
    display_label: String,
    permissions: ReviewerPermissions,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateReviewerRequest {
    expected_revision: i64,
    #[serde(default)]
    display_label: Option<String>,
    #[serde(default)]
    permissions: Option<ReviewerPermissions>,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewerRevisionRequest {
    expected_revision: i64,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewerFailureRequest {
    failure_code: String,
    instance_id: String,
}

#[derive(Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewerFailureReporterRequest {
    instance_id: String,
}

#[derive(Debug, serde::Serialize)]
struct ErrorBody {
    error: String,
}

pub fn routes(workspace: Arc<Workspace>, reviewers: Arc<ConnectionRegistry>) -> Router {
    Router::new()
        .route("/editor/documents", post(create_document))
        .route(
            "/editor/documents/{doc_id}/deletion",
            post(set_document_deleted),
        )
        .route(
            "/editor/reviewer-connections",
            get(list_reviewer_connections).post(create_reviewer_connection),
        )
        .route(
            "/editor/reviewer-connections/{connection_id}",
            patch(update_reviewer_connection).delete(revoke_reviewer_connection),
        )
        .route(
            "/editor/reviewer-connections/{connection_id}/reset",
            post(reset_reviewer_connection),
        )
        .route(
            "/editor/reviewer-connections/{connection_id}/failure-reporter",
            post(prepare_reviewer_failure_reporter),
        )
        .route(
            "/editor/reviewer-connections/{connection_id}/failure",
            post(report_reviewer_failure),
        )
        // Axum's default JSON limit is smaller than the documented Markdown
        // import ceiling. Keep a little room for JSON escaping and metadata,
        // then enforce the decoded Markdown size exactly in the handler.
        .layer(axum::extract::DefaultBodyLimit::max(
            MAX_MARKDOWN_IMPORT_BYTES * 6 + 64 * 1024,
        ))
        .with_state(EditorState {
            workspace,
            reviewers,
        })
}

async fn create_document(
    State(state): State<EditorState>,
    Json(request): Json<CreateDocumentRequest>,
) -> Result<Json<serde_json::Value>, EditorApiError> {
    validate_create_request(&request)?;
    let actor = ActorRef::editor();
    let view = match request.markdown {
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
        .markdown
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

async fn list_reviewer_connections(
    State(state): State<EditorState>,
) -> Result<Json<serde_json::Value>, EditorApiError> {
    let connections = state.reviewers.list(now_ms())?;
    Ok(Json(serde_json::json!({ "connections": connections })))
}

async fn create_reviewer_connection(
    State(state): State<EditorState>,
    Json(request): Json<CreateReviewerRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>), EditorApiError> {
    let connection = state.reviewers.create(
        request.client,
        request.display_label,
        request.permissions,
        now_ms(),
    )?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "connection": connection })),
    ))
}

async fn update_reviewer_connection(
    State(state): State<EditorState>,
    Path(connection_id): Path<String>,
    Json(request): Json<UpdateReviewerRequest>,
) -> Result<Json<serde_json::Value>, EditorApiError> {
    let current = state.reviewers.connection(&connection_id)?;
    let connection = state.reviewers.update(
        &connection_id,
        &UpdateReviewerConnection {
            expected_revision: request.expected_revision,
            display_label: request.display_label.unwrap_or(current.display_label),
            permissions: request.permissions.unwrap_or(current.permissions),
            updated_at: now_ms(),
        },
    )?;
    Ok(Json(serde_json::json!({ "connection": connection })))
}

async fn reset_reviewer_connection(
    State(state): State<EditorState>,
    Path(connection_id): Path<String>,
    Json(request): Json<ReviewerRevisionRequest>,
) -> Result<Json<serde_json::Value>, EditorApiError> {
    let connection =
        state
            .reviewers
            .reset_credential(&connection_id, request.expected_revision, now_ms())?;
    Ok(Json(serde_json::json!({ "connection": connection })))
}

/// Record a failure the native launcher observed before it could authenticate
/// as the reviewer. Reviewer credentials cannot reach this route, so one
/// reviewer cannot choose another reviewer's visible status. The body contains
/// only a schema-controlled reason and never carries reviewer credentials.
async fn report_reviewer_failure(
    State(state): State<EditorState>,
    Path(connection_id): Path<String>,
    Json(request): Json<ReviewerFailureRequest>,
) -> Result<Json<serde_json::Value>, EditorApiError> {
    let connection = state.reviewers.mark_failed(
        &connection_id,
        &request.instance_id,
        &request.failure_code,
        now_ms(),
    )?;
    Ok(Json(serde_json::json!({ "connection": connection })))
}

/// Issue non-secret process metadata for the native launcher. The registry
/// retains the binding, and Reset atomically invalidates it before returning.
async fn prepare_reviewer_failure_reporter(
    State(state): State<EditorState>,
    Path(connection_id): Path<String>,
    Json(request): Json<ReviewerFailureReporterRequest>,
) -> Result<Json<serde_json::Value>, EditorApiError> {
    let credential_version =
        state
            .reviewers
            .prepare_failure_reporter(&connection_id, &request.instance_id, now_ms())?;
    Ok(Json(serde_json::json!({
        "credential_version": credential_version
    })))
}

async fn revoke_reviewer_connection(
    State(state): State<EditorState>,
    Path(connection_id): Path<String>,
    Json(request): Json<ReviewerRevisionRequest>,
) -> Result<Json<serde_json::Value>, EditorApiError> {
    let connection = state
        .reviewers
        .revoke(&connection_id, request.expected_revision, now_ms())?;
    Ok(Json(serde_json::json!({ "connection": connection })))
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

impl From<RegistryError> for EditorApiError {
    fn from(error: RegistryError) -> Self {
        let status = match &error {
            RegistryError::Unauthorized => StatusCode::UNAUTHORIZED,
            RegistryError::StaleFailureReport => StatusCode::CONFLICT,
            RegistryError::PermissionDenied(_) => StatusCode::FORBIDDEN,
            RegistryError::Workspace(WorkspaceError::ReviewerConnection(_)) => {
                StatusCode::BAD_REQUEST
            }
            RegistryError::Workspace(WorkspaceError::Storage(
                StoreError::ReviewerConnectionNotFound(_),
            )) => StatusCode::NOT_FOUND,
            RegistryError::Workspace(WorkspaceError::Storage(
                StoreError::ReviewerConnectionRevisionConflict { .. }
                | StoreError::ReviewerConnectionRevoked(_),
            )) => StatusCode::CONFLICT,
            RegistryError::Workspace(WorkspaceError::Storage(
                StoreError::InvalidReviewerConnectionTransition(_),
            )) => StatusCode::BAD_REQUEST,
            RegistryError::Unavailable
            | RegistryError::Credential(_)
            | RegistryError::Io(_)
            | RegistryError::Workspace(_) => StatusCode::INTERNAL_SERVER_ERROR,
        };
        Self {
            status,
            message: error.to_string(),
        }
    }
}

impl From<WorkspaceError> for EditorApiError {
    fn from(error: WorkspaceError) -> Self {
        let status = match &error {
            WorkspaceError::NoSuchDocument(_) => StatusCode::NOT_FOUND,
            WorkspaceError::InvalidMarkdown(_)
            | WorkspaceError::Block(_)
            | WorkspaceError::NotFound(_)
            | WorkspaceError::ReviewerConnection(_) => StatusCode::BAD_REQUEST,
            WorkspaceError::Storage(StoreError::ReviewerConnectionNotFound(_)) => {
                StatusCode::NOT_FOUND
            }
            WorkspaceError::Storage(StoreError::ReviewerConnectionRevoked(_)) => {
                StatusCode::CONFLICT
            }
            WorkspaceError::Storage(StoreError::InvalidReviewerConnectionTransition(_)) => {
                StatusCode::BAD_REQUEST
            }
            WorkspaceError::Storage(_)
            | WorkspaceError::Snapshot(_)
            | WorkspaceError::Lineage(_) => StatusCode::INTERNAL_SERVER_ERROR,
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
            markdown: None,
        };
        assert!(validate_create_request(&exact_title).is_ok());
        let long_title = CreateDocumentRequest {
            title: format!("{}a", exact_title.title),
            markdown: None,
        };
        assert!(validate_create_request(&long_title).is_err());

        let exact_markdown = CreateDocumentRequest {
            title: String::new(),
            markdown: Some("x".repeat(MAX_MARKDOWN_IMPORT_BYTES)),
        };
        assert!(validate_create_request(&exact_markdown).is_ok());
        let long_markdown = CreateDocumentRequest {
            title: String::new(),
            markdown: Some("x".repeat(MAX_MARKDOWN_IMPORT_BYTES + 1)),
        };
        assert!(validate_create_request(&long_markdown).is_err());
    }
}
