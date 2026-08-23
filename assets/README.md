# assets

`icon.png` is the source for the app icon; the sized variants under
`app/src-tauri/icons` are generated from it:

```bash
python3 scripts/make-icon.py
npx tauri icon ../assets/icon.png   # from app/
```

The mark is Orbit, coin cut: a disc with a ring and one radial arm knocked out
of it. It is a point in polar coordinates — the document at the centre, and
every client a different bearing on it. Because the mark is cut out rather than
drawn on, the tile shows through the ring and the arm.

The tile is Polar Blue and the disc is white, which is the reversed cut in
`orbit/`. It is drawn by `scripts/make-icon.py` on the same 96 × 96 field as
those SVGs, scaled up, so the icon and the vectors cannot drift apart. See
[`../DESIGN.md`](../DESIGN.md).

`orbit/` holds the four cuts of the mark for use outside the app — one per
ground, all the same geometry.
