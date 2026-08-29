//! Shared between the daemon and the stdio shim.

pub mod connections;
pub mod discovery;
pub mod logging;
pub mod sessions;

pub use thought_mcp::EDITOR_ACTOR_ID;
pub mod sync;

/// Native Markdown imports are capped for editor responsiveness. JSON may
/// escape one input byte as six ASCII bytes, so the MCP envelope is given a
/// larger independently enforced ceiling.
pub const MAX_MARKDOWN_IMPORT_BYTES: usize = 2 * 1024 * 1024;
pub const MAX_DOCUMENT_TITLE_BYTES: usize = 4 * 1024;
pub const MAX_MCP_REQUEST_BODY_BYTES: usize = 16 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::{MAX_MARKDOWN_IMPORT_BYTES, MAX_MCP_REQUEST_BODY_BYTES};

    #[test]
    fn worst_case_markdown_json_fits_the_mcp_request_limit() {
        let markdown = "\0".repeat(MAX_MARKDOWN_IMPORT_BYTES);
        let request = serde_json::to_vec(&serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {
                "name": "create_document",
                "arguments": {"title": "", "initial_markdown": markdown}
            }
        }))
        .unwrap();
        assert!(request.len() < MAX_MCP_REQUEST_BODY_BYTES);
    }
}
