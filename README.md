# Proof of Thought

A local-first macOS writing app. Individual documents, real-time collaboration between
humans *and* agents, fully functional offline, syncing through a self-hostable relay.

*Proof of Thought* is the name on the window. Everything a machine resolves —
the crates, the `thoughtd` daemon, `ai.exalto.thought`, `THOUGHT_HOME`, the
`thought://` scheme — uses the short handle **thought**, which is the same split
`Visual Studio Code.app` makes when it identifies itself as `com.microsoft.VSCode`.

**M2 is complete**: the daemon, the window, and any number of agents edit one document at
once. An agent writing over MCP appears live in an open editor, and a rail in the left
margin says who wrote each block. `cargo test --workspace` is green and CI gates every push.

One thing is still owed from M2: the manual IME pass (AD-8), which needs a person at a
keyboard rather than a test. M3 — sharing through a relay — is planned but not built.

- **[docs/architecture.md](docs/architecture.md)** — decisions, each with its cost
  stated. Written to be argued with.
- **[docs/provenance.md](docs/provenance.md)** — what text lineage claims, stores, and
  deliberately does not claim.
- **[DESIGN.md](DESIGN.md)** — one accent, one mark, and the file each is
  defined in.
- **[prototypes/editor-probe](prototypes/editor-probe)** — throwaway probe answering
  whether WKWebView can host a collaborative ProseMirror (AD-8). It can: identical to
  Chromium on all five automated checks.

## Crates

| Crate | What it holds |
| --- | --- |
| `thought-schema` | The document model, `schema.json`, content-expression validation |
| `thought-markdown` | The markdown projection, both directions, property-tested |
| `thought-core` | yrs documents, block identity, block-scoped edits |
| `thought-provenance` | Current visible-text lineage |
| `thought-store` | SQLite op log, snapshots, actors, search |
| `thought-mcp` | The agent tool surface, with no transport attached |
| `thoughtd` | The daemon: MCP over loopback HTTP |
| `thought-testkit` | Document generators shared by the property tests |

## Install

Releases attach a universal macOS `.dmg`; see [docs/releasing.md](docs/releasing.md)
for how one is built and signed.

## Try it

```bash
npm run tauri dev --prefix app
```

The app starts the daemon itself. Press ⌘K to switch documents, ⌘N to create a blank
document in its own window, ⌘O to import a Markdown snapshot into its own window, and ⌘S
to export a one-time Markdown copy of the visible document. The toolbar reports Autosaved
only after the daemon confirms the latest edit reached SQLite. New documents never replace
the editor you are already using, and each new window is visibly cascaded from its source.
⌘W closes the window without prompting because Proof of Thought's CRDT store remains
authoritative. Export is an explicit one-time action and never establishes a mirrored file.

The daemon can also be run on its own. Add reviewers from the AI support sidebar. Each saved
connection gets its own credential and current-document or all-document scope:

```bash
cargo run -p thoughtd
```

It prints its port and writes `daemon.json` (mode 0600) with the native app capability.
Reviewer setup commands spawn `thought-mcp-stdio --connection <id>`; the shim reads that
connection's separate credential from an owner-only file and never prints it. It reuses the
published daemon or starts one when discovery is stale. Process-lifetime locks decide which
daemon may publish and open SQLite; file presence and PIDs do not.

The sidebar provides setup for ChatGPT desktop, Codex, Claude Desktop, and Claude Code. Connected
reviewers read and suggest by default. A reviewer can request direct editing for one document,
which the user must approve in the native editor. That access lasts while the configured AI
connection remains open, including idle periods, and has no countdown. A normal close revokes it
promptly. If the client disappears unexpectedly, daemon cleanup revokes it within roughly five
minutes of the last activity. An alive but hung client can retain access while its stdio connection
stays open, so the app keeps manual Revoke visible. Create, trash, and restore remain unavailable.
Last-used time may include the bridge's standard MCP keepalive. It is not a document edit, model
action, verified identity, or verified live presence.

## Shape

A Rust daemon owns the CRDT and the SQLite store. The Tauri UI is a client, MCP agents are
clients, and the relay sync client is a client — all speaking one update protocol, so an
agent editing a document with no window open is the normal path rather than a special case.

Documents are a ProseMirror tree in a Yjs `XmlFragment`. Markdown is a projection used for
agent I/O, export, and search — never the storage format.

Attribution lives beside the CRDT because Yjs cannot carry it. The append-only op log answers
who changed a block and when. Current text-lineage spans answer which recorded mutation
introduced each surviving grapheme; they do not pretend to be tamper-proof history. The AI
support sidebar groups those current sources without inventing a second proof or history API.
