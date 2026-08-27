# Pro chat security and product contract

**Status:** implementation complete on `codex/pro-chat-core`; packaged WKWebView acceptance remains
open, updated 2026-08-27

This Pro chat core pull request is stacked on PR #16, `codex/pro-provider-foundation`. The parent
pull request owns native key entry, Keychain storage, catalog access checks, replacement, and local
removal. This pull request adds a text-only chat surface that can use a configured OpenAI or
Anthropic key. It does not add files, document context, document changes, provider trace
verification, or publishing.

## Product boundary

A person can keep a separate locally stored chat for each document and provider. They can choose a
model and an available thinking level, send typed or pasted text, watch visible response text stream
into the sidebar, stop a request, retry a failed or stopped turn, and clear the current chat.

OpenAI and Anthropic chats never share a transcript. Changing providers opens that provider's chat
for the current document. Changing documents opens that document's chat for the selected provider.
This isolation prevents a provider switch or document switch from silently sending an unrelated
conversation.

Chat is not an editor command. A response appears only in the sidebar and cannot directly change
the document. This pull request does not automatically include the editor document, a selection, or
a file. Anything the person types or pastes into the chat composer is part of the chat message and
is sent.

## Consent and content sharing

Provider setup and provider chat are separate consent boundaries. Saving a key does not authorize a
chat request. The first Send to each configured provider in each window session requires a separate,
provider-specific acknowledgement that:

- the newly typed or pasted message and the eligible completed prior chat for that document and
  provider are sent to the selected provider;
- provider API usage charges can apply; and
- no editor document, selection, or file is included automatically by this pull request.

This acknowledgement is an in-memory interface gate for that window. Native code validates the
current disclosure version but does not keep a process-wide consent record. Closing the window
therefore clears its acknowledgements.

The only conversation content in the native request is completed visible chat turns plus the newly
typed or pasted message. The request also carries the selected model and thinking setting, required
provider protocol fields, and bounded request metadata.
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
model metadata. It never stores the API credential read from secure storage or provider reasoning
content. A person should never paste a secret into the chat composer.

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

## Required checks

- OpenAI and Anthropic request shape, fixed hosts, authentication headers, redirect rejection, and
  HTTPS enforcement
- `store: false` on every OpenAI Responses request and local replay for both providers
- no automatic editor document, selection, or file inclusion, and no API credential read from
  secure storage or hidden reasoning in webview messages, transcripts, errors, logs, document state,
  or evidence artifacts
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
