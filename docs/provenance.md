# Delta provenance and AI evidence

**Status:** accepted product and architecture direction; delta foundation, anchored-evidence
prerequisites, and the onboarding shell implemented, 2026-08-26

**Provenance stack:** `codex/provenance-delta-foundation` (PR #10), followed by
`codex/provenance-anchors` (PR #11), then `codex/ai-support-onboarding`
**Stack base:** `codex/tauri-ci-smoke`, after the separate
`codex/editor-toolbar-branding`, `codex/brand-assets`, `codex/daemon-single-owner`,
`codex/document-lifecycle`, `codex/sync-store-durability`, and
`codex/macos-graceful-quit` branches

The feature-by-feature completion record for the remaining consumer stack lives in
[`docs/upgrade-roadmap.md`](upgrade-roadmap.md). It is the release checklist for onboarding,
connections, the provenance view, suggestions, Pro, and Seal.

This document records the choices behind Proof of Thought's next feature series. It is
written for reviewers as much as implementers: the product language, evidence claims,
storage model, algorithms, limits, migrations, and pull request boundaries belong together.
A change that weakens a claim must change this document in the same pull request.

## 1. Target product shape

The consumer experience offers three progressively richer ways to work. This table is the
delivery target for the full stack. The current onboarding pull request ships the Basic and
Connect choice plus a transparent setup sidebar, while later pull requests deliver durable
connections, suggestions, Pro, and publishing:

| Experience | What the person gets | Evidence available | Cost language |
| --- | --- | --- | --- |
| Basic | Local writing with written-versus-pasted provenance | Locally recorded edit deltas | Proof of Thought initiates no AI request |
| Connect, recommended | One or more ChatGPT, Claude, Codex, or Claude Code reviewers; AI edits can arrive directly, avoiding the reviewer copy-and-paste loop; suggestions with Accept and Reject | Semantic MCP tool deltas and anchored suggestion deltas; app and model are reported by the connection | Uses the AI access the person already has; no separate Proof of Thought API billing |
| Pro | AI chat, files, model choice, and reasoning controls inside Proof of Thought | Provider-authenticated traces bound to exact suggestions and accepted document deltas | Uses the person's OpenAI or Anthropic API key; provider usage charges apply |

In the current onboarding pull request, Connect is the recommended path. It cannot activate
without consent, so
"recommended" means the primary card, not an automatic connection. Basic is the state after
choosing **Write locally**. Pro adds built-in AI and does not need to disconnect existing reviewers.

The planned consumer UI does not lead with MCP, TLS, proxy, CRDT, or API transport terminology.
Those details remain available under a technical disclosure.

## 2. The central claim is a delta, not a document label

Proof of Thought records how the visible document changed. It does not classify an entire
paragraph or document according to the last actor that touched it.

If a person supplies 1,000 words and Claude changes 12 for grammar, the surviving 988 words
retain their earlier source. Only the inserted or replaced wording belongs to the Claude
change. The activity history also records deletions, but deleted text does not count toward
the source breakdown of the current document.

The interface may say:

- `Claude changed 12 words`
- `7 suggestions accepted`
- `Written here 82% · Pasted 14% · Claude (reported) 4%`

This consumer breakdown groups currently surviving event contributions by stable source identity.
For example, separate direct-entry events appear as one `Written here` total, while connected
reviewers remain separate by connection. The event-level contributions and spans remain available
unchanged for forensic inspection. A group reports how many currently contributing events it
contains, and uses the label from the newest currently contributing event in that group.

It must not say `90% AI generated` merely because an AI operation touched a block. It also
must not use `AI generated` for a percentage that actually measures surviving text inserted
or replaced by an AI change. The accurate term is **contribution to the current wording**.

There is no `Mixed sources` badge. When a passage has more than one source, the interface
shows the concrete sources and their proportions or highlights the surviving spans.

Version 1 has one explicit nonclaim. A before-and-after tree cannot reveal which occurrence
survived when identical text from different sources admits several equally valid alignments.
For example, deleting one `yes` from `yesyes` is ambiguous if each copy has a different source
and no trusted transaction range was retained. Version 1 records its stable tie-break result as
`deterministic_inference`, not an exact observation of user intent.

Chain version 2 binds validated operation ranges to the event and uses them as exact alignment
boundaries. Current wording is consumer-eligible only when every event still contributing a
visible source uses V2. A document with surviving V1 and V2 sources reports `mixed`; a document
whose surviving sources are all V2 reports `anchored`. A V1 event that contributes no surviving
wording, such as a tombstone-only command or a fully deleted insertion, does not permanently
poison the document. Consumer percentages remain hidden until the separate real-WKWebView IME
gate is complete, even when the stored lineage is otherwise eligible.

## 3. Claims and labels

Every claim has three independent dimensions:

1. **Actor:** local writer, connected reviewer, built-in reviewer, remote peer, or unknown.
2. **Ingress:** entered, pasted, imported, editor command, MCP tool, API tool,
   suggestion, current unknown, or legacy unknown.
3. **Assurance:** observed, reported, verified, or unknown.

These dimensions must not be collapsed into one `writer_class` field. A paste is an ingress
method, not evidence of human or AI authorship. A connected reviewer is AI for the consumer
interface, while its exact app and model identity remain reported rather than provider
authenticated.

### Consumer labels

| Evidence | Primary label | Precise meaning |
| --- | --- | --- |
| Direct local input | `Written here` | Proof of Thought observed direct editor input. Dictation, accessibility software, macros, or retyping may look the same. |
| Clipboard input | `Pasted` | Proof of Thought observed clipboard insertion. It does not know who composed the clipboard content. |
| File input | `Imported` | Proof of Thought observed content entering through its file import path. |
| Editor command | `Edited here` | Proof of Thought observed a toolbar, undo, cut, or other editor command. This is not asserted to be directly typed text. |
| Unclassified current input | `Unclassified change` | A current update arrived without a reliable input-source signal. Proof of Thought does not guess. |
| Configured MCP connection | `ChatGPT (reported)`, `Claude (reported)`, `Codex (reported)`, or `Claude Code (reported)` | Proof of Thought observed a tool operation through that configured connection. It does not authenticate the upstream provider conversation or exact model. |
| Future built-in provider request with proof | `Claude (verified)` or `OpenAI (verified)` | Provider evidence authenticates the disclosed response and model-emitted tool call, and Proof of Thought binds that call to the exact local delta. |

`Verified` does not mean a response was correct, safe, or useful. It means the available
evidence authenticates the provider exchange and its binding to a Proof of Thought record.
The foundation reserves this assurance value for the Pro verifier. No production constructor or
network path in this pull request can emit `verified`.

## 4. Invariants

The implementation must preserve all of these:

1. Equal visible graphemes keep their existing source across a later edit.
2. Inserted or replaced graphemes receive the source of the new event.
3. Deleted graphemes leave the current source breakdown but remain in immutable history.
4. Formatting-only changes create an activity event without changing text lineage.
5. Structural changes preserve equal visible text whenever a deterministic mapping exists.
6. A rejected suggestion changes no live lineage.
7. Accepting a suggestion attributes its inserted text to the proposing reviewer, not the
   person who clicked Accept.
8. Different ingress sources are never coalesced into one provenance event.
9. Provider and model metadata live on the event or run, not on a mutable actor row.
10. The public MCP caller cannot create a trusted human provenance claim, hide its AI activity by
    choosing the legacy human or editor kind, or choose the assurance level. The legacy kind field
    remains accepted on the wire until connection identities replace caller-supplied actors.
11. Old documents are `legacy unknown` unless surviving provenance can be rebuilt without
    inventing facts.
12. A failed mutation commits neither the document update nor partial provenance.

## 5. Semantic delta model

Yjs updates remain the authoritative document history. They are not a useful consumer delta
by themselves: an implementation can delete and recreate an unchanged subtree while the
visible text changes by one character. Provenance therefore records a semantic before-and-
after delta alongside the opaque Yjs update.

### Flattening

The normalized ProseMirror tree is flattened into visible Unicode grapheme clusters with:

- stable block identity where available;
- the path to the containing text node;
- UTF-16 start and end offsets, matching JavaScript, ProseMirror, and Yjs positions;
- formatting and structural information stored separately from text lineage.

### Alignment

Stable block IDs partition unchanged structure. Within changed regions, the semantic engine
aligns extended Unicode grapheme clusters. Version 1 uses deterministic LCS-style alignment and
nearest-position tie-breaking. It does not infer user intent when identical text is ambiguous.
That behavior is frozen and remains available for old and fallback evidence.

For each local TipTap dispatch, the webview combines the root ProseMirror transaction and every
appended plugin transaction, then records all changed ranges in the complete input and output
documents. Those positions use ProseMirror's UTF-16 coordinate space. The daemon
validates each boundary against the exact before and after trees, rejects positions inside a
grapheme, and converts valid positions into global grapheme ranges. The ranges must remain
ordered and non-overlapping. The reconciler also verifies that visible text outside the ranges is
identical. Only then does the event use chain V2 and `anchored` alignment.

Each persisted anchor records its basis, ordered before and after grapheme ranges, and
domain-separated hashes of the corresponding text slices. The ordered anchor list is also part
of the event hash. Initial document creation and native Markdown import use one exact
`server_operation` anchor from the empty document to the full initial snapshot. Editor changes
use `editor_transaction` anchors.

Missing, empty, malformed, out-of-range, mid-grapheme, incomplete, or semantically inconsistent
editor hints do not block the CRDT mutation. The event falls back as a whole to frozen V1, stores
no anchors, and makes any wording it contributes ineligible for exact consumer percentages. The
implementation never records a partially trusted V2 event.

### Applying lineage

- Equal grapheme: copy the prior source event.
- Delete: append an immutable deletion segment carrying the deleted source, then remove it
  from the live view.
- Insert: assign the current source event.
- Format or structure: append an activity segment and preserve the text source.

In memory, source IDs may be expanded to one per grapheme for alignment. They are compressed
into adjacent runs before persistence.

### Percentages

Current contribution percentages use surviving non-whitespace Unicode grapheme clusters,
including punctuation. Event summaries additionally use natural units such as words and
punctuation marks. One headline must never combine denominators. The lineage API keeps
event-level contributions and exact spans for forensic inspection, then separately exposes
consumer groups. Repeated local events group by ingress, connected reviewers group by stable
connection identity when available, and future verified provider events group by provider.
Grouping never erases the source event behind a span. Frozen source labels live on events so a
restart or later actor rename cannot change the consumer breakdown.

### Performance boundary

The implementation remains correctness-first. Each durable mutation currently snapshots and
aligns the complete visible tree, hashes the complete before and after snapshots, serializes the
current projection, and replaces the complete live-span cache. Normal hydration can trust and
verify the persisted lineage cache; deleting that cache intentionally exercises a much more
expensive full-history recovery path.

An opt-in release benchmark uses a 10,000-word document and a 100-event anchored history. On the
reference Apple M4 Max development machine it measured a 60.86 ms anchored interactive commit,
a 51.84 ms cached cold open, and a 5.72 s 101-event cache-recovery replay. The reference budgets
are 100 ms, 150 ms, and 10 s respectively. These are a regression gate for that machine, not a
universal end-user latency claim or a shared-CI wall-clock assertion. Run it with:

```text
cargo test --release -p thought-mcp --test provenance_performance -- --ignored --nocapture
```

The editor may transport up to 128 immutable mutations in one ordered batch, but batching never
coalesces their provenance events. Each complete editor dispatch keeps its own source, client
event ID, range list, Yjs update, and retry identity. The daemon acknowledges only after the
complete batch has been processed durably.

## 6. Durable and derived data

Within the production `Workspace` mutation path, the immutable truth is:

- the existing append-only Yjs update log;
- one provenance event for each canonical mutation or decision;
- ordered semantic delta segments for that event.

The current live-span table is derived state. V1 can drop and rebuild it by replaying V1 update
and provenance event logs with the frozen V1 reconciler. Existing `block_provenance` remains
temporarily for the M2 rails and is compatibility data, not the new source of truth. The store
crate retains low-level compatibility helpers that do not create provenance events; production
document mutations must go through `Workspace` to receive the atomic evidence guarantees below.

Schema V2 separates five responsibilities, and schema V3 adds the immutable anchor evidence:

| Table | Role | Mutation rule |
| --- | --- | --- |
| `provenance_events` | Event envelope, frozen actor/model/source labels, input and assurance, document hashes, cumulative Yjs update-log root, prior-event hash, and canonical event hash | Append only |
| `provenance_changes` | Ordered insert, delete, format, and structure deltas with typed locations and source-event references | Append only |
| `provenance_anchors` | Ordered V2 anchor basis, before and after grapheme ranges, and hashes of the anchored text slices | Append only |
| `provenance_receipts` | Later MCP, provider, device, or Seal evidence that strengthens an event without rewriting it | Append only |
| `lineage_spans` | Current surviving UTF-16 ranges and their source events | Replaceable derived cache |
| `lineage_state` | Algorithm version, rebuild watermarks, readiness, and digest for one complete span generation | Replaceable derived cache |

The database enforces append-only evidence with update and delete rejection triggers. Exact Yjs
payloads and their immutable document, sequence, actor, origin, session, and timestamp metadata
cannot be changed or removed after commit. The relay acknowledgement field `synced_at` remains
mutable operational state and is not evidence. Each span references an event in the same
document. Every source reference in a semantic change must also belong to the same document and
must not point to a later event. Provider and model fields are copied onto the event so changing a
reusable actor row cannot rewrite history.

The before and after document digests cover the normalized visible tree plus the replicated
document tombstone. Each event also binds a cumulative, document-local update-log root through
its update sequence. That root folds the exact opaque Yjs payload and immutable update metadata
for every row in order. A legacy seed adds no new update, but its root covers all legacy update
rows through the baseline it records. This lets restart verification detect evidence mutation
that a semantic tree digest alone cannot see. Verification hashes the raw stored origin text;
typed activity consumers separately reject values outside `human`, `agent`, and `remote` so an
unknown legacy or tampered value is never silently normalized.

The local event chain is deterministic and tamper-evident under an honest store, but it is not
yet signed or externally anchored. A process able to replace the database and recompute the
entire chain can rewrite it. Device signing and optional live Seal anchoring are required before
the product may claim resistance to that attacker.

SQLite migrations are ordered, transactional, and versioned with `PRAGMA user_version`.
Schema V1 adopts the released schema without rewriting user data. Schema V2 creates the event
ledger and derived lineage tables. Schema V3 adds anchors and rebuilds the affected evidence
tables with their foreign-key relationships while preserving V2 rows and SQLite sequences. A
newer database is refused explicitly. The exact DDL is the review authority in
[`crates/thought-store/src/schema.rs`](../crates/thought-store/src/schema.rs).

Database schema versions and event-chain versions are intentionally separate. Chain V1 freezes
the original evidence byte encoding and deterministic reconciler. Chain V2 adds a
domain-separated ordered anchor list to that encoding and dispatches to the anchored reconciler.
The V1 regression digest is frozen in tests. Hydration verifies and replays V1 and V2 event by
event, so a migrated document may contain both. This build rejects every other chain version.
Any future evidence version must add version-dispatched hashing, validation, and reconciliation
while leaving both old suites readable; individual internal digest constants are not independent
compatibility promises.

The evidence digest root and domain separators use the machine namespace `thought`, including
`thought/canonical-evidence`, `thought/document`, `thought/event-chain`,
`thought/yjs-update-log`, `thought/live-lineage`, and the V2 `thought/anchor-text` domain. This
namespace correction predates a release and intentionally invalidates evidence produced by
development builds that used the old product-name strings. Once released, changing any of these
bytes requires a new format version, a migration plan, and a verifier that can dispatch across
every supported version.

A normal edit commits its actor registration, Yjs update, event, ordered changes, complete live
spans, lineage watermark, title, deletion state, search projection, and compatibility block rails
in one transaction. A rejected edit cannot rewrite the mutable actor display row. The app
installs the candidate in memory only after that transaction succeeds. Snapshots remain a
discardable follow-up cache and cannot make an already committed edit look failed. Trash and
restore are explicit event actions because they change replicated document state without changing
visible text.

### Existing databases

The first migration preserves every existing table and update. On hydration, a document with
no provenance events receives one `legacy_seed` event for its current visible content. Its
ingress and assurance are `legacy_unknown` and `unknown`. The migration must not infer typed,
pasted, or AI history from old block-level attribution.

## 7. Trusted ingress metadata

The editor captures input source at the complete TipTap dispatch boundary. Paste detection is
event-based, not a content heuristic. Direct input, paste, undo, toolbar command, and import
map to their current source categories even if two produce identical text. Drag-and-drop is
conservatively `unknown` in V1 because the closed wire vocabulary does not yet represent it.
Any other unobserved current update is also `unknown`; `legacy_unknown` is reserved for history
created before this feature.

That metadata travels with the Yjs update through the sync envelope. One transport frame may
batch several queued editor dispatches, but it never merges them into one evidence event. Each
dispatch retains its original source, UTF-8 client event ID, ordered range hints, and exact
Yjs update. Retries resend the same immutable ordered batch until one acknowledgement confirms
that the daemon durably processed the whole batch. Strict limits allow 1 to 128 mutations per
batch, 0 to 64 ranges per mutation, a 1 to 64 byte client event ID, and a nonempty update.
Repeated subscriptions are idempotent. If a window falls behind the bounded live broadcast
buffer, the daemon first subscribes it at the current channel tail, sends a full authoritative
Yjs snapshot, and then resumes live delivery. Any overlap is safe because Yjs updates are
idempotent.

The daemon assigns assurance from the trusted ingress path:

- classified editor sync (`entered`, `pasted`, `command`, or `imported`): observed;
- unclassified editor sync: unknown;
- configured external MCP connection: reported;
- future built-in provider path with verifier-accepted evidence: verified;
- relay peer: peer-reported until stronger authentication exists.

Caller-controlled tool arguments never select `verified` or another trusted provenance class.
The older block-rail actor kind remains accepted for wire compatibility, but public MCP ignores
it for both activity actors and semantic provenance.

### Foundation exposure boundary

The editor sync endpoint and public MCP endpoint use different bearer capabilities. Possession
of the MCP capability must not authorize a sourced editor update, and possession of the editor
capability must not authorize an MCP tool call. This is a protocol trust boundary, not a defense
against a hostile process running as the same operating-system user that can read Proof of
Thought's private application state or inject code into its webview. Stronger device claims need
the signing and anchoring work described below.

Native creation, File > Open, trash, and restore now use a narrow editor-only HTTP surface:
`POST /editor/documents` and `POST /editor/documents/{doc_id}/deleted`. The app authenticates
with the editor capability and sends no caller-selected actor or model identity. Document titles
are capped at 4 KiB and Markdown import at 2 MiB after JSON decoding. Imported initial text
receives observed `Imported` metadata and one whole-snapshot `server_operation` anchor. The
public MCP capability is rejected on these routes, while the editor capability remains unable to
call public MCP tools.

The anchor prerequisite pull request did not ship consumer setup. This stacked onboarding pull
request adds the first-launch choice and transparent sidebar shell only. It does not yet ship
durable reviewer connections, per-reviewer permissions, suggestions, API keys, or a new
provenance visualization. MCP block mutations that do not yet carry exact semantic operation
ranges remain V1 reported evidence. They can still support a weaker activity proof, but their
surviving wording is not eligible for exact percentages.

## 8. Suggestions and multiple reviewers

Connected reviewers default to suggestions rather than direct mutation. A suggestion stores:

- reviewer connection and reported model;
- base document revision;
- exact proposed delta;
- anchors and target hashes;
- optional explanation;
- proof or tool receipt reference;
- pending, accepted, rejected, or conflicted state;
- the decision event.

Suggestions appear inline with insertion and deletion styling plus Accept and Reject. Several
reviewers may propose changes simultaneously. Overlapping suggestions are alternatives:
accepting one makes an overlapping stale suggestion require review instead of silently
applying or deleting it.

The first delta-foundation PR reserves suggestion metadata but does not ship the suggestion
interface.

## 9. Seal publication and verification

Seal is the one publication and verification destination. There is no separate Verify
product. One document-centric page renders each claim at its own evidence strength:

- Basic: locally recorded written, pasted, and imported deltas;
- Connect: configured connection, MCP tool receipt, proposal, and decision;
- Pro: provider-authenticated trace bound to the exact proposal and accepted delta.

External MCP does not expose the complete private conversation inside ChatGPT or Claude.
Connect can publish the back-and-forth visible at the Proof of Thought tool boundary. Pro can
publish deeper provider request and response traces, subject to disclosure review.

The existing `.llmtrace` artifact authenticates provider exchanges, not local document or
MCP histories. A later cross-repository change therefore introduces a deterministic Proof of
Thought provenance bundle containing:

- canonical document revisions and deltas;
- a hash-chained event ledger;
- connection identities and reported metadata;
- suggestions and decisions;
- a Proof of Thought device or app signature;
- optional embedded or referenced `.llmtrace` packages.

Seal verifies the bundle and each nested provider trace independently. One verified provider
trace never upgrades unrelated local or reported events.

If Seal must prove that a local history existed before publication, Proof of Thought needs a
live anchoring protocol. It periodically sends a salted or blinded cumulative ledger root and
receives a signed timestamp receipt. Unsalted low-entropy document hashes must not leave the
device because they permit dictionary attacks. Uploading only at publication proves integrity
from publication onward, not contemporaneous recording.

## 10. Privacy and security boundaries

- Do not record raw keystrokes, clipboard history, or copy-out events.
- Store only the resulting document mutation, its coarse ingress, and the evidence needed for
  provenance.
- Pasted and imported content remain content with unknown prior authorship.
- API keys remain outside the webview, document database, logs, and evidence artifacts.
- Capture checkpoints remain encrypted private state. Only reviewed notarized disclosures are
  publishable provider proof.
- Proof publication is always explicit. Connecting a reviewer never publishes a document.
- The current shared local MCP capability grants workspace-wide document operations, including
  direct edits, even while no editor window is open. A configured AI app may send document content
  returned by these tools to its provider under that app's privacy terms. The setup shell discloses
  both boundaries. The connection layer must replace the shared capability with explicit
  per-connection permissions and unique credentials before presenting reviewers as configured or
  independently revocable.
- The development preview returns daemon capabilities only to a loopback socket, even if Vite is
  explicitly bound to a LAN interface for other assets.

## 11. Stacked delivery plan

The work is intentionally split because each layer has a different failure and rollback
boundary.

1. **Delta provenance foundation, PR #10:** versioned migrations, immutable events and semantic
   deltas, derived live spans, trusted ingress propagation, legacy seeding, and tests.
2. **Anchored evidence prerequisites, PR #11:** schema V3 anchors, chain V2 hashing and
   replay, validated editor ranges, immutable batched transport, editor-only native lifecycle,
   mixed-history compatibility, concurrency coverage, and a reference benchmark.
3. **Reviewer connection and onboarding:** the current stacked pull request ships simple Basic
   and recommended Connect choices plus the transparent sidebar shell. The next connection pull
   request adds durable connection identities, permissions, multiple reported reviewers, and the
   consumer provenance view before calling Connect complete.
4. **Replicated suggestions:** proposal state, inline visualization, Accept and Reject,
   conflict handling, and exact proposal-to-delta attribution.
5. **Pro provider path:** secure keys, built-in chat, model and reasoning controls, files, and
   provider trace binding.
6. **Seal bundle:** deterministic bundle, signing and optional live anchoring, publish flow,
   and the Seal page/verifier changes in the appropriate repository.

Consumer UI beyond the current onboarding and sidebar shell is not rendered or shipped until its
corresponding pull request is ready. Each pull request targets its predecessor while the stack is
open, then is retargeted or rebased onto `main` as predecessors merge.

## 12. Acceptance contract and current coverage

The foundation and anchored-evidence pull requests automate the claims they expose:

1. A grammar replacement attributes only the replacement and preserves every untouched
   grapheme source.
2. V1 duplicate text alignment is deterministic, including its documented cross-source
   ambiguity. V2 validated ranges disambiguate the selected occurrence.
3. Heading or paragraph type changes, paragraph split and merge, formatting-only changes,
   deletion, and later replacement preserve or change lineage according to the V1 rules.
4. Emoji, combining marks, grapheme boundaries, and UTF-16 offsets round-trip in the semantic
   engine. This is not a claim that a real IME interaction has been tested end to end.
5. Entered and pasted updates remain separate through the editor queue. Each complete editor
   dispatch retains its own immutable range hints and client event ID through batching, retry, and
   one durable ACK.
   All source values and legacy source-less updates cross the daemon wire without promotion.
6. An MCP caller cannot promote reported provenance by claiming a human actor.
7. Restart, cache deletion and rebuild, legacy seeding, empty documents, and failed persistence
   produce deterministic results without invented or partial evidence.
8. The event chain binds replicated metadata and a cumulative root over exact Yjs update bytes
   and immutable update metadata.
9. Source references cannot cross document boundaries or point forward in the event ledger.
10. Actor registration, document state, semantic evidence, and derived projections either commit
    together or remain unchanged.
11. Trash and restore use distinct event actions, and restart verification rejects a changed
    update payload, immutable update metadata, anchor material, or unsupported event-chain
    version.
12. A 100-word anchored grammar scenario changes only the requested wording. Cross-source
    repeated occurrences use the supplied range, and restart preserves the same result.
13. Actual concurrent Yjs inserts from two replica clients and two provenance sources remain
    distinct in live spans before and after restart.
14. Valid V1-only, V2-only, and mixed V1/V2 histories verify and rebuild event by event. Invalid
    ranges fall back atomically to V1 and disable exact consumer output only while their source
    still contributes wording.
15. Native create, import, trash, and restore use the editor-only capability. A public MCP token
    cannot invoke that path, and native import produces observed, anchored `Imported` lineage.
16. The opt-in 10,000-word, 100-event benchmark passes the documented reference-machine budgets.

The following gates remain before their corresponding consumer features ship:

1. A real Japanese or Pinyin IME transaction passes from WKWebView through the anchored envelope
   and restart. Automated composition-guard coverage is necessary but is not this manual claim.
2. Consumer contribution percentages remain hidden until that real IME pass is recorded.
3. Suggestion acceptance keeps the proposing source; rejection changes no live span. This lands
   with the replicated-suggestions pull request rather than being simulated in the anchor layer.
4. Reviewer connection identities and their permissions are exercised through onboarding before
   Connect is presented as configured.

Manual review must additionally confirm that public language says what the evidence establishes,
and no more.
