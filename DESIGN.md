# Design

One accent, one mark, and the two files that define them. Everything else in the
interface is neutral on purpose — if a second colour starts carrying meaning,
it belongs in this document or it does not belong in the app.

## Proof Blue

| | Light ground | Dark ground |
| --- | --- | --- |
| Accent | `#2f6fed` | `#6ea1ff` |
| Contrast against its ground | 4.55:1 on `#ffffff` | 6.94:1 on `#16181c` |

It is two values, not one tinted two ways. The light value on a dark ground
measures 3.91:1 and the dark value on white measures 2.56:1 — both legible in a
specimen and both muddy in use, which is why the pair exists.

Defined once, in [`app/src/styles.css`](app/src/styles.css) as `--accent`, and
rebound under `prefers-color-scheme`. Nothing should hard-code either hex; take
it from the token.

The rest of the palette in that file — `--paper`, `--ink`, `--ink-soft`,
`--ink-faint`, `--rule`, `--raised` — is the neutral ramp. The greys carry a
slight blue bias so they sit with the accent rather than beside it.

### What the accent is for

The one saturated thing in a view: the caret, a focused control, a link. If two
things in the same view are blue, one of them is wrong.

Presence colours are not the accent. Actors are assigned from a separate
six-colour palette in [`app/src/names.ts`](app/src/names.ts), because a person
in the document is not a piece of interface chrome, and their colour has to stay
theirs across both themes.

## The mark

Orbit, coin cut: a filled disc with a ring and one radial arm knocked out of it,
so whatever is behind shows through the mark. Polar coordinates — the document
is the centre, and the editor, an agent over MCP and the relay are the same
document seen from different bearings. The engine is named for them.

The arm sits at 45°, on the diagonal of the icon's square. Below that it reads
as a clock hand; above it, it flattens and loses the climb.

Four cuts live in [`assets/orbit/`](assets/orbit):

| File | Ground |
| --- | --- |
| `light.svg` | light |
| `dark.svg` | dark, including the app tile |
| `mono.svg` | one colour, inherited — the macOS template image |
| `reversed.svg` | Proof Blue or any saturated field |

All four are the same geometry on a 96 × 96 field. The app tile is the reversed
cut — a white disc on Proof Blue, so the brand colour owns the whole tile rather
than a shape inside it, which is all a Dock shows at a glance. Its gradient is
the accent lifted at the top and deepened at the bottom, both derived from the
one hex, so the icon cannot disagree with this file.

The tile is drawn by [`scripts/make-icon.py`](scripts/make-icon.py) from the
same coordinates, scaled up, so the icon and the vectors cannot drift; see
[`assets/README.md`](assets/README.md) for how the sized icons are regenerated.

A filled silhouette is the point. Outline marks are pleasant at 1024 px and
become lint at 32, and there is nothing in a solid disc left to thin out.
