# Polar

A local-first macOS writing app. Individual documents, real-time collaboration between
humans *and* agents, fully functional offline, syncing through a self-hostable relay.

Nothing is implemented yet. What exists is a design and one risk probe.

- **[docs/architecture.md](docs/architecture.md)** — 17 decisions, each with its cost
  stated. Written to be argued with.
- **[prototypes/editor-probe](prototypes/editor-probe)** — throwaway probe answering
  whether WKWebView can host a collaborative ProseMirror (AD-8). It can: identical to
  Chromium on all five automated checks.

## Shape

A Rust daemon owns the CRDT and the SQLite store. The Tauri UI is a client, MCP agents are
clients, and the relay sync client is a client — all speaking one update protocol, so an
agent editing a document with no window open is the normal path rather than a special case.

Documents are a ProseMirror tree in a Yjs `XmlFragment`. Markdown is a projection used for
agent I/O, export, and search — never the storage format.
