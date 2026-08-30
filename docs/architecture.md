# Proof of Thought — Architecture (v0)

**Status:** draft, nothing built yet · **Date:** 2026-08-22

A local-first macOS writing app. Individual documents (no folders in MVP), real-time
collaboration between humans *and* agents, fully functional offline, with a
self-hostable relay for sharing.

This document exists to be argued with. Each decision states what we're doing, why,
and what it costs us. Open questions are collected at the end rather than papered over.

---

## 1. Decisions

### AD-1 — Yjs (`yrs` in Rust) over Automerge

Yjs is faster, has the mature ProseMirror binding we need, and `yrs` is a wire-compatible
Rust port — so the same document can be authoritative in Rust and mirrored in the webview
without a translation layer.

**Cost:** Yjs has no real history model. Tombstones are GC'd, and "who wrote this, when"
is not recoverable from the CRDT. We buy that back with an append-only op log carrying
actor attribution (AD-4). If it turns out we need branch/merge or deep time-travel,
Automerge becomes the right answer and this is an expensive reversal.

### AD-2 — Daemon-first: `thoughtd` owns the store

The CRDT authority lives in a Rust daemon, not in the webview. The Tauri UI is a client.
MCP agents are clients. The relay sync client is a client. All of them speak the same
update protocol.

**Why:** agents must be able to edit documents when no window is open. If the CRDT lives
in the webview, every agent edit round-trips through a running, focused UI — which makes
the headline feature a special case instead of the normal path. It also avoids two
processes independently writing the same SQLite CRDT store.

**Cost:** IPC boundary to design and debug; a daemon lifecycle to manage; the UI can no
longer just reach into document state synchronously.

### AD-3 — Structured tree, not markdown source

Documents are a ProseMirror tree in a `Y.XmlFragment`. Markdown is a *projection* —
used for agent I/O, export, and search indexing — never the storage format.

**Why:** in markdown-source, formatting *is* text, so concurrent edits around the same
markers merge character-perfectly into structurally broken output (`**bo*ld*`). In a tree,
marks are metadata on ranges and those edits compose cleanly. Agents make large, fast,
structurally-significant edits — exactly the ones that break flat strings. Blocks also get
stable identity, which is what anchored comments, suggestions, and agent edits need.

Bear's *feel* is preserved via input rules (`## ` → heading), which is an input-method
choice, not a storage one.

**Cost:** no raw-markdown view; lossy `.md` export; more editor work upfront.

### AD-4 — SQLite is an op log plus snapshots, not a document model

No attempt to model rich text as rows. SQLite holds binary Yjs update frames, periodic
compacted snapshots, actor records, and an FTS index over the markdown projection.

### AD-5 — Agent changes are block-addressed and reviewer changes are suggestions

There is deliberately **no `update_document(id, full_markdown)` tool**. Whole-document
replacement produces a diff touching every block, destroys concurrent human edits, and
makes attribution meaningless. Agent tools address blocks by ID and edit ranges (§4).

Configured reviewers cannot call the direct-write tools. They can submit one normalized,
block-addressed patch with `suggest_change`; the local editor accepts or rejects it.
Suggestions live in the document CRDT so they replicate to every window and survive restart.

### AD-6 — Actor identity now, authentication later

Every device and every agent has a stable actor ID, kind, and display name. Self-asserted,
unverified, no accounts. This is *not* auth — it's what makes attribution, per-actor undo,
and accept/reject possible.

**Why now:** retrofitting identity into an op log that lacks it leaves all pre-migration
history permanently anonymous.

### AD-7 — Capability-URL sharing over a store-and-forward relay; no E2E in MVP

Sharing a document produces `thought://join/<doc_id>#<secret>`. The fragment never reaches
the server: the client sends `share_id = SHA256(secret)` to subscribe, so possession of
the link is the grant and the relay never learns the secret.

The relay is dumb but **persistent** — store-and-forward, not pure relay — so a peer that
was offline can catch up even when its collaborator is also offline.

No end-to-end encryption in MVP (you host the relay, you trust it), but `secret` is
reserved as the future content key. Known cost of that upgrade: encrypted updates mean the
relay can no longer compact snapshots, so op logs grow unbounded and every new joiner
replays from zero. That's a design problem to solve then, not a flag to flip.

### AD-8 — Tauri + WKWebView — automated checks clear, IME still unverified

macOS-only means one engine, so we're not fighting three implementations. But WKWebView
has the weakest `contenteditable` of the three — IME composition, cursor behavior at block
boundaries, selection near widgets.

`prototypes/editor-probe` now tests this. **WKWebView scores identically to Chromium on
all five automated checks** (2026-08-22): convergence under 60 concurrent agent ops,
schema validity on both replicas, offline divergence reconciling in ~1s, no caret theft
on remote updates, and markdown input rules firing.

**Correction to this decision's original framing.** It claimed WKWebView was the risk and
that native AppKit was the contingency. Investigation says otherwise on both counts:

* WKWebView's IME is the same code path Safari uses, and ProseMirror-based editors handle
  IME in Safari in production. The probe found no behavioural difference from Chromium on
  anything measurable. The engine-specific residual is a matter of degree — WebKit and
  Blink differ in how aggressively composition aborts when the marked range is mutated —
  and it is testable in plain Safari, no Tauri involved.
* The real exposure is **not the engine at all**; it is AD-17 below, and it would follow us
  to Electron. Native AppKit is therefore mispriced as a contingency: it does not fix the
  thing most likely to break, and the thing most likely to break has a cheap software fix.

IME still needs one manual pass with a Japanese or Pinyin input source, because composition
cannot be synthesised. But a failure there is now expected to be fixable in the bridge
rather than fatal to the stack.

---

## 2. Shape

```
┌─ Proof of Thought.app (Tauri shell) ───────┐
│   ProseMirror/TipTap ↔ Yjs (view replica)  │
└──────────────────┬─────────────────────────┘
                   │ IPC — Yjs update frames + awareness
┌──────────────────▼─────────────────────────┐
│  thoughtd (Rust, launchd agent)            │
│    • yrs docs — the authority              │
│    • SQLite: op log, snapshots, actors, FTS│
│    • MCP server (stdio + localhost HTTP)   │
│    • relay sync client                     │
└─────┬────────────────────────────────┬─────┘
      │ MCP                            │ WebSocket
┌─────▼──────────┐            ┌────────▼────────┐
│ local agents   │            │ relay (self-host)│
└────────────────┘            └────────┬─────────┘
                                       │
                              other peers + their agents
```

