# Consumer AI support upgrade tracker

**Status:** active stacked delivery plan, updated 2026-08-26

This is the completeness checklist for the consumer AI support upgrade. A feature is not done
because a schema value, mock, reserved field, or static card exists. It is done only when the
usable flow, evidence boundary, failure behavior, tests, product copy, and reviewer notes land in
the owning pull request.

## Status key

- `[x]` implemented in the named branch. Behavior is covered by automated tests where practical,
  and static safety or product claims are checked directly when they are not runtime behavior.
- `[ ]` not implemented yet
- `Blocked` cannot ship until the listed acceptance gate passes
- `Preview only` may explain later functionality but cannot be selected or presented as working

## Stack map

| Stack | Scope | State |
| --- | --- | --- |
| `codex/editor-toolbar-branding` | Formatting schema, toolbar, and link actions | Separate base branch |
| `codex/brand-assets` | Product mark, icons, and colour assets | Separate base branch |
| `codex/daemon-single-owner` | Authenticated discovery and one daemon store owner | Separate base branch |
| `codex/document-lifecycle` | Document windows plus one-shot Markdown import and export | Separate base branch |
| `codex/sync-store-durability` | Durable editor acknowledgement and reconnect queue | Separate base branch |
| `codex/macos-graceful-quit` | Guarded macOS application termination | Separate base branch |
| `codex/tauri-ci-smoke` | Non-blocking Tauri smoke check | Separate base branch |
| PR #10, `codex/provenance-delta-foundation` | Semantic delta provenance foundation | Stacked predecessor |
| PR #11, `codex/provenance-anchors` | Exact anchored evidence and editor-only provenance ingress | Stacked predecessor |
| Current branch, `codex/ai-support-onboarding` | First-launch choice and AI support sidebar shell | Implemented here |
| Next connection PR | Durable reviewer identities, permissions, status, and every supported client | Planned |
| Suggestions PR | Replicated proposals, inline review, Accept, Reject, and conflicts | Planned |
| Pro PR | Secure provider keys, built-in chat, files, model and thinking controls, verified traces | Planned |
| Seal PR | Signed provenance bundle, explicit publishing, live anchoring, and Seal verification page | Planned |

Every PR targets its predecessor while the stack is open. When a predecessor merges, descendants
are retargeted or rebased without collapsing their review boundaries.

## 1. First launch and AI support shell

Owner: current branch

- [x] Connect is the visually recommended first-launch choice, but requires an explicit click.
- [x] Basic is an explicit local recording choice. Proof of Thought initiates no AI request or
  connection, while clearly noting that an AI app configured earlier may remain connected until
  it is removed in that app.
- [x] The choice persists across launches and can be changed later without touching documents or
  immutable history.
- [x] First launch waits for the choice before listing, opening, or creating a document.
- [x] The right sidebar explains the chosen setup, evidence strength, cost, and privacy boundary
  without presenting the choice itself as a live connection.
- [x] Connect says there is no separate Proof of Thought API charge while existing app plan limits
  still apply.
- [x] Basic explains `Written here`, `Pasted`, `Imported`, `Edited here`, and
  `Unclassified change` without claiming who composed pasted or imported content or promising
  exact attribution without anchored evidence.
- [x] The interface explains that connecting never publishes a document.
- [x] Reported and verified are explained without implying correctness, safety, usefulness, or
  human authorship.
- [x] ChatGPT desktop, Codex, and Claude Code receive setup guidance derived from the daemon's
  current local STDIO command. These are setup guides, not validated installed-client status.
- [x] Selecting Connect or copying setup is not treated as proof of a live connection. The shell
  makes no inferred connection-status claim from daemon health or caller-controlled actor data.
- [x] ChatGPT web is not presented as able to reach the local editor.
- [x] Claude Desktop is visibly tracked but cannot be marked ready until its local desktop
  extension is packaged and tested.
- [x] The shell prominently discloses that the current shared setup can read, search, create,
  edit, and trash documents across the local workspace, can work with no editor window open, can
  edit directly, may send returned content to the AI provider, and must currently be removed in
  the configured AI app.
- [x] The actionable command is explicitly a temporary broad legacy setup, not a durable reviewer
  connection. Per-connection credentials, permissions, status, and in-app revocation are release
  gates for the next connection pull request.
- [x] The old raw connection-command popover is removed so it cannot bypass the choice,
  disclosure, or client availability guidance.
