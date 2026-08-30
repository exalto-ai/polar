# Pro provider setup security contract

**Status:** provider-foundation implementation complete; packaged WKWebView acceptance remains open,
updated 2026-08-27

This contract covers API-key setup only. It does not activate built-in chat, file transfer, model
selection, thinking controls, verified provider claims, or Seal publication.

## Product boundary

Pro setup supports OpenAI and Anthropic. A person can:

1. Open the provider's official API-key page.
2. Acknowledge that future provider API use can cost money.
3. Enter a key in a native macOS secure field.
4. Check model-catalog access without generating AI output or sending document content.
5. Replace a saved key without losing the earlier key when the new check fails or is cancelled.
6. Check the saved key again.
7. Remove Proof of Thought's local Keychain copy.

Local removal does not revoke the provider-side key. The interface directs the person back to the
provider's key page when provider-side revocation is needed.

## Secret boundary

The webview may send only a fixed provider identifier and the current disclosure version. It may
receive only nonsecret configuration metadata, a closed validation status, an action outcome, and
an optional bounded provider request ID.

The raw key must never enter:

- HTML controls, JavaScript values, Tauri command arguments, events, or return values
- local storage, Yjs, SQLite, Markdown files, exports, logs, or crash text
- provenance events, suggestion records, provider receipts, or Seal bundles

Native AppKit owns input through `NSSecureTextField`. Native Rust owns validation and storage. Key
and Bearer-header buffers use zeroizing memory where practical, and the secure field is cleared
before it is released.

## Keychain boundary

Provider keys use the separate `ai.exalto.thought.provider` Keychain service with fixed `openai`
and `anthropic` accounts. Reviewer helpers are not trusted for this service.

Creation and replacement set both the secret value and a trusted-app list for the running app. The
list is reset when an existing item is replaced, so another same-user app cannot precreate a
broad-access item and keep silent access after Proof of Thought stores a validated key. Ordinary
reads do not ask the app to change the item's owner or access list. macOS may still request one
decrypt authorization after the installed app's signing identity or path changes. Choosing Always
Allow lets Keychain remember the current app for later reads. A denied or cancelled authorization
fails closed and directs the person to replace the key in Settings if access cannot be restored.

The nonsecret settings file stores validation status, timestamps, model count, bounded request ID,
disclosure version, and cost-acknowledgement time. It uses private permissions and atomic replace.
A Keychain item without current acknowledged settings is treated as unconfigured. This covers an
interrupted save and a same-user preseed conservatively.

## Provider requests

Setup performs one read-only catalog request:

- OpenAI: `GET https://api.openai.com/v1/models` with Bearer authentication and a unique client
  request ID.
- Anthropic: `GET https://api.anthropic.com/v1/models?limit=100` with `x-api-key` and
  `anthropic-version: 2023-06-01`.

Production requests enforce HTTPS, use only those fixed hosts, reject redirects, apply a timeout,
disable connection reuse for the check, and bound the fully decoded response. Model counts and IDs
also have explicit limits. Raw error bodies are parsed only for documented error codes and the
documented Anthropic spend-limit prefix, then discarded. Bounded request IDs are rejected if they
reflect the API key.

A successful response means only that the provider accepted the key for model-catalog access. It
does not establish balance, generation permission, access to every listed model, or the behavior
of a future request. OpenAI organization and project routing are not part of this foundation; the
interface should not imply support for legacy or multi-organization routing.

## Failure contract

The interface receives only these categories:

- invalid key format
- credential or access invalid
- permission denied
- billing unavailable
- spending, credit, or usage limit reached
- temporary rate limit
- unsupported region
- provider unavailable
- timeout
- network or TLS failure
- invalid provider response
- no models available
- saved credential missing

Network and provider failures are not presented as invalid keys. Failed validation cannot replace
a saved key. Concurrent key operations across windows fail closed. Setup cancellation makes no
change. Local removal first records a `removal_pending` tombstone, then deletes the Keychain item
and clears the provider metadata. A failed or interrupted deletion stays visibly unfinished and
can be retried, rather than presenting the provider as configured or silently abandoning a key.

Add, Check, Replace, and Remove are also mutually exclusive with active chat for the same provider.
Chat cannot start while one of those key actions is active. This keeps one credential fixed for the
entire provider request.

## Provenance and privacy boundary

Setup creates no AI request with document content, no mutation, no suggestion, no provider receipt,
and no provenance event. It must never unlock a verified label. The stacked Pro chat flow requires
separate provider-specific consent. Each request sends a fresh bounded projection of the current
editor document and title, completed eligible chat, and the newly typed or pasted message. It adds
no editor selection, native save path, or external file automatically. Its additional transport,
local transcript, cancellation, and claim limits are in
[pro-chat-security.md](pro-chat-security.md). Provider and model labels remain Reported. The current
stack deliberately adds no second evidence store or provider-verification protocol.

Anthropic recommends App Attest for distributed macOS apps that call its API directly. This
consumer BYOK path intentionally uses a person's static API key, so the app must keep the native
boundary above and must not claim the stronger properties of App Attest.

## Required checks

- Add, cancel, failed replacement, successful replacement, recheck, and idempotent local removal
- fixed request hosts and authentication-header shapes
- redirect rejection and HTTPS enforcement
- bounded decoded success and error bodies, model counts, model IDs, and nonreflecting request IDs
- sanitized 401, 402, 403, 429, 5xx, billing, quota, timeout, network, TLS, and malformed responses
- no sentinel key in serialized command input, output, metadata, logs, document state, or exports
- private settings permissions, symlink rejection, atomic rollback, and conservative crash states
- app-only Keychain access construction and signed-build update continuity
- repeated capability checks and chat reads never ask to mutate Keychain ownership or access; a
  changed installed-app identity may require one macOS decrypt authorization, while later reads
  reuse the approved access
- keyboard, focus, narrow-window, screen-reader, cancellation, offline, and recovery behavior

## Official references

- [OpenAI API authentication](https://developers.openai.com/api/reference/overview)
- [OpenAI model catalog](https://developers.openai.com/api/reference/resources/models/methods/list)
- [OpenAI API error codes](https://developers.openai.com/api/docs/guides/error-codes)
- [OpenAI API keys](https://platform.openai.com/api-keys)
- [Anthropic authentication](https://platform.claude.com/docs/en/manage-claude/authentication)
- [Anthropic API overview](https://platform.claude.com/docs/en/api/overview)
- [Anthropic model catalog](https://platform.claude.com/docs/en/api/models/list)
- [Anthropic API errors](https://platform.claude.com/docs/en/api/errors)
- [Anthropic rate and spend limits](https://platform.claude.com/docs/en/api/rate-limits)
- [Anthropic API keys](https://platform.claude.com/settings/keys)