### Document CRDT shape

```
Y.Doc
├─ "content"      Y.XmlFragment            ProseMirror document
├─ "meta"         Y.Map                    title, icon, created_at, deleted_at (LWW)
├─ "suggestions"  Y.Map<id, Suggestion>    reviewer proposals, replicated
└─ "comments"     Y.Map<id, Comment>       range-anchored threads
```

Agent edits and suggestion patches address stable block ids. A suggestion also records an
exact content revision; any intervening content change makes it stale. This deliberately
trades automatic rebasing for a small, auditable acceptance path. Future range-anchored
comments may use Yjs `RelativePosition`s independently.

---

## 3. SQLite schema

```sql
-- Metadata only. Content lives in the op log.
CREATE TABLE documents (
  id          TEXT PRIMARY KEY,          -- uuidv7
  title       TEXT NOT NULL DEFAULT '',  -- denormalized from first heading, for list views
  created_at  INTEGER NOT NULL,          -- unix ms
  updated_at  INTEGER NOT NULL,
  -- NOTE: no deleted_at here. The tombstone must replicate, so it lives in
  -- Y.Doc "meta" as an LWW field. This column is a derived cache of it.
  deleted_at  INTEGER,
  share_id    TEXT,                      -- SHA256(secret); NULL = local-only
  relay_url   TEXT
);

-- Append-only. The source of truth.
CREATE TABLE updates (
  seq         INTEGER PRIMARY KEY AUTOINCREMENT,
  doc_id      TEXT    NOT NULL REFERENCES documents(id),
  payload     BLOB    NOT NULL,          -- yrs binary update frame
  actor_id    TEXT    NOT NULL REFERENCES actors(id),
  origin      TEXT    NOT NULL,          -- 'human' | 'agent' | 'remote'
  session_id  TEXT,                      -- groups one agent run / editing burst
  created_at  INTEGER NOT NULL,
  synced_at   INTEGER                    -- NULL = not yet acked by relay
);
CREATE INDEX updates_doc_seq  ON updates(doc_id, seq);
CREATE INDEX updates_unsynced ON updates(doc_id) WHERE synced_at IS NULL;

-- Compaction, so cold start isn't a full replay.
CREATE TABLE snapshots (
  doc_id       TEXT    NOT NULL REFERENCES documents(id),
  through_seq  INTEGER NOT NULL,
  state        BLOB    NOT NULL,
  state_vector BLOB    NOT NULL,
  created_at   INTEGER NOT NULL,
  PRIMARY KEY (doc_id, through_seq)
);

-- Self-asserted in MVP. See AD-6.
CREATE TABLE actors (
  id           TEXT PRIMARY KEY,
  kind         TEXT NOT NULL,            -- 'human' | 'agent'
  display_name TEXT NOT NULL,
  model        TEXT,                     -- agents only
  color        TEXT NOT NULL,            -- attribution rendering
  first_seen   INTEGER NOT NULL
);

-- Derived. Rebuildable by replaying the op log; dropped without loss (M2.8).
CREATE TABLE block_provenance (
  doc_id      TEXT    NOT NULL REFERENCES documents(id),
  block_id    TEXT    NOT NULL,          -- yrs BranchID, "client:clock"
  created_by  TEXT    NOT NULL REFERENCES actors(id),
  created_at  INTEGER NOT NULL,
  touched_by  TEXT    NOT NULL REFERENCES actors(id),
  touched_at  INTEGER NOT NULL,
  session_id  TEXT,                      -- the run that last touched it (AD-11)
  PRIMARY KEY (doc_id, block_id)
);
CREATE INDEX block_provenance_doc ON block_provenance(doc_id);

CREATE VIRTUAL TABLE doc_fts USING fts5(
  doc_id UNINDEXED, title, body, tokenize='porter unicode61'
);
```

---

## 4. MCP tool surface

Exposed by `thoughtd` over stdio and localhost HTTP. Available whether or not the UI is running.

**Read**

```
list_documents(query?, limit?)
  -> [{ doc_id, title, updated_at, word_count }]

read_document(doc_id, format="markdown")
  -> { markdown, version, content_revision, blocks: [{ block_id, type, line_range }] }

search(query, limit?)
  -> [{ doc_id, block_id, title, snippet }]

document_actors(doc_id)
  -> [{ actor_id, kind, display_name, model, color, last_seen, edits }]

block_provenance(doc_id)
  -> [{ block_id, created_by, created_at, touched_by, touched_at,
        session_id, kind, display_name, model, color }]
```

`block_provenance` answers per block what the op log answers per update. It is for agents as
much as for the window: "who wrote this paragraph" is a question an agent asks before
rewriting someone's work. A block with no entry is unattributed, which is not the same as
belonging to the caller (M2.8).

**Direct write** — unavailable to configured reviewer credentials. Every call takes the
`version` from the last read.

```
replace_block(doc_id, block_id, markdown, version)
insert_blocks(doc_id, after=block_id|"start", markdown, version)
delete_block(doc_id, block_id, version)
replace_text(doc_id, block_id, find, replace, occurrence?, version)
```

On a stale `version`, the daemon **warns and proceeds** rather than rejecting — the CRDT
merges correctly regardless; the risk is semantic (the agent reasoned about text that has
since changed), so the right response is to tell the agent what moved, not to fail the write.

**Reviewer proposal**

```
suggest_change(doc_id, request_id, content_revision, change, explanation?)
list_suggestions(doc_id)
```

The proposal call normalizes the requested operation once and stores that patch in the
document's `suggestions` map. Acceptance requires the exact `content_revision`; it never
attempts a three-way merge. See [`suggestions.md`](suggestions.md).

---

## 5. Relay protocol

One WebSocket, multiplexed across documents. The relay understands framing and storage,
not document semantics.

```
→ SUBSCRIBE  { doc_id, share_id, state_vector }
← SYNC       { doc_id, update }              -- everything the client is missing
→ UPDATE     { doc_id, update, client_seq }
← ACK        { doc_id, client_seq, server_seq }
← BROADCAST  { doc_id, update, from_actor }
↔ AWARENESS  { doc_id, actor_id, payload }   -- ephemeral, never persisted
← ERROR      { doc_id, code, message }
```

Server storage is an append-only log per document plus periodic compaction. Compaction
requires the server to interpret `yrs` frames — acceptable now, impossible under E2E (AD-7).

---

## 6. Open questions

