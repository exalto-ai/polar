//! Turn a completed native chat response into a pending daemon suggestion.
//!
//! The webview chooses only the saved turn and insertion point. Native code
//! reloads the response and provider metadata from the local transcript.

use std::io::Read as _;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use thoughtd::discovery::Daemon;
use zeroize::Zeroizing;

use crate::pro_chat::ChatState;
use crate::pro_provider::Provider;

const MAX_DAEMON_RESPONSE_BYTES: u64 = 64 * 1024;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SuggestChatResponseRequest {
    document_id: String,
    provider: Provider,
    turn_id: String,
    request_id: String,
    after: SuggestionPosition,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum SuggestionPosition {
    Start,
    End,
    Block { block_id: String },
}

#[derive(Debug, Serialize)]
pub struct SuggestChatResponseResult {
    suggestion_id: String,
}

#[derive(Deserialize)]
struct DaemonResponse {
    suggestion: DaemonSuggestion,
}

#[derive(Deserialize)]
struct DaemonSuggestion {
    suggestion_id: String,
}

#[tauri::command]
pub async fn suggest_chat_response(
    chat: tauri::State<'_, ChatState>,
    daemon: tauri::State<'_, Daemon>,
    request: SuggestChatResponseRequest,
) -> Result<SuggestChatResponseResult, String> {
    let chat = chat.inner().clone();
    let daemon = daemon.inner().clone();
    tauri::async_runtime::spawn_blocking(move || suggest(&chat, &daemon, request))
        .await
        .map_err(|_| "Creating the suggestion stopped unexpectedly.".to_string())?
}

fn suggest(
    chat: &ChatState,
    daemon: &Daemon,
    request: SuggestChatResponseRequest,
) -> Result<SuggestChatResponseResult, String> {
    if request.request_id.is_empty()
        || request.request_id.len() > 128
        || !request
            .request_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
    {
        return Err("The suggestion request is invalid.".to_string());
    }
    if let SuggestionPosition::Block { block_id } = &request.after
        && (block_id.is_empty() || block_id.len() > 128 || block_id.chars().any(char::is_control))
    {
        return Err("The suggestion target is invalid.".to_string());
    }

    let source =
        chat.completed_response(&request.document_id, request.provider, &request.turn_id)?;
    let endpoint = daemon_endpoint(&daemon.url, &request.document_id)?;
    let body = Zeroizing::new(
        serde_json::to_vec(&serde_json::json!({
            "request_id": request.request_id,
            "turn_id": request.turn_id,
            "provider": request.provider,
            "requested_model": source.requested_model,
            "reported_model": source.reported_model,
            "assistant_text": source.assistant_text,
            "wording_revision": source.wording_revision,
            "after": request.after,
        }))
        .map_err(|_| "The suggestion could not be prepared.".to_string())?,
    );
    let mut authorization = Zeroizing::new(format!("Bearer {}", daemon.token));
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(5)))
        .proxy(None)
        .max_redirects(0)
        .max_redirects_will_error(false)
        .http_status_as_error(false)
        .max_idle_connections(0)
        .build()
        .into();
    let mut response = agent
        .post(endpoint)
        .header("Authorization", authorization.as_str())
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .send(body.as_slice())
        .map_err(|_| "Proof of Thought could not reach its local document service.".to_string())?;
    authorization.clear();
    let status = response.status().as_u16();
    let mut response_body = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_DAEMON_RESPONSE_BYTES + 1)
        .read_to_end(&mut response_body)
        .map_err(|_| "Proof of Thought returned an invalid suggestion response.".to_string())?;
    if response_body.len() as u64 > MAX_DAEMON_RESPONSE_BYTES {
        return Err("Proof of Thought returned an invalid suggestion response.".to_string());
    }
    if status != 200 {
        return Err(match status {
            409 => "The document changed after this response was generated.",
            400 | 413 => "This response cannot be suggested at the current position.",
            404 => "This document is no longer available.",
            _ => "Proof of Thought could not create the suggestion.",
        }
        .to_string());
    }
    let response: DaemonResponse = serde_json::from_slice(&response_body)
        .map_err(|_| "Proof of Thought returned an invalid suggestion response.".to_string())?;
    if response.suggestion.suggestion_id.is_empty()
        || response.suggestion.suggestion_id.len() > 300
        || response
            .suggestion
            .suggestion_id
            .chars()
            .any(char::is_control)
    {
        return Err("Proof of Thought returned an invalid suggestion response.".to_string());
    }
    Ok(SuggestChatResponseResult {
        suggestion_id: response.suggestion.suggestion_id,
    })
}

fn daemon_endpoint(url: &str, document_id: &str) -> Result<String, String> {
    let base = url
        .strip_suffix("/mcp")
        .filter(|base| base.starts_with("http://127.0.0.1:"))
        .ok_or_else(|| "Proof of Thought has an invalid local service address.".to_string())?;
    Ok(format!(
        "{base}/editor/documents/{document_id}/suggestions/pro-chat"
    ))
}
