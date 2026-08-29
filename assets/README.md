# assets

The paired quotation mark is the current Proof of Thought identity. The
authoritative source package is checked in here:

- `avatar-1024.png` is the full-field avatar.
- `macos/` contains the transparent macOS icon sizes and supplied ICNS.
- `orbit/` contains the tile and light/dark mark variants.
- `web/` contains browser and installable-web-app icons.

`icon.png` is an exact copy of `macos/AppIcon-1024.png` and drives the
cross-platform Tauri outputs. Regenerate them from `app/` with:

```bash
npx tauri icon ../assets/icon-manifest.json
cp ../assets/macos/PoT-AppIcon.icns src-tauri/icons/icon.icns
```

The manifest pins adaptive backgrounds to the logo's deep blue (`#0c1622`),
which is also the editor ground. The web files used by Vite are mirrored into
`app/public/`.