All seven from the first draft are now resolved as AD-9 through AD-15 below. What remains
genuinely open is empirical, not architectural: whether WKWebView can host this editor
(AD-8), which the probe in `prototypes/editor-probe` exists to answer.

### AD-9 — TipTap over bare ProseMirror
Everything custom we need is decoration-based, and TipTap exposes raw PM plugins and the
`EditorView`. Reversible: TipTap docs *are* PM docs, so ejecting rewrites the shell, not the data.

### AD-10 — Daemon runs as a child process now; launchd deferred
`thoughtd` is a standalone binary the app happens to spawn, so the switch costs no code.
**Consequence that is not deferrable:** MCP transport must be **HTTP on localhost** with the
port in a well-known file, plus a stdio shim that proxies to it and only spawns the daemon if
absent. A plain stdio MCP server would let each agent client spawn its own daemon — two
processes writing one SQLite store.

**Cost:** startup performs a public instance check followed by an authenticated loopback
probe, and the daemon holds process-lifetime home and store locks. Current stale discovery
is replaced only by the new lock owner. An incompatible pre-lock discovery format still
fails closed rather than risking a second authority during the transition.

### AD-11 — ⌘Z is scoped to your own edits; agent runs get "Revert this run"
Undoing a collaborator's edit is the classic violation and agents get no exception. But agent
edits are discrete batches keyed by `session_id`, so per-run revert is a separate affordance
on the attribution chip.

### AD-12 — Markdown dialect, and the limits of what round-tripping may decide
CommonMark + GFM (tables, strikethrough, task lists). Nothing else in v0 — it is the
dialect models emit most reliably without prompting. **`parse(serialize(doc)) == doc` is a
property test from day one.**

**Corrected 2026-08-23.** This decision originally read "a node that cannot round-trip does
not enter the schema," which is wrong and contradicts AD-3. The entire argument for a
structured tree was the content ceiling — tables, embeds, toggles, mentions. Letting the
markdown projection veto the schema reinstates exactly the ceiling the tree exists to
escape. The projection serves the product; it does not define it.

The round-trip property constrains **which agent operations are safe**, not what the
product may contain. Nodes fall into two tiers:

* **Round-trippable** — survives `parse(serialize(x)) == x`. Agents read it as markdown and
  may rewrite it with markdown. Everything in v0 is this tier, tables included, verified
  over 20,000 generated documents.
* **Projection-only** — the product wants it, markdown cannot carry it. The node still
  exists in the schema and the editor. The projection emits a stable placeholder carrying
  the `block_id`, and `replace_block` with a *markdown* payload is refused for that block;
  editing it requires a structured payload. Agents can see it and address it, but cannot
  silently flatten it.

Nothing in v0 is projection-only, so that mechanism is not built. It is written down so
that the answer to "we want embeds" is a tier assignment rather than "drop the feature."

Where markdown is lossy even for round-trippable content — the intra-word emphasis gap,
empty paragraphs — the loss is bounded by AD-5, which keeps agents from ever writing back
a whole document.

### AD-13 — Snapshot every 200 updates or 30s idle, keep the last two
Correcting a conflation in the first draft: snapshots serve *load performance*, the op log
serves *provenance* (activity feed, per-run revert). Compaction must never delete the log.
Two retention policies, not one.

### AD-14 — Deletion is an LWW field in the document CRDT
Not a SQLite column — a column cannot replicate, so a peer would never learn of the delete.
Soft delete; edits never resurrect a tombstoned document; Trash recoverable for 30 days.

### AD-16 — The daemon-to-UI bridge must coalesce updates
Discovered by the probe, not by design. Applying Yjs updates one at a time, each producing
its own ProseMirror transaction, saturates the main thread under agent load — during early
runs updates arrived **20s behind** a nominal 120ms link, and the editor stayed responsive
only because the backlog was timer-driven.

Agents produce exactly this traffic shape: dozens of block ops in a burst. So the IPC
bridge coalesces updates over an animation frame and applies them as one transaction, and
`replace_block`-style tool calls within a single agent turn merge into one update before
they cross the boundary. Attribution stays per-op in the log (AD-13); only *application*
is batched.

### AD-17 — The bridge must hold remote updates during IME composition
`y-prosemirror`'s sync plugin contains **no reference to `view.composing`** (verified
against the bundled source; `prosemirror-view` has 26 for control). Remote Yjs updates are
applied to the view unconditionally, including while an input method has an active
composition in the node being redrawn.

This is latent in every Yjs + ProseMirror deployment, and mostly harmless in ordinary
collaboration: human co-editors generate sparse updates that rarely touch the exact node
someone is composing in. **Agents invert that.** They emit dense bursts of block-level
rewrites — precisely the traffic that lands on the composing node — so our headline
feature amplifies a bug most Yjs apps never trip.

Mitigation, and it is cheap: hold inbound remote updates while `view.composing` is true and
flush on `compositionend`. This is the same buffer AD-16 already requires for coalescing,
with one more condition on the flush. AD-15 helps independently: configured reviewer
proposals do not touch the text layer until a person accepts them.

Consequence: an IME failure is a bridge bug, not a reason to abandon the webview.

### AD-18 — Text lineage records operations, not a second history system

The CRDT and append-only update log remain the document history. Text lineage stores only
what the current document needs: one source event for each committed mutation and compact
UTF-16 spans pointing from surviving text to those events. The event id is the update-log
sequence, and the update, event, spans, search index, tombstone cache, and block rails commit
in one SQLite transaction.

When an operation supplies exact before/after text ranges, lineage uses them. Otherwise it
preserves only the common prefix and suffix within stable block ids and labels the result
`inferred`.
Content written before lineage existed is labelled `legacy_unknown`; it is not reconstructed
from history and presented as fact. MCP identity is self-reported and can only produce
`reported` provenance. A mutation is `observed` only when the bundled editor captures it at
the ProseMirror dispatch boundary. The loopback bearer authorizes access to private writing;
it does not authenticate actor identity.

Window-only document lifecycle operations use small `/editor` HTTP routes on the same loopback
daemon. They do not travel through public MCP tools with a caller-selected `kind`. This is a
surface boundary, not a second authentication system: the existing bearer protects both routes.

There is deliberately no receipt ledger, hash chain, replay engine, cached lineage state, or
format/structure delta model. Those would duplicate the CRDT and imply verification the local
daemon cannot provide. Evidence and verified traces may refer to source events later without
changing this storage model.

