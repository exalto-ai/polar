# Delta provenance and AI evidence

**Status:** accepted product and architecture direction, 2026-08-26

**First implementation stack:** `codex/provenance-delta-foundation`
**Depends on:** PR #9, editor document lifecycle and provenance rails

This document records the choices behind Proof of Thought's next feature series. It is
written for reviewers as much as implementers: the product language, evidence claims,
storage model, algorithms, limits, migrations, and pull request boundaries belong together.
A change that weakens a claim must change this document in the same pull request.

## 1. Target product shape

The planned consumer experience offers three progressively richer ways to work. This table is
the delivery target for the stack, not a claim that this foundation pull request ships the
onboarding or sidebar:

| Experience | What the person gets | Evidence available | Cost language |
| --- | --- | --- | --- |
| Basic | Local writing with written-versus-pasted provenance | Locally recorded edit deltas | No AI connection |
| Connect, recommended | One or more ChatGPT, Claude, Codex, or Claude Code reviewers; AI edits can arrive directly, avoiding the reviewer copy-and-paste loop; suggestions with Accept and Reject | Semantic MCP tool deltas and anchored suggestion deltas; app and model are reported by the connection | Uses the AI access the person already has; no separate Proof of Thought API billing |
| Pro | AI chat, files, model choice, and reasoning controls inside Proof of Thought | Provider-authenticated traces bound to exact suggestions and accepted document deltas | Uses the person's OpenAI or Anthropic API key; provider usage charges apply |

When onboarding ships, Connect is the recommended path. It cannot activate without consent, so
"recommended" means the primary card, not an automatic connection. Basic is the state after
choosing **Continue with Basic**. Pro adds built-in AI and does not need to disconnect existing
reviewers.

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
`deterministic_inference`, not an exact observation of user intent. User-visible contribution
percentages remain gated until the editor and MCP envelopes carry validated semantic range hints;
that anchored transport belongs in the next pull request.

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

Stable block IDs are preferred anchors. Between stable anchors, a deterministic diff aligns
the neighboring text region so paragraph splits, merges, and block-type changes can preserve
equal wording. Repeated text uses deterministic nearest-position tie-breaking. An operation
with a trusted exact range, such as a future suggestion decision, can supply that range as an
additional anchor in a later algorithm version.

Version 1 uses deterministic LCS-style alignment over grapheme clusters, with stable unchanged
blocks partitioning the work. It does not persist editor selection ranges or infer user intent
when identical text is ambiguous. Its deterministic tie-breaking is part of the evidence format;
adding trusted range anchors or changing the algorithm requires a new version and the old
verifier must remain available for recorded evidence.

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

Version 1 is correctness-first. Each durable mutation currently snapshots and aligns the complete
visible tree, hashes the complete before and after snapshots, serializes the current projection,
and replaces the complete live-span cache. Cold hydration verifies the full update/event history.
The editor queue can combine adjacent unsent updates only when their source is identical, but this
foundation makes no interactive latency or large-history startup claim. Before consumer lineage
or suggestions ship, realistic document and history benchmarks must set budgets and drive safe
batching, incremental alignment, incremental span updates, or checkpointed verification.

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

The V2 schema separates five responsibilities:

| Table | Role | Mutation rule |
| --- | --- | --- |
| `provenance_events` | Event envelope, frozen actor/model/source labels, input and assurance, document hashes, cumulative Yjs update-log root, prior-event hash, and canonical event hash | Append only |
| `provenance_changes` | Ordered insert, delete, format, and structure deltas with typed locations and source-event references | Append only |
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
Version 1 adopts the released schema without rewriting user data; version 2 creates the
evidence ledger and derived lineage tables. A newer database is refused explicitly. The exact
DDL is the review authority in [`crates/thought-store/src/schema.rs`](../crates/thought-store/src/schema.rs).
Chain version 1 freezes the complete evidence encoding and semantic reconciliation suite. This
build rejects every other chain version. A future version must first add a schema migration and
version-dispatched verifier and reconciler so old evidence remains readable; the individual
internal digest constants are not independent compatibility promises.

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

