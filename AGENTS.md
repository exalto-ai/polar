# Proof of Thought agent guide

This file is the source of truth for agent instructions. `CLAUDE.md` is a symlink to it.

## Project map

- `crates/thoughtd/` is the daemon and the CRDT authority. The window, MCP agents, and the relay client are all clients of it, speaking the same update protocol (AD-2). `crates/thoughtd/src/bin/thought-mcp-stdio.rs` is the stdio shim that finds or spawns the daemon.
- `crates/thought-schema/` holds `schema.json`, the node and mark types, and validation. `crates/thought-core/` owns yrs documents, block identity, and anchors. `crates/thought-markdown/` is the markdown projection in both directions. `crates/thought-store/` is SQLite: op log, snapshots, actors, FTS. `crates/thought-mcp/` is the tool surface with no transport attached. `crates/thought-testkit/` holds the document generators the property tests share.
- `app/` is the Tauri window: TypeScript, TipTap, Vite. `app/src-tauri/` and `prototypes/` are deliberately outside the Cargo workspace and must not gate the build.
- [`docs/architecture.md`](docs/architecture.md) is the decision record — every decision numbered, with its cost stated. [`DESIGN.md`](DESIGN.md) governs the accent and the mark. [`docs/releasing.md`](docs/releasing.md) covers signing and release.
- The product is *Proof of Thought* in the interface and **thought** everywhere a machine resolves a name: the crates, `thoughtd`, `ai.exalto.thought`, `THOUGHT_HOME`, `thought://`.

## Non-negotiable trust boundaries

- The daemon binds loopback only and publishes its port and a 256-bit bearer token to `daemon.json`, readable by the user alone. That token guards the user's private writing: never widen the bind address, log the token, or relax the file mode. Reviewer connections use separate scoped credentials.
- There is deliberately no `update_document(id, full_markdown)` tool. Agent writes address blocks by ID and edit ranges, because whole-document replacement destroys concurrent human edits and makes attribution meaningless (AD-5).
- Agent writes land in the suggestion layer by default. Direct write is a per-session grant, not a default and not a global setting.
- A share link is `thought://join/<doc_id>#<secret>`. The fragment must never reach the relay — clients subscribe with `share_id = SHA256(secret)`, so possession of the link is the grant and the server never learns the secret.
- Generic actor, display, and model claims are self-asserted and unverified. A durable reviewer credential authenticates only the configured local Proof of Thought connection ingress. It never authenticates the caller app, provider, model, person, conversation, or those claims. Attribution and per-actor behavior must not be presented as upstream identity verification (AD-6, AD-21).
- There is no end-to-end encryption in the MVP: you host the relay, you trust it (AD-7). Do not describe the relay as private, and do not add a flag that implies encryption the protocol does not implement. The exit path is recorded at the end of `docs/architecture.md`.
- Awareness payloads are ephemeral and must never be persisted.

## Validate changes

Run the checks relevant to edited code before handing work off. CI builds with `RUSTFLAGS: -D warnings`, so a warning is a failure:

```bash
cargo fmt --all -- --check
cargo clippy --workspace --all-targets
cargo test --workspace
PROPTEST_CASES=20000 cargo test --workspace --test round_trip
npm ci --prefix app
npm run schema:check --prefix app
npm test --prefix app
npx tsc --noEmit --project app
```

The toolchain floor is Rust 1.95 because yrs 0.27 uses if-let guards; do not lower it.

## Working conventions

- TypeScript defines the schema. A build step writes `getSchema(extensions).spec` to `crates/thought-schema/schema.json` and Rust consumes it. Never hand-edit that JSON, and never let the two halves drift — `npm run schema:check --prefix app` is what catches it, and drift surfaces late as a bug that looks like a CRDT fault.
- `parse(serialize(doc)) == doc` is the one guard on the agent-facing contract. Whether a node round-trips decides which agent operations are safe on it, not whether it may exist (AD-12). Extend the generators in `thought-testkit` when you add a node or mark.
- On a stale `version`, the daemon warns and proceeds — it does not reject. The CRDT merges correctly; the risk is semantic, so tell the agent what moved (§4).
- Markdown is a projection for agent I/O, export, and search. It is never the storage format (AD-3).
- `docs/architecture.md` is written to be argued with. Changing a decision means amending it there with the new cost stated — not leaving the record describing something the code no longer does.
- One accent and one mark, defined in `app/src/styles.css` and `assets/orbit/`. Take the accent from the token; never hard-code either hex. Presence colours come from the separate palette in `app/src/names.ts`.
- Keep ordinary tests deterministic and offline.
- Use `codex/` branch names.
- Keep every PR narrow: address one specific bug, feature, or issue. Split unrelated changes into separate PRs.
- Prefer the simplest solution that meets current requirements. Avoid speculative abstractions, infrastructure, and product features; push back when proposed code or product design adds complexity without clear, present value.
- Respect the MVP non-goals: folders and hierarchy, accounts and authentication, end-to-end encryption, mobile, plugins, version-history UI, and `.md` file mirroring on disk.
- Keep `README.md` short and current; put depth under `docs/`. Update the docs when the MCP surface, the daemon lifecycle, the relay protocol, or the release steps change.
