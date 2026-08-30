# Pro chat security and product contract

**Status:** implementation complete on `codex/pro-chat-core`; packaged WKWebView acceptance remains
open, updated 2026-08-27

The Pro chat core is stacked on PR #16, `codex/pro-provider-foundation`. The parent
pull request owns native key entry, Keychain storage, catalog access checks, replacement, and local
removal. This pull request adds a chat surface that can use a configured OpenAI or Anthropic key
with the current editor document as context. It does not add file attachments, direct document
changes, provider trace verification, or publishing.

## Product boundary

A person can keep a separate locally stored chat for each document and provider. They can choose a
model and an available thinking level, send typed or pasted text, watch visible response text stream
into the sidebar, stop a request, retry a failed or stopped turn, and clear the current chat.

OpenAI and Anthropic chats never share a transcript. Changing providers opens that provider's chat
for the current document. Changing documents opens that document's chat for the selected provider.
This isolation prevents a provider switch or document switch from silently sending an unrelated
conversation.

Chat is not an editor command. A response appears only in the sidebar and cannot directly change
the document. Every Send and Retry includes a fresh, bounded Markdown projection of the current
editor document and its current title. The app does not add the internal document identifier or
native save path. A selection is included only after the person explicitly captures it as visible
plain-text focus; external files are not included. Literal paths, links, and other
content written in the document remain part of its Markdown projection. Anything the person types
or pastes into the chat composer is also sent.

## Consent and content sharing

Provider setup and provider chat are separate consent boundaries. Saving a key does not authorize a
chat request. The first Send to each configured provider in each window session requires a separate,
provider-specific acknowledgement that:

- the current editor document, including its title, formatting, and links, the newly typed or
  pasted message, and the eligible completed prior chat for that document and provider are sent to
  the selected provider;
- provider API usage charges can apply; and
- no selection, native save path, or external file is included automatically.

This acknowledgement is an in-memory interface gate for that window. Native code validates the
current disclosure version but does not keep a process-wide consent record. Closing the window
therefore clears its acknowledgements. After acknowledgement, the composer keeps a concise reminder
that every request sends the current document and can incur provider charges. That reminder remains
visible when the person switches documents.

The provider request contains completed visible chat turns, the newly typed or pasted message, and
the current document snapshot and bounded metadata described above. Native code validates the live
ProseMirror tree against the editor schema and projects it to Markdown before building the provider
request. Invalid or oversized snapshots fail before provider I/O. The document is explicitly
delimited as untrusted source material in a native system instruction, and the model is told that it
can suggest changes but cannot claim to have applied them.
Failed, stopped, interrupted, or incomplete assistant output can remain visible for inspection, but
it is excluded from later provider context. Provider reasoning or thinking content is neither sent
to the webview nor persisted in the local transcript. The thinking selector controls a provider
request option, not a promise that hidden reasoning will be returned or recorded.

## Native transport boundary

Native Rust owns the API key, request body construction, HTTPS connection, stream parsing,
cancellation, and provider error classification. The credential read from secure storage never
becomes a JavaScript value, command argument, event payload, transcript field, or log field.

The chat transport uses fixed provider endpoints and rejects redirects:

- OpenAI uses the Responses API with streaming enabled and `store: false`. Proof of Thought keeps
  the visible conversation locally and replays the eligible visible turns on each request.
- Anthropic uses the Messages API with streaming enabled. Proof of Thought likewise keeps and
  replays the eligible visible conversation locally.

`store: false` does not promise that no upstream record exists. Messages leave the device, and each
provider's processing and retention remain governed by its API terms. Calling the transcript local
means Proof of Thought stores and replays its own copy instead of relying on a provider conversation
identifier.

Of provider-generated content, only visible output crosses into the webview. Bounded status, timing, token
usage, request ID, failure, and requested or reported model metadata cross separately. Hidden
reasoning and signatures do not cross or enter the local transcript. Unknown events are ignored
when safe. Malformed lifecycle data, oversized data, and unexpected tool output fail closed.

Provider terminal handling is intentionally specific. OpenAI visible refusal content may complete
normally when the surrounding response lifecycle is valid. Anthropic refusal, truncation, or
continuation-required output remains non-completed, stays out of later replay, and can be retried
only through the explicit Retry action.

Stop aborts Proof of Thought's active transport and prevents further stream handling. It cannot
guarantee that a provider stops work immediately, refunds usage, or avoids charges for work already
processed. This pull request does not automatically retry a partial stream. Retry is an explicit
action and reuses the original turn's provider, model, thinking level, and user text.