`document_lineage` includes a SHA-256 revision of the normalized Markdown projection represented
by the response. The native window computes the same revision from its visible editor tree and
renders current sources only on an exact match. This is a consistency binding, not a signature or
proof of origin.

**Cost:** inferred alignment can be ambiguous when text repeats, current spans do not answer
historical questions, and legacy content stays unknown. Exact editor ranges pay off the first;
the op log answers history; neither justifies inventing missing provenance for the third. The
window briefly hides current sources while a local edit is still saving.

### AD-23 — Provider keys stay native, and setup is not provenance

The window may choose OpenAI or Anthropic, but key entry stays in a native macOS secure field.
Native Rust checks the provider's fixed TLS model-catalog endpoint and stores a successful key in
the login Keychain. The webview never receives key text. Redirects are rejected, decoded bodies
are bounded, and raw provider errors do not cross into the interface.

The check establishes only that the key could list models at that moment. It creates no document
mutation, attribution, verified trace, or publication. Sending document content remains a separate
action owned by the feature that needs it.

**Cost:** setup requires macOS-specific code and signed-build Keychain testing. Provider status is
deliberately less detailed than raw API errors.

### AD-24 — Built-in chat is local, partitioned, and not provenance

Native Rust reads the configured key, calls fixed OpenAI or Anthropic endpoints, streams visible
output, classifies failures, and owns cancellation. Keys and authorization headers never enter the
webview. Each document and provider has a separate local transcript and revision; no provider-side
conversation identifier is reused.

Before the first send to each provider in a window, the person acknowledges that the current
document title and contents, including formatting and links, the visible chat, and the new message
leave the device and may incur charges. Setup consent does not cover sending content. Native code
validates and projects the live editor tree for each request. Only completed visible turns are
replayed as chat context. Partial, stopped, failed, or interrupted output may remain visible but is
excluded from later requests.

The transcript is private app state, not a document or evidence record. Clearing it does not alter
the CRDT, lineage, reviewers, or suggestions. This layer sends no selected ranges or files and
cannot claim provider-verified provenance.

**Cost:** the native layer owns provider-specific streaming state and a private transcript store.
Stopping a request cannot prove the provider stopped processing immediately.

### AD-25 — Chat responses enter documents through the existing suggestion path

A completed built-in chat response can become a pending block insertion at the editor's current
position. It never writes document content directly. Native code reloads the response, model, and
wording revision from the private transcript; the webview chooses only the saved turn and insertion
point. The daemon rejects the proposal if current wording no longer matches the wording sent to the
provider. Accept and Reject remain the same operations used for reviewer suggestions.

The native process reuses the daemon's private platform bearer. A second provider bearer would not
create a useful boundary because the bundled webview can already invoke the native command and use
the platform connection. Provider and model labels remain Reported, not Verified.

**Cost:** the first version inserts complete response blocks rather than replacing a selected text
range. Moving or editing the document after generation requires a new response. The private
transcript and replicated suggestion remain separate records.

### AD-15 — The local editor writes directly; configured reviewers propose

The bundled editor writes through the daemon's observed editor routes. Durable reviewer
credentials can read and propose but cannot directly edit, create, trash, or restore. A
future direct-write grant must be a separate, expiring session capability if the product
needs one; it is not a dormant permission flag in the current protocol.

**Cost:** reviewers always wait for acceptance, including on local unshared documents. This
is simpler and safer than shipping an unused grant lifecycle in the MVP.

### AD-20 — One product name and one machine namespace

The interface and application bundle are **Proof of Thought**. Names resolved by package
managers, process launchers, protocols, and local paths use **thought**: the frontend and
desktop packages, the `thought` window executable, Rust libraries, `thoughtd`,
`thought-mcp-stdio`, `ai.exalto.thought`, `THOUGHT_HOME`, and `thought://`.

**Cost:** changing a machine name invalidates build caches and artifact paths, can break
scripts or integrations that resolve the old name, and requires the release bundle,
signing, notarization, and packaged-executable checks to be repeated. The bundle identifier
and application-data paths stay stable so a package rename does not strand documents or
daemon discovery state.

### AD-21 — Reviewer connections get separate scoped credentials

The native app keeps the platform bearer from `daemon.json`. Each configured reviewer gets a
different 256-bit credential, stored in an owner-only native file with only its hash in SQLite.
The copyable setup command contains a stable connection ID, never the credential. Every MCP tool
rechecks that connection and its current-document or all-document scope. Reviewer credentials
authorize reads and replicated suggestions, never direct content mutation.

This authenticates one configured local Proof of Thought ingress. It does not authenticate the
calling app, provider, model, person, or conversation; those remain reported claims under AD-6.
It is also not an OS sandbox: software already running as the same user can read owner files.

**Cost:** credentials need native files and reset/revoke handling. Revocation stops subsequent
tool checks; an operation already past authorization may finish. We accept that narrow race rather
than adding session binding, leases, heartbeat processes, a lifecycle ledger, or signing-specific
Keychain ACL machinery.

## 7. Explicit non-goals for MVP

Folders and hierarchy · accounts and authentication · end-to-end encryption · mobile ·
plugins · version-history UI · `.md` file mirroring on disk.

---

# Part II — M1: the daemon

Part I is decisions. This is the first thing we actually build.

**No UI.** The editor is well-trodden ground and the probe already cleared it. The part
with no precedent to copy is a Rust daemon serving CRDT documents to agents over MCP with
stable anchors, so that is where the unknowns are and that is what goes first.

## M1.0 — Acceptance — **met 2026-08-23**

All four criteria pass in CI, and `thoughtd` serves them over real MCP. What follows is the
design as built; corrections found while building it are marked inline.


M1 is done when, with no window open anywhere:

1. An agent creates a document, reads it as markdown with block anchors, calls
   `replace_block`, and reads back its own edit.
2. The op log attributes every change to the right actor, and survives a daemon restart.
3. `parse(serialize(doc)) == doc` holds as a property test over documents generated from
   the schema.
4. Two agents editing concurrently converge, and both edits are individually attributable.

All four are scriptable. None of them need a webview, which is the point.

## M1.1 — Crate layout

```
proof-of-thought/
  crates/
    thought-schema/      # the schema as data; node/mark types; validation
    thought-core/        # yrs documents, block identity, anchors, markdown projection
    thought-store/       # SQLite: op log, snapshots, actors, FTS
    thought-mcp/         # tool surface + HTTP transport
    thoughtd/            # binary: wiring, config, lifecycle
    thought-mcp-stdio/   # shim binary: stdio -> HTTP, spawns thoughtd if absent
```

