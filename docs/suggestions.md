# Reviewer suggestions

Configured reviewers can propose a change. They cannot edit document content directly.

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
