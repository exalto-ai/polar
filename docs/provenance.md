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