The split is not ceremony — it is what makes the acceptance criteria testable in
isolation. `thought-core` must round-trip markdown with no SQLite anywhere near it, and
`thought-store` must be exercisable without standing up an MCP server.

## M1.2 — The schema is data, defined once

The sleeper integration risk: TipTap defines the ProseMirror schema in TypeScript, and the
daemon needs the same schema to serialize, parse, and validate. Drift between them means
agents produce documents the editor rejects — a failure that surfaces late and looks like
a CRDT bug.

ProseMirror schemas *are* data: a plain spec object. So `thought-schema/schema.json` is the
single source of truth. TipTap loads it at construction; Rust deserializes it into a
`Schema` used by the serializer, parser, and validator. Neither side hand-writes a schema.

v0 nodes: `doc`, `paragraph`, `heading` (1–3), `blockquote`, `bulletList`, `orderedList`,
`listItem`, `codeBlock`, `horizontalRule`, `table`/`tableRow`/`tableCell`, `text`.
v0 marks: `strong`, `em`, `code`, `strike`, `link`.

Whether a node round-trips determines which agent operations are safe on it, not whether it
may exist (AD-12). Everything in v0 round-trips.

## M1.3 — Block identity

The ADR asserted that agents address blocks by ID without saying what mints them.
ProseMirror nodes have no stable identity, and an explicit `block_id` attribute is itself
CRDT state that can conflict on merge.

**Decision: use the intrinsic Yjs identity.** Every `Y.XmlElement` already carries a Yjs
ID — `(client_id, clock)` — that is globally unique without coordination, stable across
edits to the block's contents, and free. `block_id` is its string form, `"{client}:{clock}"`.

Consequences, including the unpleasant one:

* Splitting a block mints a new ID for the new half, so a `block_id` an agent read a moment
  ago can cease to exist. That is honest rather than convenient, and it is exactly what the
  `RelativePosition` anchors exist to absorb. `replace_block` against a vanished ID returns
  a "block moved" result carrying the current anchors, not an error.
* The daemon is authoritative for IDs. The webview needs them only to map decorations, and
  gets them from the daemon rather than deriving its own.

**Verified 2026-08-22** against `yrs` 0.27.4 (`prototypes/yrs-check`). The API is not
`element.id()` — that does not exist. It is `Branch::id()`, reached through
`AsRef<Branch>`, returning `BranchID::Nested(ID)` for nested elements and
`BranchID::Root(name)` for roots. Measured:

```
id at create    = 8691074583632113:0
id after edit   = 8691074583632113:0   stable across content edits
id on replica B = 8691074583632113:0   identical after sync
```

That third line is the one that matters and is easy to assume without checking: the same
block carries the same `block_id` on every replica, so an agent on one machine can hand a
`block_id` to an agent on another. Had it been replica-local, the whole anchor design would
have been quietly wrong.

**Toolchain note, also from that check:** `yrs` 0.27.4 uses `if let` guards and does not
build on Rust 1.94.1 (the current default here). It builds on 1.95.0. `thoughtd` should pin a
`rust-toolchain.toml` at >= 1.95 so this surfaces at setup rather than mid-build.

## M1.4 — SQLite access patterns

WAL mode, `synchronous = NORMAL`. Writes go through a single writer task — no contention to
design around, because the daemon owns the document anyway. Reads for list and search use a
small read pool.

**Append (hot path).** One `INSERT` into `updates`. Not one transaction per operation: an
agent turn's operations batch into a single write, which is the same coalescing AD-16
requires at the IPC boundary.

**Cold start.** Load the newest snapshot for the document, then replay
`SELECT payload FROM updates WHERE doc_id = ? AND seq > ? ORDER BY seq`. Documents are
loaded lazily on first access and held in an LRU, not all loaded at boot.

**Snapshot.** Every 200 updates or 30s idle, whichever comes first; keep the last two.
Snapshots serve load performance only — **compaction never deletes from `updates`**, because
the log is what the activity feed and per-run revert read (AD-13).

**Search.** FTS5 over the markdown projection, rewritten on snapshot rather than per update.
Agents need search or they resort to reading every document.

## M1.5 — Markdown projection

Lives in Rust, not JS. AD-2 says agents work with no window open, which means the daemon
serves markdown without a webview, which puts both directions of the projection on the
critical path:

* **Serialize:** walk the yrs `XmlFragment` against the schema, emitting CommonMark + GFM
  and a block map — `[{block_id, type, line_start, line_end}]` — alongside it. Agents get
  the markdown they are good at; anchors travel beside the text rather than polluting it.
* **Parse:** `pulldown-cmark` into a ProseMirror tree, validated against the schema.

**Extended 2026-08-25.** Toolbar formatting remains tree data. Title is a level-one
heading with `variant: "title"`; its projection uses the exact one-shot marker
`<!--thought:title-->` immediately before the heading so Title and H1 remain distinct. Inline
font size is a `fontSize` mark with a canonical whole-pixel value from 8px through 96px;
the projection uses an exact `<span style="font-size: 18px">` wrapper. The parser rejects
other CSS spellings so arbitrary style data cannot enter the document.

**Cost:** Title and font size are not native CommonMark. Their exact marker and HTML subset must
stay aligned across the TypeScript schema, Rust parser, Rust serializer, and generated round-trip
tests. External Markdown tools may strip those extensions, so export preserves wording but cannot
promise that another editor will preserve the same presentation. The marker is machine-format
syntax in the `thought` namespace. Changing it after release would require a dual-read migration
because an older exported Title would otherwise reopen as an ordinary H1. The shorter development
marker was replaced before release rather than becoming a permanent compatibility alias.

This is more work than one line of an ADR makes it sound, and it should be sized as such.
The property test in M1.0 is the guard: a node that cannot survive `parse(serialize(x))`
does not ship.

### Built 2026-08-23 — `crates/thought-schema`, `crates/thought-markdown`

Green over 40,000 generated documents. **Every defect below was found by the property
test, not by reading the code** — which is the argument for writing it before the schema
rather than after:

| Defect | Why it bites |
| --- | --- |
| Code fences | Serializer appended a terminating newline only when absent; parser always stripped one. Any block ending in a blank line lost it. |
| Tight lists | `pulldown-cmark` emits `Item -> Text` with no `Paragraph`, putting inline content directly in a `listItem` the schema says holds blocks. |
| `---` rules | Doubles as a setext underline, and `- ---` re-parses as a rule at the *outer* level. Emit `***`, which is neither. |
| Adjacent lists | Two sibling lists sharing a bullet merge into one. CommonMark starts a new list when the marker changes, so alternate between siblings. |
| ATX headings | A trailing `#` run is read as a closing sequence and stripped. |

**The one irreducible limitation.** A marked span fused to adjacent word characters cannot
round-trip when punctuation sits against the delimiter — from the span's own text, or from
a nested mark's delimiter, since every delimiter is punctuation. Escaping cannot help: the
backslash is punctuation too. CommonMark's flanking rules make this symmetric, so it
applies on both the opening and closing side. Whitespace separation resolves every variant,
which is why it is rare in practice — emphasis nearly always follows a space.

It is pinned by `intraword_emphasis_gap_is_pinned` rather than papered over, so a
`pulldown-cmark` upgrade that moves the boundary is noticed.

**Empty paragraphs are also unrepresentable**, and that independently justifies AD-5: an
agent that round-tripped a whole document through markdown would silently delete every
empty block. Surgical block edits never see the whole tree, so the loss cannot propagate
back into the document. The ban on whole-document replacement is load-bearing, not
stylistic.

**Gap this surfaced:** nothing validates content expressions. The generator happily built
`listItem` nodes without the leading `paragraph` the schema requires, and the serializer
took the blame. Either M1 validates against the content expressions in `schema.json`, or it
should stop claiming to.

**Correction to M1.2's mechanics.** TipTap builds its schema from Extensions and cannot
load a raw ProseMirror schema spec, so "TipTap loads schema.json" is not implementable as
written. The workable direction is the reverse: TS remains the definition, a build step
exports `getSchema(extensions).spec` to JSON, Rust consumes that, and CI fails if the
committed JSON drifts from what the extensions produce. One source of truth either way —
but the arrow points the other direction.

## M1.6 — MCP surface

**Built.** `thought-mcp` holds the tool surface with no transport attached, which is what lets
M1.0 be tested with no window, no editor and no HTTP; `thoughtd` wires it to `rmcp`'s
streamable-HTTP server. Corrections found while building:

* **Store and document cache share one mutex.** `rusqlite::Connection` is `Send` but not
  `Sync`, so the store needs a lock regardless — and two locks would need a global ordering
  the natural call shapes disagree about (reads go cache-then-store, creates go
  store-then-cache). One lock removes the question.
* **`THOUGHT_HOME`** overrides the store and discovery locations, or a test run publishes
  itself as *the* daemon and overwrites the real one's port and token.
* **Reindex on every mutation**, not on snapshot as M1.4 said — serializing a document and
  writing two rows is cheap, and agents reading a stale index is not.

HTTP on localhost. The port and a token live in
`~/Library/Application Support/ai.exalto.thought/daemon.json`, mode `0600` — any local
process can reach a localhost port, and documents are the user's private writing.
`thought-mcp-stdio` reads that file, proxies stdio to HTTP, and starts `thoughtd` when the
published instance is absent or stale (AD-10). **Built.** A public random instance ID is
checked before the bearer is sent. The daemon then proves the bearer on an exact health
route. Racing or recovery launches are safe because only the process holding both the home
and store locks may open SQLite and atomically replace discovery.

```
list_documents(query?, limit?)   -> [{doc_id, title, updated_at, word_count}]
read_document(doc_id)            -> {markdown, version, blocks:[{block_id,type,line_start,line_end}]}
search(query, limit?)            -> [{doc_id, block_id, title, snippet}]

create_document(title?, initial_markdown?) -> {doc_id, version}
replace_block(doc_id, block_id, markdown, version)
insert_blocks(doc_id, after, markdown, version)   # after = block_id | "start" | "end"
delete_block(doc_id, block_id, version)
replace_text(doc_id, block_id, find, replace, occurrence?, version)
```

`version` is the state vector from the last read. **A stale version warns and proceeds** —
the CRDT merges correctly regardless, so the risk is semantic rather than structural, and
failing the call punishes the agent for something that is not a conflict. The response
carries what moved.

Every call is attributed to an `actor_id` derived from the MCP session, and operations
within one turn share a `session_id` so per-run revert has something to key on (AD-11).

## M1 — complete, 2026-08-23

All four acceptance criteria pass in CI, plus the transport: `thoughtd` over loopback HTTP and
`thought-mcp-stdio` for stdio clients. What remains open is the manual IME pass (AD-8), which
needs a person and is now expected to be a bridge bug rather than a stack problem (AD-17).

## M1.7 — Deliberately not in M1

No relay, no Tauri, no editor, no suggestion layer, no awareness. Suggestion mode is
skipped on purpose: M1 documents are local and unshared, and AD-15 already says those take
direct writes.

## M1.8 — Open, in order of how much they would hurt

1. ~~Does `yrs` expose branch IDs publicly?~~ **Resolved** — see M1.3. IDs are stable across
   edits and identical across replicas.
2. ~~Does the actor identity survive an MCP session cleanly?~~ **Resolved** — identity derives
   from the client-supplied `agent` name, never the connection, so a reconnecting agent stays
   one actor. Every write names its caller; there is no anonymous edit path, because an
   unattributed change cannot appear in the activity feed or be reverted with its run.
3. ~~Table round-tripping.~~ **Resolved** — tables stay in v0. They round-trip over 20,000
   generated documents under two constraints, both pinned in `table_constraints_are_pinned`:
   tables must be rectangular (GFM pads every row to the header width), and the first row is
   always the header (GFM has no headerless table). Neither is a real restriction —
   ProseMirror tables are rectangular by construction. The initial failure was a generator
   emitting ragged tables that no editor could produce.

---

# Part III — M2: the bridge

M1 proved the daemon is complete without a UI. M2 attaches one, and in doing so builds the
two mitigations the probe and the source audit turned up: update coalescing (AD-16) and the
IME composition guard (AD-17).

**Shape amended 2026-08-30:** document-scoped windows with the shared ⌘K switcher and an
optional right sidebar for agent and proof controls. Agent carets and authorship colors remain
visible. Full v0 schema including table editing. System sans throughout, light and dark.

## M2.0 — Acceptance