Provider-key Add, Check, Replace, and Remove actions cannot run while that provider has an active
chat request. Starting chat is likewise blocked while a key action is active. This prevents a key
from changing partway through a request.

## Local storage and isolation

Chat transcripts are private native files, separate from Yjs documents, the SQLite provenance
ledger, Markdown files, reviewer connections, and suggestions. Storage is partitioned by document
and provider, uses private permissions and atomic replacement, rejects unsafe paths and links, and
uses a revision check so a stale window cannot overwrite a newer conversation.

A pending request is recorded before provider I/O. Restart recovery marks a request that was left
pending as interrupted instead of presenting it as completed. The transcript records visible user
and assistant text, turn status, timestamps, token counts when reported, bounded request IDs, and
model metadata. The per-request document snapshot and title are not copied into dedicated transcript
or provenance fields. An assistant can quote document content in its visible reply, and that reply
is stored as ordinary visible chat. The transcript never stores the API credential read from secure
storage or provider reasoning content. A person should never put a secret in the editor or chat
composer if they do not want it sent to the selected provider.

Clear deletes only the current document and provider chat. It does not delete or rewrite the
document, immutable provenance, reviewer history, reviewer suggestions, another provider's chat,
or another document's chat.

## Model and evidence claims

The interface shows chat models retained from the last successful catalog check and thinking levels
allowed by the current native provider adapter. A catalog listing does not prove current generation
access, so a provider can still reject the model or settings when Send is used. The requested model
is the person's selection. A provider-reported model, when available in the response, is stored and
displayed separately. Neither value is silently promoted into a verified claim.

A Pro chat turn creates no document mutation, suggestion, provenance event, provider receipt, or
Seal material. It cannot show `Verified`. Provider-authenticated trace capture and the binding from
a provider exchange to an exact proposal, anchors, hashes, and accepted document delta remain the
scope of the later Pro trace pull request.

Copying a response creates no document mutation or provenance event. If the person later pastes it
into the editor, Proof of Thought records the new wording as `Pasted`, without guessing who composed
it before it reached the clipboard. Direct Apply or Insert controls remain deferred until provider
output can enter the existing proposal Accept and Reject path and bind the provider response to the
exact accepted delta.

## Required checks

- OpenAI and Anthropic request shape, fixed hosts, authentication headers, redirect rejection, and
  HTTPS enforcement
- `store: false` on every OpenAI Responses request and local replay for both providers
- a fresh schema-valid, bounded current-document snapshot on every Send and Retry, with only the
  title and any explicit plain-text focus as metadata; the app adds no native save path or internal
  document identifier
- no API credential read from secure storage or hidden reasoning in webview messages, transcripts,
  errors, logs, document state, or evidence artifacts; request-only document snapshots are not
  copied into dedicated transcript or provenance fields
- stream parsing across arbitrary chunk boundaries, bounded buffers, visible-text-only output,
  malformed lifecycle responses, provider-specific terminal states, unknown events, and unexpected
  tool output
- explicit per-provider and per-window consent, provider API cost language, and provider-switch
  isolation
- Stop during connection and streaming, explicit retry, no automatic partial retry, offline,
  timeout, billing, rate-limit, and provider failure states
- per-document and per-provider restart recovery, stale-revision rejection, private permissions,
  unsafe-link rejection, atomic writes, and Clear isolation
- requested and provider-reported model separation plus native disabling of unsupported thinking
  levels
- keyboard, focus, screen-reader, reduced-motion, narrow-window, restart, and packaged WKWebView
  acceptance, which remains open and is not satisfied by browser automation

## Official references

- [OpenAI Responses API](https://developers.openai.com/api/reference/cli/resources/responses/methods/create)
- [OpenAI streaming events](https://developers.openai.com/api/reference/resources/responses/streaming-events)
- [OpenAI models](https://developers.openai.com/api/docs/models)
- [Anthropic Messages API](https://platform.claude.com/docs/en/api/messages/create)
- [Anthropic streaming](https://platform.claude.com/docs/en/build-with-claude/streaming)
- [Anthropic model catalog](https://platform.claude.com/docs/en/api/models/list)
- [Anthropic effort](https://platform.claude.com/docs/en/build-with-claude/effort)
- [Anthropic extended thinking](https://platform.claude.com/docs/en/build-with-claude/extended-thinking)
