# yrs-check

Twelve lines answering the question M1.3 rests on: does a `yrs` block carry a stable,
replica-independent identity we can hand to agents as `block_id`?

```bash
cargo +1.95.0 run    # 1.94.1 cannot build yrs 0.27.4
```

Yes. `Branch::id()` via `AsRef<Branch>` — not `element.id()`, which does not exist — is
stable across edits to the block's contents **and identical on a second replica after
sync**. The second property is the load-bearing one; a replica-local id would have made
the whole anchor design silently wrong.