1. Typing in the editor reaches SQLite and survives a restart of both app and daemon.
2. An agent editing over MCP appears **live** in an open editor, without a reload.
3. A human's typing appears in the agent's next `read_document`.
4. An agent's caret is visible while it edits, and authorship is distinguishable per actor.
5. Remote updates arriving mid-composition do not disturb an active IME composition.
6. A 200-op agent burst applies as coalesced transactions, not 200 separate ones.

(5) and (6) are testable without a human: composition can be *simulated* by holding the
guard open programmatically, even though a faithful IME test cannot be.

## M2.1 — The UI is a peer, not a special case

The editor needs Yjs update frames and awareness, not MCP tool calls. So `thoughtd` grows a
second endpoint — **and it speaks the relay protocol from §5, not a bespoke one.**

```
   Tauri webview  ──WebSocket──►  thoughtd  ──WebSocket──►  relay (M3)
      Yjs replica                   yrs authority            store-and-forward
```

The window is a peer that happens to be local. One protocol, two transports, and M3 gets to
reuse rather than add. If the UI had its own private channel, every future sync feature
would have to be built twice and could drift.

Messages are §5's: `SUBSCRIBE`, `SYNC`, `UPDATE`, `ACK`, `BROADCAST`, `AWARENESS`. Awareness
is never persisted — it is presence, not content.

The daemon sends `ACK` only after an `UPDATE` has reached SQLite. The window keeps every
local update queued until that acknowledgement, merges work that has not yet been sent,
and resends unacknowledged work after reconnecting. The toolbar reports Connecting,
Autosaved, Saving, Offline, or Save failed. A no-op resend is acknowledged too, since Yjs
updates are idempotent and an ACK can be lost when a socket closes.

**Cost:** the window now owns a two-slot retry queue and retains unacknowledged Yjs bytes.
The entry count is bounded, but retained bytes can grow during a long outage until the
source-aware layer adds its byte ceiling. The daemon must preserve positional reply ordering,
and acknowledge idempotent no-op retries.

## M2.2 — The schema, exported not authored twice

Correcting M1.2's direction, which was unimplementable as written: TipTap builds its schema
from Extensions and cannot load a raw ProseMirror spec.

TypeScript remains the definition. A build step writes `getSchema(extensions).spec` to
`crates/thought-schema/schema.json`, Rust consumes that, and **CI fails if the committed JSON
drifts from what the extensions produce.** Otherwise the two halves diverge and agents start
producing documents the editor rejects — a failure that surfaces late and looks like a CRDT
bug.

## M2.3 — Coalescing (AD-16)

Found by the probe: applying Yjs updates one at a time, each as its own ProseMirror
transaction, saturates the main thread under agent load — updates arrived 20s behind a 120ms
link. Agents emit exactly that traffic shape.

The provider buffers inbound updates and flushes once per animation frame as a single
transaction. Attribution stays per-op in the log; only *application* batches.

## M2.4 — The composition guard (AD-17)

`y-prosemirror` has no reference to `view.composing`, so remote updates are applied to the
view while an input method has live marked text in the node being redrawn.

The provider holds inbound updates while `view.composing` is true and flushes on
`compositionend` — the same buffer as M2.3, with one more condition on the flush. That
adjacency is why both belong to the same milestone.

## M2.5 — The window

Each document has its own native window. ⌘K opens a switcher backed by the daemon's FTS index,
the same `search` the agents use, so there is one search implementation rather than two. An
optional right sidebar holds agent and proof controls without interrupting startup or changing
how a document opens.

**Cost:** the sidebar reduces horizontal editing space while open and adds a focus region that
must leave keyboard and assistive-technology navigation while closed. It is presentation only;
the daemon and CRDT remain the document authority.

System sans throughout, sized and spaced for long-form writing. The editor and window use
the fixed deep-blue `#0c1622` ground shared with the app icon. A compact, centered toolbar
provides New, Import Markdown, Export Markdown, local zoom, persistent block style, font
size, bold, italic, and link commands. Clicking linked text opens an action card with
explicit open, copy, edit, and remove commands instead of navigating immediately. ⌘N
creates a blank document in its own window, visibly cascaded from and leaving the current
editor connected. The Connections action follows the same separate-document behavior. A
window can still open an existing document through the switcher. ⌘W closes the window;
export remains an explicit action rather than an exit prompt.

The database and CRDT remain authoritative. Import reads a Markdown snapshot into one new
document, atomically creating its initial CRDT state. Export projects the current editor
tree through Rust's canonical Markdown serializer, so it cannot race a WebSocket update or
drift from the agent-facing format. Every export chooses a destination and writes a one-time
copy; it never establishes a mirrored file or an external source of truth. Paths are
selected and used only inside native Rust commands. Markdown input rules give Bear's typing
feel (`## ` → heading) without markdown storage (AD-3).

**Cost:** every import creates a new document and every export asks for a destination.
Closing does not offer export because the CRDT store is authoritative and a Markdown copy
must not be mistaken for a mirrored working file.

**Cost:** the editor deliberately does not follow the system light or dark appearance. A future
theme must revisit the editor and app-icon relationship rather than tinting either independently.
Changing the ground or mark also requires keeping the style tokens, canonical `assets/orbit/`
sources, generated desktop and web assets, and `DESIGN.md` in sync.

## M2.6 — Agents made visible

Agent carets ride the awareness protocol, labelled with the actor's display name and colored
from the palette assigned in M1. Authorship tint comes from the op log rather than the CRDT,
since Yjs cannot carry it (AD-1) — which is what AD-6's insistence on identity-from-day-one
was for.

## M2.7 — Not in M2

No relay, no sharing, no suggestion layer, no activity feed, no per-run revert UI. Those
need M3's sharing story or are milestones of their own.

## M2.8 — Provenance rails

The other half of M2.6, and the second half of M2.0 (4): agent carets say who is
*here*, and the rails say who wrote *this*.

A thin bar in the left margin beside every block, coloured by the actor who last touched
it, solid for a human and dashed for an agent — the same shape-not-colour distinction the
presence chips make, because a palette nobody memorised does not distinguish anything.
Hovering a bar names the actor; hovering an agent's presence chip lights everything it
wrote.

**Where it comes from.** The op log, as M2.6 said it would. Attribution is a diff: every
commit already builds the normalized tree to reindex it, so `commit` additionally hashes
each top-level block and compares against the previous hashes, and any block whose
fingerprint moved is credited to that commit's actor. Block identity is intrinsic and
stable (AD-5), so a block that changed keeps its id and only its content hash moves —
which is exactly the signal this needs.

