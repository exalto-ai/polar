# Text lineage

Text lineage answers one question: **which recorded mutation introduced each grapheme that is
still visible?** It does not replace the CRDT, the update log, or block attribution.

## The model

Each committed mutation creates one source event. Each surviving text span points to an event.
A source says:

- how the change entered: typing, command, import, MCP, API, or suggestion;
- how strongly that origin is known: observed, reported, verified, or unknown;
- how the mutation maps to the text: exact, inferred, or unknown.

These are separate claims. An MCP caller may report its name and model, but the local daemon did
not observe who is behind the connection. Its assurance is therefore `reported`. Content made
before lineage existed is `legacy_unknown`, not reconstructed and presented as certain.

The bundled editor captures one ProseMirror dispatch, its before/after ranges, and the Yjs update
that dispatch emitted as one frame. Frames remain separate in a FIFO until SQLite acknowledges
them. There is no batch protocol or retry id: if an acknowledgement is lost, resending the same
Yjs update is a no-op and can be acknowledged safely. The editor stays read-only until its first
sync frame is applied, so a range never describes an unhydrated placeholder document.

This is a product trust boundary, not authentication of a human. The same loopback bearer guards
MCP and editor access to the user's private writing. It does not make self-asserted MCP identity
observed, and adding another bearer would not protect against a hostile process running as the
same OS user.

Creating, importing, trashing, and restoring from the window use the daemon's narrow `/editor`
routes. Agent-facing MCP tools always create agent actors; they cannot select `kind: human`.
Both surfaces use the same bearer because it protects the same private store. Their different
routes describe which product boundary observed an operation, not different user accounts.

## Storage

SQLite has two authoritative tables:

- `provenance_events` stores one event per committed update. Its id is the update-log sequence.
- `lineage_spans` stores the source event for each current UTF-16 text span.

One transaction commits the CRDT update, event, current spans, search index, tombstone cache,
and existing block-attribution rails. A failed transaction changes none of them. Snapshots are
still a load-performance cache; the update log is still the historical record.

Only current spans are stored. Deleted text needs no live span, and historical questions use the
update log. On restart the daemon loads the spans directly rather than replaying history.

## Alignment

An operation that provides validated before/after text ranges is `exact`. Text outside those
ranges must be unchanged. Equal prefixes and suffixes inside a range retain their earlier source;
the replacement belongs to the new event.

If a boundary cannot provide ranges, the daemon preserves only the common prefix and suffix within
stable block ids and marks the event `inferred`. Multiple edits inside one block are deliberately
collapsed into one conservative changed region. It must never be displayed as exact.

Offsets are UTF-16 because that is the editor's coordinate system. Boundaries are grapheme-safe,
so an emoji or combining character is never split.

## Deliberate omissions

There is no receipt table, hash chain, event replay engine, persisted lineage cache, or separate
format and structure delta language. Those mechanisms duplicate existing history, add migration
and recovery states, and imply verification that a local bearer token cannot provide.

Later evidence or trace features may refer to a source event. They do not need a second lineage
ledger.

## Current sources view

The AI support sidebar reads `document_lineage` directly and groups sources already present in
the response. It shows labels, assurance, and alignment. It does not expose percentages, build a
separate consumer ledger, or replay event history in the browser.

Each response includes a SHA-256 revision of the normalized Markdown projection. The native
window computes the same revision from its visible editor tree and displays sources only when the
two match. Pending saves, invalid editor trees, and stale responses fail closed. This binds the
labels to the wording and formatting on screen; it is not a signature, timestamp, or proof of
origin.

## Reviewer connections

Each configured reviewer has a durable connection id, a unique credential, and an explicit
document scope. This layer is read-only: writes wait for the suggestion layer or a future
expiring session grant. The configured app and any model it reports are attribution labels, not
provider authentication. Revoking one connection invalidates its credential without affecting
other reviewers.

## Provider setup

Built-in provider keys are entered and stored by native macOS code. A model-catalog check does not
change the document and creates no lineage or verified evidence. Later features must bind any
provider exchange to the exact accepted change before presenting it as provider-verified.
