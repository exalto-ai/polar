# Releasing

`.github/workflows/release.yml` builds a universal macOS `.dmg` and attaches it
to a **draft** release, so a build can be opened and checked before anyone gets
it.

```bash
git tag v0.1.0 && git push origin v0.1.0
```

Or run the workflow manually from the Actions tab with a tag name.

## What ships inside the bundle

The window is useless without the daemon, so `polard` and `polar-mcp-stdio` are
built for both architectures, stitched with `lipo`, and declared as Tauri
sidecars. Tauri copies them next to the app executable and strips the target
triple, which is where the app looks for them. `scripts/stage-sidecars.sh` does
the building. A bundle without them launches a window that can never reach a
document, which is the failure a release is most likely to have.

## Signing and notarization

**The workflow builds fine without any of this.** An unsigned `.dmg` runs, but
Gatekeeper warns on first open and the user has to right-click → Open. Fine for
early testing, not fine for anyone else.

To sign, add these repository secrets. Set them yourself — key material should
go from your vault into GitHub without passing through anything in between:

| Secret | What it is |
| --- | --- |
| `APPLE_CERTIFICATE` | Your **Developer ID Application** `.p12`, base64-encoded |
| `APPLE_CERTIFICATE_PASSWORD` | The password set when exporting that `.p12` |
| `APPLE_SIGNING_IDENTITY` | e.g. `Developer ID Application: Your Name (TEAMID)` |
| `APPLE_ID` | The Apple ID used for notarization |
| `APPLE_PASSWORD` | An **app-specific password**, not your Apple ID password |
| `APPLE_TEAM_ID` | The ten-character team id |

Getting the certificate out of Keychain Access: find *Developer ID Application*
under **My Certificates**, right-click → Export as `.p12`, then

```bash
base64 -i Certificates.p12 | pbcopy
```

Add each with the GitHub CLI, which prompts for the value rather than leaving it
in shell history:

```bash
gh secret set APPLE_CERTIFICATE
```

Repeat for each name in the table; `gh secret list` confirms what is set.

An app-specific password comes from appleid.apple.com → Sign-In and Security →
App-Specific Passwords. Notarization rejects a plain Apple ID password.

## Checking a build locally

```bash
./scripts/stage-sidecars.sh
cd app && npx tauri build --bundles app
```

Then confirm the daemon actually shipped:

```bash
ls app/src-tauri/target/*/release/bundle/macos/Polar.app/Contents/MacOS/
```

`polard` and `polar-mcp-stdio` should both be there beside `app`.