- [x] Pro is a disabled preview. It is described as an add-on that can coexist with Connect.
- [x] First-launch focus is trapped inside the required choice, including the disclosure.
  Connect and Basic work from the keyboard, focus moves to the active surface, and reopened setup
  can be dismissed before another native modal takes over.
- [x] Storage denial leaves the current window usable and reports that the choice was not saved.
- [x] A daemon startup failure is shown inside the onboarding surface instead of only behind it.
- [x] Browser visual review covers keyboard selection, startup failure, the narrow sidebar, and
  the expanded onboarding disclosure at 560×400 and 1040×400.
- [ ] Manual visual review in the macOS WKWebView at normal and narrow window sizes.
- [x] A fresh unsigned Apple Silicon DMG was built and verified for this stack.

## 2. Reviewer connections

Owner: next connection PR

- [ ] Package and test a consumer-friendly local connection for ChatGPT desktop.
- [ ] Package and test a consumer-friendly local connection for Codex.
- [ ] Package and test a Claude Desktop extension.
- [ ] Package and test Claude Code setup.
- [ ] Replace the shared caller-selected MCP actor identity with a durable connection registry.
- [ ] Give every connection a stable ID, app/provider, display label, reported model, status,
  granted permissions, created time, and revocation state.
- [ ] Issue a unique credential for every connection, store it in native secure storage, and
  support rotation, expiration, and immediate revocation without affecting other reviewers.
- [ ] Support multiple simultaneous reviewers, including two reviewers from the same app.
- [ ] Keep same-name reviewers separate and preserve identity across reconnects.
- [ ] Make configured, connected, disconnected, failed, and revoked states distinct.
- [ ] Add, rename, reconnect, permission-change, and remove/revoke actions to the sidebar.
- [ ] Ensure one connection cannot use another connection's identity or permissions.
- [ ] Remove the temporary shared-capability exception before presenting any reviewer as a
  configured, independently permissioned connection.
- [ ] Freeze historical labels on events so renaming a connection does not rewrite history.
- [ ] Bind direct MCP edits to exact semantic ranges, or show them only as weaker V1 activity
  evidence until exact ranges are available.
- [ ] Show `ChatGPT (reported)`, `Claude (reported)`, `Codex (reported)`, and
  `Claude Code (reported)` only when authenticated connection evidence supports the app label.
- [ ] Never render a caller-supplied model name as verified.
- [ ] Basic causes Proof of Thought to initiate no reviewer or publication network action and
  provides a supported way to inspect and revoke connections previously configured in AI apps.

## 3. Consumer provenance view

Owner: next connection PR unless split into its own named blocking PR

- [ ] Show surviving current-wording contributions by concrete source.
- [ ] Support `Written here`, `Pasted`, `Imported`, `Edited here`, and
  `Unclassified change`.
- [ ] Keep connected reviewers separate by stable connection ID.
- [ ] Preserve untouched source spans when a reviewer changes only a few words.
- [ ] Exclude deleted text from the current contribution breakdown while retaining it in history.
- [ ] Do not reassign visible text for formatting-only edits.
- [ ] Do not use a `Mixed sources` badge.
- [ ] Do not call a surviving contribution percentage `AI generated`.
- [ ] Keep event-level evidence available for forensic inspection.
- [ ] Hide exact percentages for mixed surviving V1/V2 evidence.
- [ ] Block exact consumer percentages until the real WKWebView IME gate passes.

## 4. Replicated reviewer suggestions

Owner: suggestions PR

- [ ] Connected reviewers create suggestions by default instead of silently mutating shared text.
- [ ] If a temporary direct-write override remains, make it explicit, narrow to one document and
  reviewer, visibly time-bound, and record both consent and every resulting edit.
- [ ] Store reviewer connection, reported model, base revision, exact delta, anchors, target
  hashes, explanation, receipt reference, state, and decision event.
- [ ] Support pending, accepted, rejected, and conflicted states.
- [ ] Render insertions and deletions inline without relying on color alone.
- [ ] Add accessible Accept and Reject controls.
- [ ] Attribute accepted inserted text to the proposing reviewer, not the person clicking Accept.
- [ ] Rejection records a decision and changes neither content nor live lineage.
- [ ] Multiple reviewers can hold proposals at the same time.
- [ ] Overlapping proposals become explicit alternatives. Accepting one cannot silently apply or
  discard another.
- [ ] Stale anchors, target hashes, or base revisions cannot apply silently.
- [ ] Acceptance atomically updates document content, suggestion state, Yjs history, evidence,
  anchors, and lineage.