The editor captures input source at the ProseMirror transaction boundary. Paste detection is
event-based, not a content heuristic. Direct input, paste, undo, toolbar command, and import
map to their current source categories even if two produce identical text. Drag-and-drop is
conservatively `unknown` in V1 because the closed wire vocabulary does not yet represent it.
Any other unobserved current update is also `unknown`; `legacy_unknown` is reserved for history
created before this feature.

That metadata travels with the Yjs update through the sync envelope. The outbound queue may
merge adjacent updates only when their provenance metadata is identical. Retries preserve the
original metadata and acknowledgement ordering.

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

The current native File > Open and document-lifecycle controls inherited from PR #9 still call
the public `create_document` tool. The foundation therefore records visible text from that
existing import path conservatively as MCP and reported AI activity. The `Imported` sync
classification is implemented and tested, but a following consumer UI pull request must move
native import onto the editor-only capability before showing `Imported` to people. This pull
request also does not ship onboarding, the reviewer sidebar, suggestions, API keys, or a new
provenance visualization.

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
- A connected app receives only the permissions granted to that connection. Multiple reviewer
  identities must not share one undifferentiated external credential.

## 11. Stacked delivery plan

The work is intentionally split because each layer has a different failure and rollback
boundary.

1. **Delta provenance foundation:** versioned migrations, immutable events and semantic
   deltas, derived live spans, trusted ingress propagation, legacy seeding, and tests.
2. **Reviewer suggestions:** consumer onboarding, connected reviewer identities, inline
   suggestions, Accept and Reject, conflicts, and concrete delta visualization.
3. **Pro provider path:** secure keys, built-in chat, model and reasoning controls, files, and
   provider trace binding.
4. **Seal bundle:** deterministic bundle, signing and optional live anchoring, publish flow,
   and the Seal page/verifier changes in the appropriate repository.

Planned consumer UI is not rendered or shipped until its corresponding pull request is ready.
Each pull request targets its predecessor while the stack is open, then is retargeted or rebased
onto `main` as predecessors merge.

## 12. Acceptance contract and current coverage

The foundation pull request automates the claims it currently exposes:

1. A grammar replacement attributes only the replacement and preserves every untouched
   grapheme source.
2. Duplicate text alignment is deterministic, including its documented cross-source ambiguity.
3. Heading or paragraph type changes, paragraph split and merge, formatting-only changes,
   deletion, and later replacement preserve or change lineage according to the V1 rules.
4. Emoji, combining marks, grapheme boundaries, and UTF-16 offsets round-trip in the semantic
   engine. This is not a claim that a real IME interaction has been tested end to end.
5. Entered and pasted updates remain separate through the editor queue; all source values and
   legacy source-less updates cross the daemon wire without promotion.
6. An MCP caller cannot promote reported provenance by claiming a human actor.
7. Restart, cache deletion and rebuild, legacy seeding, empty documents, and failed persistence
   produce deterministic results without invented or partial evidence.
8. The event chain binds replicated metadata and a cumulative root over exact Yjs update bytes
   and immutable update metadata.
9. Source references cannot cross document boundaries or point forward in the event ledger.
10. Actor registration, document state, semantic evidence, and derived projections either commit
    together or remain unchanged.
11. Trash and restore use distinct event actions, and restart verification rejects a changed
    update payload, immutable update metadata, or unsupported event-chain version.

The following remain acceptance gates for the next pull request that enables consumer
percentages and suggestions:

1. Validated semantic range hints travel with editor and MCP updates and remain ordered across
   queue coalescing and retry.
2. The 100-word grammar scenario and cross-source repeated-occurrence edits use those anchors.
3. Actual concurrent Yjs inserts from different provenance actors remain distinct in live spans.
4. A real IME transaction passes from WKWebView through the sourced envelope and restart.
5. Suggestion acceptance keeps the proposing source; rejection changes no live span.
6. Interactive typing and cold-open benchmarks pass agreed budgets for realistic large documents
   and histories; correctness-preserving batching or incremental work lands where required.

Manual review must additionally confirm that public language says what the evidence establishes,
and no more.
