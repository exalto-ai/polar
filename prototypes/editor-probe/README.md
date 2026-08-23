# Editor probe

Exists to answer one question before any of Proof of Thought gets built: **can WKWebView host a
collaborative ProseMirror well enough to ship a writing app on?** (AD-8 in
`docs/architecture.md`.) It is a risk probe, not a foundation — none of this code is
meant to survive into the app.

## Run

```bash
npm run dev          # Chromium baseline, http://localhost:1420
npm run tauri dev    # the real target — WKWebView
```

Append `?autotest=1` to run the automated passes on load. `src-tauri/tauri.conf.json`
already points `devUrl` there, so the Tauri build self-tests on launch and prints its
verdict to stdout over IPC — results can be read without screenshotting a desktop.
In the browser, the **Self-test** button does the same thing.

## What it sets up

Two `Y.Doc` replicas bound to two TipTap editors, joined by a fake link with adjustable
latency and an offline queue. An "agent" fires block-level replacements and insertions at
the peer replica — the same shape the MCP tool surface will emit — so human typing and
agent restructuring collide the way they will in production.

## What is measured

Automated (`Self-test`):

| Check | Why it matters |
|---|---|
| Converges under 120 concurrent agent ops | The core CRDT claim |
| `doc.check()` passes on both replicas | Convergence to *identical garbage* is still a failure |
| Offline divergence reconciles | The local-first claim |
| Caret survives remote updates | Where WKWebView is most likely to hurt |
| Markdown input rules fire | The Bear-feel affordance (AD-3) |

Manual, in the checklist pane — **IME is the one that can actually kill the stack.**
Composition cannot be faithfully synthesised, so it needs a human with a Japanese or
Pinyin input source: start a long composition, fire an agent burst mid-composition, and
watch whether the candidate window survives and the buffer commits intact.

## Results — 2026-08-22

| Check | Chromium | WKWebView |
|---|---|---|
| Converges under 60 concurrent agent ops | pass (~1s) | pass (~1s) |
| `doc.check()` on both replicas | pass | pass |
| Offline divergence reconciles | pass (~1s) | pass (~1s) |
| Caret survives remote updates | pass | pass |
| Markdown input rules fire | pass | pass |

**No behavioural difference between the engines.** Every failure seen while building this
was a flaw in the test, not in WKWebView — most stubbornly, asserting the input rule on
`doc.lastChild` when TipTap keeps a trailing empty paragraph, so the converted heading was
never the last node.

IME is **not** covered and is the one result that matters most. See below.

## Reading the result

A green self-test in Chromium proves the *design* is sound. Only a green self-test **plus
a clean manual IME pass in WKWebView** clears AD-8. If IME breaks there and cannot be
worked around, the fallback is native AppKit over the same Rust core, which costs months.
