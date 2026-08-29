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

- **[docs/architecture.md](docs/architecture.md)** — 17 decisions, each with its cost
  stated. Written to be argued with.
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

The app starts the daemon itself. Press ⌘K to switch documents, ⌘↵ to make one.

To watch an agent edit a document you have open, from another terminal:

```bash
scripts/agent watch 2
```

Rails appear in the left margin as it writes — dashed for an agent, solid for a person.
Hover one to see who wrote the block and when. A document only you have written shows
none, which is the point.

The daemon can also be run on its own, which is the whole point — agents do not need a
window:

```bash
cargo run -p thoughtd
```

It prints its port and writes `daemon.json` (mode 0600) with a bearer token. MCP clients
that speak HTTP can use the URL inside; clients that spawn a stdio server should spawn
`thought-mcp-stdio`, which reuses the published daemon or starts one only when nothing is
published, then proxies to it. A stale or unauthenticated discovery record is reported for
the developer to resolve instead of causing one process to replace another.

## Shape

A Rust daemon owns the CRDT and the SQLite store. The Tauri UI is a client, MCP agents are
clients, and the relay sync client is a client — all speaking one update protocol, so an
agent editing a document with no window open is the normal path rather than a special case.

Documents are a ProseMirror tree in a Yjs `XmlFragment`. Markdown is a projection used for
agent I/O, export, and search — never the storage format.

Attribution lives in the SQLite op log rather than the CRDT, because Yjs cannot carry it.
That log is what makes *who wrote this block* answerable at all, and it is never compacted
away.