- [ ] Retry is idempotent and cannot duplicate proposals or decisions.
- [ ] Suggestions and decisions survive restart, cache rebuild, concurrent clients, and offline
  replication.

## 5. Pro provider path

Owner: Pro PR

- [ ] Pro can be enabled alongside existing Connect reviewers.
- [ ] Guide people through creating an OpenAI or Anthropic API key.
- [ ] Add, validate, replace, and revoke provider keys through native secure storage.
- [ ] Keep keys out of the webview, document database, logs, crash text, provenance records,
  exports, and Seal bundles.
- [ ] Provide built-in chat with streaming, cancellation, retry, and clear failure states.
- [ ] Support OpenAI and Anthropic provider selection.
- [ ] Support model selection and distinguish requested from provider-reported model.
- [ ] Support provider-specific thinking or reasoning levels and disable unsupported choices.
- [ ] Support file selection, type and size validation, upload progress, cancellation, errors,
  retention disclosure, and deletion.
- [ ] Keep chat attachments separate from document `Imported` provenance unless they actually
  change the document.
- [ ] Make document changes suggestion-first and reuse the same Accept and Reject flow.
- [ ] Capture provider-authenticated request, response, and tool evidence.
- [ ] Keep private capture checkpoints encrypted locally and publish no deep trace until the
  person reviews exactly what will be disclosed.
- [ ] Emit verified assurance only after the provider trace, proposal, anchors, hashes, and exact
  accepted delta verify together.
- [ ] Failed, cancelled, or unverifiable requests create no partial mutation and receive no
  verified label.
- [ ] State clearly that provider API usage charges apply and selected content or files are sent
  to that provider.

## 6. Seal publication and verification

Owner: Seal PR plus the required Seal repository change

- [ ] Use Seal as the one publication and verification destination. Do not create a separate
  Verify product.
- [ ] Produce a deterministic versioned bundle with revisions, deltas, ledger, anchors,
  connections, suggestions, decisions, signatures, and optional provider traces.
- [ ] Add a native signing-key lifecycle.
- [ ] Add optional live anchoring with salted or blinded cumulative roots and signed timestamps.
- [ ] Never transmit unsalted low-entropy document hashes.
- [ ] Make publishing an explicit action with a disclosure review.
- [ ] Let the person choose whether deeper Pro traces are included or redacted.
- [ ] Publish only the tool-boundary exchange available to Connect.
- [ ] Verify the bundle and each nested provider trace independently.
- [ ] Render Basic, Connect, Pro, V1-only, V2-only, and mixed histories at their actual evidence
  strength without promotion.
- [ ] Document retention, republishing, deletion, and unpublishing behavior.
- [ ] Explain that publication-only upload proves integrity from publication onward. Earlier
  existence requires a valid live-anchor receipt.

## 7. Cross-stack release gates

- [ ] Record a real Japanese or Pinyin IME transaction through WKWebView, persisted anchored
  transport, and restart before enabling exact percentages.
- [ ] Keep actor, ingress, and assurance as independent dimensions in storage and UI.
- [ ] Record deltas, not raw keystrokes, clipboard history, or copy-out activity.
- [ ] Keep cost and privacy copy in the same PR that activates each capability.
- [ ] Give immutable evidence append-only and tamper coverage. Give mutable preferences,
  connection state, and proposal state authorized transition coverage. Give both categories
  migration, restart/rebuild, atomic rollback, retry, and mixed-history coverage where relevant.
- [ ] Make connection consent, API key use, file disclosure, and publication separate actions.
- [ ] Run keyboard, focus, screen-reader, reduced-motion, narrow-window, offline, restart, and
  recovery acceptance checks for every new interactive surface.
- [ ] Build and verify a fresh DMG for every consumer-facing PR.

## Current official client support references

- OpenAI documents local STDIO MCP in the ChatGPT desktop app, Codex CLI, and Codex IDE extension,
  with shared host configuration:
  <https://learn.chatgpt.com/docs/extend/mcp>
- OpenAI documents that ChatGPT web uses hosted plugin tools and does not read local Codex
  configuration:
  <https://learn.chatgpt.com/docs/extend/mcp>
- Anthropic documents local STDIO setup for Claude Code:
  <https://code.claude.com/docs/en/mcp>
- Anthropic documents local Claude Desktop servers as desktop extensions:
  <https://support.claude.com/en/articles/10949351-getting-started-with-local-mcp-servers-on-claude-desktop>

These links support setup wording only. They do not authenticate which app or model produced a
specific edit. That requires the durable Proof of Thought connection identity planned above.
