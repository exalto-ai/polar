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

- **[docs/architecture.md](docs/architecture.md):** 18 decisions, each with its cost
  stated. Written to be argued with.
- **[docs/provenance.md](docs/provenance.md):** the accepted delta-lineage, reviewer,
  evidence, and Seal contract for the next feature stack.
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

The app starts the daemon itself. Press ⌘K to switch documents, ⌘N to create a blank
document in its own window, ⌘O to import a Markdown snapshot into its own window, and ⌘S
to export a one-time Markdown copy of the visible document. The toolbar reports Autosaved
only after the daemon confirms the latest edit reached SQLite. New documents never replace
the editor you are already using, and each new window is visibly cascaded from its source.
⌘W offers to export a Markdown copy before closing. Proof of Thought's CRDT store remains
authoritative; an export does not establish a mirrored file on disk.

The daemon can also be run on its own. Reviewers connect through the verified stdio bridge,
so no development script or browser endpoint reads bearer capabilities directly:

```bash
cargo run -p thoughtd
```

It prints its port and writes `daemon.json` (mode 0600) with separate MCP and editor bearer
capabilities. MCP clients that speak HTTP use only the MCP capability; the app keeps the editor
capability for sourced sync updates. Clients that spawn a stdio server should spawn
`thought-mcp-stdio`, which reuses the published daemon or starts one only when nothing is
published, then proxies to it. The reviewer bridge is intentionally limited to macOS and Linux,
where it can prove listener ownership and the exact daemon executable before sending a bearer.
Its loopback requests ignore proxy settings, reject redirects, and use bounded timeouts. A
conclusively dead discovery record from protocol 3 through the current protocol can be removed
under the home and store locks; live, ambiguous, malformed, legacy, future, or locked records remain
untouched.

## Shape

A Rust daemon owns the CRDT and the SQLite store. The Tauri UI and MCP reviewers are clients.
A future relay sync client will speak the same update protocol. Reviewers can inspect documents
with no window open.

Documents are a ProseMirror tree in a Yjs `XmlFragment`. Markdown is a projection used for
agent I/O, export, and search. It is never the storage format.

Attribution lives in the SQLite op log rather than the CRDT, because Yjs cannot carry it.
That log is what makes *who wrote this block* answerable at all, and it is never compacted
away.
