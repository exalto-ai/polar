# Reviewer suggestions

Configured reviewers propose changes by default. They can edit document content immediately only
while the user has granted direct access for that document and connected MCP session.

## Flow

1. The reviewer calls `read_document` and receives `content_revision` plus stable block ids.
2. It calls `suggest_change` with one block-addressed operation and a unique `request_id`.
3. The daemon validates and normalizes the patch once, then stores it in the document's
   `suggestions` Y.Map. The existing CRDT op log persists and replicates it.
4. Any editor window can accept or reject through the editor-only API.
5. Acceptance applies the stored patch and marks it accepted in one CRDT update. Accepted
   text is attributed to the reviewer with `suggestion` ingress.

There is no second suggestion table or replay ledger. Reusing a reviewer's `request_id` for
the same document returns the first proposal.

## Connecting a reviewer

The Connected app panel creates a separately scoped reviewer credential, then shows setup for
the selected client:

- ChatGPT desktop: add a STDIO server under Settings → MCP servers, then restart and check `/mcp`.
- Codex: run the generated `codex mcp add` command, then check `/mcp`.
- Claude Desktop: merge the generated JSON entry into `claude_desktop_config.json`, fully quit,
  reopen, then click + in a chat → Connectors or inspect Developer settings.
- Claude Code: run the generated `claude mcp add --transport stdio --scope user` command, then
  check `/mcp`.

The copied value contains a stable connection ID, never its credential. ChatGPT on the web does
not read the local desktop configuration. ChatGPT desktop and Codex share local MCP configuration
on the same Mac. Claude Desktop and Claude Code have separate setup paths.

`Not used yet` means the credential has not authenticated a request since it was created or reset.
`Last used` is historical local connection activity and may include the stdio bridge's standard
MCP keepalive. It is not a document edit, model action, or verified live presence. A displayed
model is explicitly reported by the client and is not provider-verified.

## Connection-lifetime direct editing

Suggestions remain the default. A configured reviewer can call `request_direct_edit` for one
document, and the request appears in the native editor. While it is pending, denied, or expired,
the reviewer keeps using `suggest_change`.

If the user chooses **Allow direct editing**, the same configured connection may call
`replace_block`, `insert_blocks`, `replace_text`, and `delete_block` immediately for that document
and daemon-issued MCP session. Those edits do not wait for Accept/Reject. They still use stable
block ids, pass through the daemon's normal authorization and version checks, and are attributed as
reported MCP activity. The app, provider, model, person, and conversation remain self-reported,
not verified.

The grant is in-memory, session-scoped, and narrower than the durable reviewer credential. It has
no countdown and lasts while that configured AI connection remains open, including periods with
no tool calls. It ends when the user revokes it, the connection is changed, reset, or removed, or
the MCP session closes. It does not transfer to a replacement session. The configured stdio shim
quietly keeps an open session alive. A normal shutdown closes the daemon session and revokes the
grant promptly. If the client or shim disappears without closing, keepalives stop and bounded
daemon cleanup revokes the grant within roughly five minutes of the last activity. Direct HTTP
clients must manage their own standard MCP ping lifecycle. Transports that do not provide a
daemon-issued session identity stay suggestion-only.

Connection lifetime is deliberately observable and revocable in the editor. An alive but hung AI
client whose stdio connection remains open can keep receiving successful keepalive responses and
therefore retain its grant. The user can revoke that grant at any time. Connection reset, removal,
daemon restart, or an eventual close also ends it.

Reviewer tool discovery includes the guarded block-edit tools so clients that cache their tool
list can use them after approval. Their presence is not a grant. The daemon checks the active
credential, document, session, and grant on every edit. Reviewer connections can never use this
flow to create, trash, or restore documents, and built-in provider chat remains suggestion-only.

## Patch shapes

`suggest_change` accepts one of:

- `replace_block`
- `insert_blocks`
- `replace_text`
- `delete_block`

Markdown and find/replace input are converted to normalized ProseMirror nodes at proposal
time. Acceptance does not parse or search again.

## Stale proposals

`content_revision` covers normalized content, structure, block identity, and block order.
Suggestion metadata does not change it.

If any content changes after a proposal, the proposal is shown as `stale` and cannot be
accepted. The reviewer must read the current document and submit a new proposal. Rejection
still works.

This is intentionally conservative. It avoids target hashes, relative-anchor rebasing,
overlap graphs, and automatic conflict resolution. Multiple windows do not race the state:
all decisions pass through the one daemon authority and are serialized by the workspace.
