//! Editor-only lifecycle and reviewer-management endpoints.

use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::routing::{get, patch, post};
use axum::{Json, Router};
use std::sync::Arc;
use thought_mcp::{ActorRef, MutationContext, ReviewerAccess, ReviewerClient, Workspace};
use thoughtd::connections::{ConnectionRegistry, now_ms};

#[derive(Clone)]
struct EditorState {
    workspace: Arc<Workspace>,
    reviewers: Arc<ConnectionRegistry>,
}

#[derive(serde::Deserialize)]
struct CreateDocument {
    title: String,
    #[serde(default)]
    markdown: Option<String>,
}

#[derive(serde::Deserialize)]
struct SetDeleted {
    deleted: bool,
}

#[derive(serde::Deserialize)]
struct CreateReviewer {
    client: ReviewerClient,
    display_label: String,
    access: ReviewerAccess,
}

#[derive(serde::Deserialize)]
struct UpdateReviewer {
    expected_revision: i64,
    display_label: String,
    access: ReviewerAccess,
}

#[derive(serde::Deserialize)]
struct ReviewerRevision {
    expected_revision: i64,
}

type ApiError = (StatusCode, String);

fn failed(error: impl std::fmt::Display) -> ApiError {
    (StatusCode::BAD_REQUEST, error.to_string())
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
            get(list_reviewers).post(create_reviewer),
        )
        .route(
            "/editor/reviewer-connections/{connection_id}",
            patch(update_reviewer).delete(revoke_reviewer),
        )
        .route(
            "/editor/reviewer-connections/{connection_id}/reset",
            post(reset_reviewer),
        )
        .with_state(EditorState {
            workspace,
            reviewers,
        })
}

async fn create_document(
    State(state): State<EditorState>,
    Json(input): Json<CreateDocument>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let actor = ActorRef::editor();
    let document = match input.markdown {
        Some(markdown) => state.workspace.create_document_from_markdown_with_context(
            &input.title,
            &markdown,
            &actor,
            &MutationContext::imported(),
        ),
        None => state.workspace.create_document_with_context(
            &input.title,
            &actor,
            &MutationContext::entered(),
        ),
    }
    .map_err(failed)?;
    Ok(Json(serde_json::to_value(document).map_err(failed)?))
}

async fn set_document_deleted(
    State(state): State<EditorState>,
    Path(doc_id): Path<String>,
    Json(input): Json<SetDeleted>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let outcome = state
        .workspace
        .set_document_deleted_with_context(
            &doc_id,
            input.deleted,
            &ActorRef::editor(),
            &MutationContext::command(),
        )
        .map_err(failed)?;
    Ok(Json(serde_json::to_value(outcome).map_err(failed)?))
}

async fn list_reviewers(
    State(state): State<EditorState>,
) -> Result<Json<serde_json::Value>, ApiError> {
    Ok(Json(serde_json::json!({
        "connections": state.reviewers.list().map_err(failed)?
    })))
}

async fn create_reviewer(
    State(state): State<EditorState>,
    Json(input): Json<CreateReviewer>,
) -> Result<(StatusCode, Json<serde_json::Value>), ApiError> {
    let connection = state
        .reviewers
        .create(input.client, input.display_label, input.access, now_ms())
        .map_err(failed)?;
    Ok((
        StatusCode::CREATED,
        Json(serde_json::json!({ "connection": connection })),
    ))
}

async fn update_reviewer(
    State(state): State<EditorState>,
    Path(connection_id): Path<String>,
    Json(input): Json<UpdateReviewer>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let connection = state
        .reviewers
        .update(
            &connection_id,
            input.expected_revision,
            input.display_label,
            input.access,
            now_ms(),
        )
        .map_err(failed)?;
    Ok(Json(serde_json::json!({ "connection": connection })))
}

async fn reset_reviewer(
    State(state): State<EditorState>,
    Path(connection_id): Path<String>,
    Json(input): Json<ReviewerRevision>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let connection = state
        .reviewers
        .reset(&connection_id, input.expected_revision, now_ms())
        .map_err(failed)?;
    Ok(Json(serde_json::json!({ "connection": connection })))
}

async fn revoke_reviewer(
    State(state): State<EditorState>,
    Path(connection_id): Path<String>,
    Json(input): Json<ReviewerRevision>,
) -> Result<Json<serde_json::Value>, ApiError> {
    let connection = state
        .reviewers
        .revoke(&connection_id, input.expected_revision, now_ms())
        .map_err(failed)?;
    Ok(Json(serde_json::json!({ "connection": connection })))
}
