# Design

One ground, one accent, one mark, and the files that define them. Every surface
begins with the deep blue supplied with the current logo.
If a second colour starts carrying interface meaning, it belongs in this
document or it does not belong in the app.

## Deep blue ground and Proof Blue

| Token | Value | Use |
| --- | --- | --- |
| Ground | `#0c1622` | Window, editor, app-icon tile |
| Accent | `#6ea1ff` | Caret, focus, active formatting, links |
| Positive | `#76c392` | Connected, saved, available, and Verified state |
| Positive text | `#9bd5ae` | Positive status copy on the ground |
| Caution | `#d7a13a` | Pending, saving, incomplete, and attention state |
| Caution text | `#e3c486` | Caution status copy on the ground |
| Negative | `#d66b63` | Error, failed, destructive, and offline state |
| Negative text | `#f0aaa4` | Negative status copy on the ground |

These are defined once in [`app/src/styles.css`](app/src/styles.css). The editor intentionally uses one appearance so its ground is
always identical to the icon. Interface code should use the tokens.

The rest of the palette in that file, including `--ink`, `--ink-soft`,
`--ink-faint`, `--rule`, and `--raised`, is the neutral ramp. The greys carry a
slight blue bias so they sit with the accent rather than beside it.

The six `--status-*` variables are the non-accent semantic palette. They report
outcomes and lifecycle state whose meaning is also present in text, structure,
or an icon. They never indicate selection, focus, links, or actor identity.

### What the accent is for

The one saturated thing in a view: the caret, a focused control, a link. If two
things in the same view are blue, one of them is wrong.

Presence colours are not the accent. Actors are assigned from a separate
six-colour palette in [`app/src/names.ts`](app/src/names.ts), because a person
in the document is not a piece of interface chrome, and their colour has to stay
theirs across both themes.

## The mark

Two quotation forms face inward. They make thought and authorship literal
without adding a wordmark to small app surfaces. The icon uses the white mark
on the deep-blue tile; standalone blue and white SVG variants support light and
dark external grounds.

The supplied master files live in [`assets/orbit/`](assets/orbit/),
[`assets/macos/`](assets/macos/), and [`assets/web/`](assets/web/).
[`assets/icon-manifest.json`](assets/icon-manifest.json) carries the shared
ground into generated platform assets. See [`assets/README.md`](assets/README.md)
for the regeneration path.
