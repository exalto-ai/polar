use axum::extract::Path;
use axum::http::StatusCode;
use axum::routing::post;
use axum::{Json, Router};
use std::sync::Arc;
use thought_mcp::{ActorRef, MutationContext, Workspace};

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

type ApiError = (StatusCode, String);

fn failed(error: impl std::fmt::Display) -> ApiError {
    (StatusCode::BAD_REQUEST, error.to_string())
}

pub fn routes(workspace: Arc<Workspace>) -> Router {
    let create_workspace = workspace.clone();
    Router::new()
        .route(
            "/editor/documents",
            post(move |Json(input): Json<CreateDocument>| {
                let workspace = create_workspace.clone();
                async move {
                    let actor = ActorRef::editor();
                    let document = match input.markdown {
                        Some(markdown) => workspace.create_document_from_markdown_with_context(
                            &input.title,
                            &markdown,
                            &actor,
                            &MutationContext::imported(),
                        ),
                        None => workspace.create_document_with_context(
                            &input.title,
                            &actor,
                            &MutationContext::entered(),
                        ),
                    }
                    .map_err(failed)?;
                    Ok::<_, ApiError>(Json(document))
                }
            }),
        )
        .route(
            "/editor/documents/{doc_id}/deletion",
            post(
                move |Path(doc_id): Path<String>, Json(input): Json<SetDeleted>| {
                    let workspace = workspace.clone();
                    async move {
                        let outcome = workspace
                            .set_document_deleted_with_context(
                                &doc_id,
                                input.deleted,
                                &ActorRef::editor(),
                                &MutationContext::command(),
                            )
                            .map_err(failed)?;
                        Ok::<_, ApiError>(Json(outcome))
                    }
                },
            ),
        )
}