Persisted in `block_provenance`, which is **derived state** with the same standing as
`snapshots` and `doc_fts`: rebuildable, discardable, never the truth. A document with no
attribution is attributed once by replaying its log — from the log, never a snapshot,
since snapshots compact away the history that says who wrote what (AD-13).

**`created_by` and `touched_by` are separate columns.** A paragraph an agent drafted and a
human then reworded is both, and storing only the second erases where the words came from.
The rail's label says "You · 4m ago, drafted by research" for exactly that case.

**What it costs, stated plainly:**

1. **Block granularity, not character.** Two actors in one paragraph: the last one owns the
   rail. Character-level attribution needs something Yjs cannot carry (AD-1) and would mean
   a second CRDT running alongside the first.
2. **"Last touched" flattens history.** `created_by` recovers one step of it. Not more.
3. **Blank means unknown, never yours.** A document can arrive with content and no local log,
   and drawing rails on it as the reader's own would be a lie — so it gets none. M3.3 keeps
   this from becoming permanent by putting an actor descriptor on the wire, and in doing so
   hands the rails a job they do not do yet: distinguishing *this machine saw this person
   write it* from *a peer told me this person wrote it*. Until they do, everything a rail
   claims is something this daemon watched happen.
4. **One human.** Every window on a device writes as one actor, which is what AD-6 already
   says identity is — per device, per agent. Two windows are two peers of one human, and
   the rails cannot tell them apart because there is nothing to tell apart. A second *human*
   actor first becomes possible when M3 puts a second device on the relay (M3.3) — and it
   arrives self-asserted, which is the point at which the sentence above stops being free.
5. **It leans on one y-prosemirror internal.** Block ids come from `_item.id` on the CRDT's
   children, matched to the editor's nodes by position and checked by node kind — TipTap
   keeps a trailing paragraph the CRDT never sees, so the two lists are legitimately
   different lengths. A disagreement skips the frame: a late rail is invisible, a rail on
   the wrong paragraph is a lie.

The rails are also where AD-11's "Revert this run" belongs when it is built — `session_id`
is already on the row.

## M2 — complete, 2026-08-23

All six acceptance criteria pass. The last of them — authorship distinguishable per actor —
closed when the rails landed in M2.8; the other five have been gated in CI since the window
was attached.

What remains is the manual IME pass (AD-8). It needs a person at a keyboard, so it is listed
below rather than counted here.

## M2.9 — Open

The manual IME pass (AD-8), which needs a person. Everything else in M2.0 passes in CI.

---

# Part IV — M3: sharing

M2 made the window a peer. M3 adds a second hop so peers on different machines
reach each other.

**Decided 2026-08-23:** relay on Fly.io, plaintext frames with server-side
compaction, one write-capable link per document, and a recipient's agents get
the same access the recipient has.

## M3.0 — Acceptance

1. Two daemons on different machines converge on a shared document.
2. Both go offline, both edit, both reconnect — and converge.
3. A peer joining with only a link catches up **without the origin being
   online**. This is what makes it store-and-forward rather than a relay.
4. Carets and presence cross machines.
5. An agent on either machine can edit a shared document, and its edit arrives
   attributed to it rather than to "somewhere else" (M3.3).
6. Deleting a document propagates, and does not resurrect on the other side.

## M3.1 — Extract `thought-sync`

The `Frame` codec lives in `thoughtd/src/sync.rs`, where a separate relay binary
cannot reach it. It moves to its own crate along with the wire fixture, and both
the daemon and the relay depend on it. The fixture gate then covers three
implementations rather than two.

## M3.2 — The relay

Store-and-forward per document. It moves opaque frames and does not need to
understand documents — except for compaction, which does need `yrs`, and which
is the reason deferring encryption buys anything at all (AD-7).

* **Auth:** the client sends `share_id = SHA256(secret)`. The secret stays in
  the URL fragment and never reaches the server, so it can later become the
  content key without changing how joining works.
* **Storage:** SQLite, the same append-only shape as `thought-store` minus the
  actor tables — the relay has no opinion about who anyone is.
* **Fly:** one machine, one persistent volume, single region. This is stateful
  and not horizontally scalable as designed; two replicas would each hold half
  the story. Worth knowing before someone scales it up expecting it to work.

## M3.3 — Attribution has to cross the hop

**Found while planning, and it is not small.** Yjs updates carry no author
(AD-1), which is exactly why AD-6 insisted on an op log with actor ids from the
first commit. But that log is *local*. Nothing in the wire protocol carries an
actor, so an update arriving from the relay would be recorded as `origin=remote`
with no idea who wrote it — and every feature that rests on attribution (the
activity feed, per-run revert, the connections panel, authorship colour) would
work perfectly on one machine and go blank the moment a document is shared.

So `UPDATE` grows an actor descriptor: id, kind, display name, model. The
receiving daemon records it in its own log rather than inventing `remote`.

The uncomfortable part, stated plainly: **this is self-asserted and spoofable.**
A peer can claim any name. That is consistent with AD-6 — identity, not
authentication — but it stops being an internal detail the moment someone else's
name appears in your window. The UI has to distinguish "this machine saw this
person write it" from "a peer told me this person wrote it", or it is quietly
lying. Authentication is the answer eventually; saying so is the answer now.

## M3.4 — The daemon's relay client

`updates.synced_at` has existed since M1 and nothing has used it. It becomes the
queue: everything unsynced is what we owe the relay. Reconnect with backoff, the
same shape the window's provider already uses.

## M3.5 — Sharing

A `thought://join/<doc_id>#<secret>` capability URL, with the scheme registered so
macOS hands it to the app. Sharing produces the link; opening one joins. No
accounts, no inbox — possession of the link is the grant, so a link in a
screenshot is a leak, and the UI should say so where the link is produced.

Agents on the receiving machine get the document like any other, which follows
from sharing with a person meaning sharing with how they work.

## M3.6 — Not in M3

No accounts, no read-only links, no revocation, no E2E.

## The cost of deferring encryption, recorded

E2E was deferred deliberately (AD-7), so the exit path should be a decision
later rather than a surprise:

* The relay compacts because it can read frames. Encrypted, it cannot, so op
  logs grow without bound and every new joiner replays from zero.
* The replacement is client-side snapshots: peers periodically upload an
  encrypted snapshot the relay serves in place of the log. That is a protocol
  addition, not a flag.
* The share secret is already the right shape to become the content key, and
  `share_id` already hides it from the server. Nothing in M3 forecloses this —
  it just does not pay for it yet.
