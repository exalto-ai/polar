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
| `codex/ai-support-onboarding` | First-launch choice and AI support sidebar shell | Stacked predecessor |
| Current branch, `codex/reviewer-connections` | Durable reviewer identities, permissions, status, reset, and revocation | Ready for review |
| Consumer provenance PR | Surviving contributions by stable source, with evidence-strength gates | Planned |
| Suggestions PR | Replicated proposals, inline review, Accept, Reject, and conflicts | Planned |
| Pro PR | Secure provider keys, built-in chat, files, model and thinking controls, verified traces | Planned |
| Seal PR | Signed provenance bundle, explicit publishing, live anchoring, and Seal verification page | Planned |

Every PR targets its predecessor while the stack is open. When a predecessor merges, descendants
are retargeted or rebased without collapsing their review boundaries.

## 1. First launch and AI support shell

Owner: onboarding shell parent

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
- [x] The parent shell prominently disclosed that its broad shared setup could read, search,
  create, edit, and trash documents across the local workspace, work with no editor window open,
  edit directly, send returned content to the AI provider, and require removal in the configured
  AI app.
- [x] The parent command was explicitly labeled as a temporary broad legacy setup, not a durable
  reviewer connection. Per-connection credentials, permissions, status, and in-app revocation are
  release gates for the connection-core pull request that now follows it.
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

## 2. Reviewer connections

Owner: current branch

- [x] Generate consumer-friendly, connection-specific local setup for ChatGPT desktop, Codex, and
  Claude Code without exposing the raw credential to the webview or command text.
- [ ] Complete the packaged-client manual acceptance pass for ChatGPT desktop.
- [ ] Complete the packaged-client manual acceptance pass for Codex.
- [ ] Package and test a Claude Desktop extension.
- [ ] Complete the packaged-client manual acceptance pass for Claude Code.
- [x] Replace shared caller-selected local MCP actor identity with a durable reviewer connection
  registry.
- [x] Give every connection an immutable ID, configured client and provider, mutable display
  label, explicit permissions, lifecycle status, creation time, and revocation state. A model name
  remains reported request metadata rather than a verified registry fact.
- [x] Issue one unique credential per connection, keep raw credentials in native secure storage,
  and support reset and immediate revocation without changing or disabling other reviewers.
- [x] Store only credential hashes in SQLite. Keep raw credentials out of the webview, setup
  commands, discovery data, logs, lifecycle events, and provenance.
- [x] Support multiple simultaneous reviewers, including two reviewers from the same app.
- [x] Keep same-name reviewers separate and preserve identity across reconnects and credential
  reset.
- [x] Make configured, connected, disconnected, failed, and revoked states distinct, including
  lease expiry, clean disconnect, daemon restart, and anonymous-authentication failure behavior.
- [x] Add, rename, reconnect, permission-change, credential-reset, and revoke actions to the
  sidebar.
- [x] Give each reviewer current-document or all-document scope plus required read permission.
  Keep durable connections read-only until reviewable suggestions ship.
- [x] Bind each MCP session to its authenticated connection and recheck current authorization for
  every operation so one connection cannot use another connection's identity or permissions.
- [x] Serialize permission changes, reset, and revocation against in-flight authorization so a
  completed management action cannot race a later mutation.
- [x] Restrict the shared process-lifetime MCP capability to internal read-only use. External
  reviewers have no shared write fallback and the editor evidence route remains separately
  authenticated.
- [x] Preserve immutable connection identity and historical event labels when a display label or
  credential changes.
- [x] Freeze the canonical selected-document allowlist into each append-only lifecycle snapshot so
  later access changes cannot make earlier authorization history ambiguous.
- [x] Do not expose durable direct MCP edits before the suggestion layer. A future direct-write
  override must be explicit, per-session, and expiring.
- [x] Show `Configured for ChatGPT desktop`, `Configured for Codex`, and corresponding Claude
  route labels as saved routing configuration, never as proof that the named app made the call.
- [x] Generate MCP server names in the `thought-<connection suffix>` machine namespace. Earlier
  unreleased stacked setup commands used `proof-of-thought-<connection suffix>` and must be removed
  from the client before copying the replacement command.
- [x] Store a caller-supplied model only on the event for that call, label it as tool-reported,
  never fall back to a previous connection model, and never render it as verified.
- [x] Basic causes Proof of Thought to initiate no reviewer or publication network action and
  provides a supported way to inspect and revoke connections previously configured in AI apps.
- [x] State the local trust boundary plainly: connection credentials protect the Proof of Thought
  MCP surface, but they do not sandbox another process running as the same operating-system user.

## 3. Consumer provenance view

Owner: consumer provenance PR

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
- [x] Keep actor, ingress, and assurance as independent dimensions in storage and connection UI.
- [ ] Record deltas, not raw keystrokes, clipboard history, or copy-out activity.
- [ ] Keep cost and privacy copy in the same PR that activates each capability.
- [x] Give reviewer connection state authorized transitions, migration, restart, rollback, and
  credential-recovery coverage while keeping lifecycle events append-only and credential-free.
- [ ] Give future proposal state authorized transition, migration, restart/rebuild, atomic
  rollback, retry, and conflict coverage in the suggestions pull request.
- [ ] Make connection consent, API key use, file disclosure, and publication separate actions.
- [ ] Run keyboard, focus, screen-reader, reduced-motion, narrow-window, offline, restart, and
  recovery acceptance checks for every new interactive surface.
- [ ] Build and verify a fresh DMG for every consumer-facing PR.
- [x] Remove only conclusively dead supported discovery under the home and store locks, reject
  ambiguous or live publishers without signalling them, and drain SIGINT and SIGTERM through the
  same discovery-cleanup path.
- [x] Disable proxies and redirects for every credential-bearing loopback request, apply bounded
  control and MCP timeouts, and verify listener ownership plus the exact sidecar before sending an
  MCP or editor bearer.
- [x] Verify stable Developer ID helper identities and shared Keychain access in signed releases;
  treat ad hoc DMGs as same-build tests rather than proof of cross-update credential continuity.

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
specific edit. In particular, ChatGPT desktop and Codex share host configuration, so choosing one
in Proof of Thought records the intended route rather than runtime app identity. The durable
connection identity authenticates the local ingress; provider-authenticated identity and model
claims remain future Pro work.
