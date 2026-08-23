# Polar — Architecture (v0)

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

### AD-2 — Daemon-first: `polard` owns the store

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

### AD-5 — Agent edits are anchored, surgical, and default to a suggestion layer

There is deliberately **no `update_document(id, full_markdown)` tool**. Whole-document
replacement produces a diff touching every block, destroys concurrent human edits, and
makes attribution meaningless. Agent tools address blocks by ID and edit ranges (§4).

Agents write into a suggestion layer by default; direct write is a per-session grant.
Suggestions live in the document CRDT so they replicate to other peers.

### AD-6 — Actor identity now, authentication later

Every device and every agent has a stable actor ID, kind, and display name. Self-asserted,
unverified, no accounts. This is *not* auth — it's what makes attribution, per-actor undo,
and accept/reject possible.

**Why now:** retrofitting identity into an op log that lacks it leaves all pre-migration
history permanently anonymous.

### AD-7 — Capability-URL sharing over a store-and-forward relay; no E2E in MVP

Sharing a document produces `polar://join/<doc_id>#<secret>`. The fragment never reaches
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
┌─ Polar.app (Tauri shell) ──────────────────┐
│   ProseMirror/TipTap ↔ Yjs (view replica)  │
└──────────────────┬─────────────────────────┘
                   │ IPC — Yjs update frames + awareness
┌──────────────────▼─────────────────────────┐
│  polard (Rust, launchd agent)              │
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
├─ "suggestions"  Y.Map<id, Suggestion>    agent proposals, replicated
└─ "comments"     Y.Map<id, Comment>       range-anchored threads
```

Anchors are encoded Yjs `RelativePosition`s, which survive concurrent edits. That property
is the entire reason suggestions and comments can be trusted under agent load.

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

-- Derived read-model, rebuilt from the doc CRDT. Authority is Y.Doc "suggestions".
CREATE TABLE suggestion_index (
  id          TEXT PRIMARY KEY,
  doc_id      TEXT NOT NULL REFERENCES documents(id),
  actor_id    TEXT NOT NULL,
  status      TEXT NOT NULL,             -- 'pending' | 'accepted' | 'rejected'
  created_at  INTEGER NOT NULL
);

CREATE VIRTUAL TABLE doc_fts USING fts5(
  doc_id UNINDEXED, title, body, tokenize='porter unicode61'
);
```

---

## 4. MCP tool surface

Exposed by `polard` over stdio and localhost HTTP. Available whether or not the UI is running.

**Read**

```
list_documents(query?, limit?)
  -> [{ doc_id, title, updated_at, word_count }]

read_document(doc_id, format="markdown")
  -> { markdown, version, blocks: [{ block_id, type, line_range }] }

search(query, limit?)
  -> [{ doc_id, block_id, title, snippet }]
```

**Write** — every call takes the `version` from the last read.

```
replace_block(doc_id, block_id, markdown, version)
insert_blocks(doc_id, after=block_id|"start", markdown, version)
delete_block(doc_id, block_id, version)
replace_text(doc_id, block_id, find, replace, occurrence?, version)
comment(doc_id, block_id, body)
```

On a stale `version`, the daemon **warns and proceeds** rather than rejecting — the CRDT
merges correctly regardless; the risk is semantic (the agent reasoned about text that has
since changed), so the right response is to tell the agent what moved, not to fail the write.

In suggestion mode (the default) every write lands in the `suggestions` map for
accept/reject instead of mutating `content`. Direct write is granted per session.

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
`polard` is a standalone binary the app happens to spawn, so the switch costs no code.
**Consequence that is not deferrable:** MCP transport must be **HTTP on localhost** with the
port in a well-known file, plus a stdio shim that proxies to it and only spawns the daemon if
absent. A plain stdio MCP server would let each agent client spawn its own daemon — two
processes writing one SQLite store.

### AD-11 — ⌘Z is scoped to your own edits; agent runs get "Revert this run"
Undoing a collaborator's edit is the classic violation and agents get no exception. But agent
edits are discrete batches keyed by `session_id`, so per-run revert is a separate affordance
on the attribution chip.

### AD-12 — Markdown dialect: CommonMark + GFM tables, strikethrough, task lists
Nothing else in v0. It is the dialect models emit most reliably without prompting.
**`parse(serialize(doc)) == doc` is a property test from day one.** A node that cannot
round-trip does not enter the schema.

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
with one more condition on the flush. AD-15 helps independently — in suggestion mode agent
writes never touch the text layer at all.

Consequence: an IME failure is a bridge bug, not a reason to abandon the webview.

### AD-15 — Direct writes for local unshared docs, suggestion mode once shared
Per-session override either way. The reason to gate agent writes is other people, not the
agent; gating solo local editing is friction with no beneficiary.

## 7. Explicit non-goals for MVP

Folders and hierarchy · accounts and authentication · end-to-end encryption · mobile ·
plugins · version-history UI · `.md` file mirroring on disk.
