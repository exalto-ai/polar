# Polar

A local-first macOS writing app. Individual documents, real-time collaboration between
humans *and* agents, fully functional offline, syncing through a self-hostable relay.

**M1 is complete**: a Rust daemon serves documents to agents over MCP, with no editor
anywhere. `cargo test --workspace` is green and CI gates every push.

- **[docs/architecture.md](docs/architecture.md)** — 17 decisions, each with its cost
  stated. Written to be argued with.
- **[DESIGN.md](DESIGN.md)** — one accent, one mark, and the file each is
  defined in.
- **[prototypes/editor-probe](prototypes/editor-probe)** — throwaway probe answering
  whether WKWebView can host a collaborative ProseMirror (AD-8). It can: identical to
  Chromium on all five automated checks. One manual IME pass is still outstanding.

## Crates

| Crate | What it holds |
| --- | --- |
| `polar-schema` | The document model, `schema.json`, content-expression validation |
| `polar-markdown` | The markdown projection, both directions, property-tested |
| `polar-core` | yrs documents, block identity, block-scoped edits |
| `polar-store` | SQLite op log, snapshots, actors, search |
| `polar-mcp` | The agent tool surface, with no transport attached |
| `polard` | The daemon: MCP over loopback HTTP |
| `polar-testkit` | Document generators shared by the property tests |

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

The daemon can also be run on its own, which is the whole point — agents do not need a
window:

```bash
cargo run -p polard
```

It prints its port and writes `daemon.json` (mode 0600) with a bearer token. MCP clients
that speak HTTP can use the URL inside; clients that spawn a stdio server should spawn
`polar-mcp-stdio`, which finds or starts the daemon and proxies to it.

## Shape

A Rust daemon owns the CRDT and the SQLite store. The Tauri UI is a client, MCP agents are
clients, and the relay sync client is a client — all speaking one update protocol, so an
agent editing a document with no window open is the normal path rather than a special case.

Documents are a ProseMirror tree in a Yjs `XmlFragment`. Markdown is a projection used for
agent I/O, export, and search — never the storage format.
