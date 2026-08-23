# assets

`icon.png` is the source for the app icon; the sized variants under
`app/src-tauri/icons` are generated from it:

```bash
python3 scripts/make-icon.py
npx tauri icon ../assets/icon.png   # from app/
```

The mark is the caret the editor draws — thin bar, round cap — sitting in a gap
in a paragraph. The product is a cursor several people and several agents
share, so the icon is that cursor rather than a page or a pen. It is drawn by
`scripts/make-icon.py`, at a size where the caret survives being shrunk to 32px:
it is the only saturated shape, and the text lines are quiet enough to read as
texture rather than competing with it.
