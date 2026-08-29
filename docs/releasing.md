# Releasing

`.github/workflows/release.yml` builds a universal macOS `.dmg` and attaches it
to a **draft** release, so a build can be opened and checked before anyone gets
it.

```bash
git tag v0.1.0 && git push origin v0.1.0
```

Or run the workflow manually from the Actions tab with a tag name.

## What ships inside the bundle

The window is useless without the daemon, so `thoughtd` and `thought-mcp-stdio` are
built for both architectures, stitched with `lipo`, and declared as Tauri
sidecars. Tauri copies them next to the app executable and strips the target
triple, which is where the app looks for them. `scripts/stage-sidecars.sh` does
the building. A bundle without them launches a window that can never reach a
document, which is the failure a release is most likely to have.

## Signing and notarization

**The workflow builds fine without any of this.** An unsigned `.dmg` runs, but
Gatekeeper refuses it on download and the user has to go into System Settings to
allow it. Fine for testing, not fine for anyone else.

Signing uses a Developer ID certificate; notarization uses an **App Store
Connect API key** rather than an Apple ID and app-specific password. Three
values, no second factor, and nothing that breaks when someone changes their
Apple ID password.

| Secret | What it is |
| --- | --- |
| `APPLE_CERTIFICATE` | Developer ID Application `.p12`, base64-encoded |
| `APPLE_CERTIFICATE_PASSWORD` | The `.p12` export password |
| `APPLE_SIGNING_IDENTITY` | `Developer ID Application: Exalto, Inc. (3FGNZ9DY9Y)` |
| `APPLE_TEAM_ID` | `3FGNZ9DY9Y` |
| `APPLE_NOTARIZATION_KEY_BASE64` | The App Store Connect `.p8`, base64-encoded |
| `APPLE_NOTARIZATION_KEY_ID` | The key's ID |
| `APPLE_NOTARIZATION_ISSUER_ID` | The issuer ID from App Store Connect |

Both live in 1Password, and a notarization key is per **team**, not per app — the
Exalto key notarizes anything signed by the Exalto Developer ID, so nothing new
had to be created for Proof of Thought:

- `Exalto - Apple Signing` → *Apple Developer ID - Exalto (3FGNZ9DY9Y)*
- `Exalto - LLM Notary` → *App Store Connect API - LLM Notary Notarization*

Set them straight from the vault, so the values never land in a file, a
clipboard, or shell history:

```bash
op item get "Apple Developer ID - Exalto (3FGNZ9DY9Y)" --vault "Exalto - Apple Signing" \
  --fields password --reveal | gh secret set APPLE_CERTIFICATE_PASSWORD
```

```bash
op item get "App Store Connect API - LLM Notary Notarization (2RTKQ2H2FW)" \
  --vault "Exalto - LLM Notary" --fields credential --reveal \
  | base64 | gh secret set APPLE_NOTARIZATION_KEY_BASE64
```

The `credential` field holds the raw PEM, so it is base64-encoded on the way
past; the workflow decodes it back to a `.p8` and checks it with `openssl`
before building.

Use `--format json` rather than `--fields`: `op item get --fields` CSV-quotes
any multi-line value, so the key arrives wrapped in double quotes, decodes to
something that is not a PEM, and the build fails eight minutes later with
`invalidPEMDocument` from inside the bundler:

```bash
op item get "App Store Connect API - LLM Notary Notarization (2RTKQ2H2FW)" \
  --vault "Exalto - LLM Notary" --format json --reveal \
  | python3 -c "import json,sys; print(next(f['value'] for f in json.load(sys.stdin)['fields'] if f.get('label')=='credential'), end='')" \
  | base64 | gh secret set APPLE_NOTARIZATION_KEY_BASE64
```

### Why the DMG is notarized twice

Tauri notarizes and staples the **application**, then wraps it in a disk image.
The DMG is a separately distributed container and needs its own ticket, or
Gatekeeper rejects the download even though the app inside it is fine. The
workflow submits the finished DMG to `notarytool` and staples it as a distinct
step.

## Checking a build locally

```bash
./scripts/stage-sidecars.sh
(
  cd app
  npm exec -- tauri build --bundles app,dmg \
    --config '{"bundle":{"macOS":{"signingIdentity":"-"}}}'
)
```

The explicit `-` identity makes Tauri apply one complete ad hoc signature across the local app
bundle and both helpers. Omitting it leaves only linker signatures, which is not equivalent to the
packaged test boundary below.

Then confirm the daemon actually shipped:

```bash
ls app/src-tauri/target/release/bundle/macos/'Proof of Thought.app'/Contents/MacOS/
```

`thought`, `thoughtd`, and `thought-mcp-stdio` should all be present. `thought` is the
window executable; the other two are its sidecars.

Verify the complete local bundle before opening the DMG:

```bash
local_app="app/src-tauri/target/release/bundle/macos/Proof of Thought.app"
local_dmg="$(find app/src-tauri/target/release/bundle/dmg -maxdepth 1 -type f -name '*.dmg' -print -quit)"
test -n "$local_dmg"
codesign --verify --deep --strict --verbose=2 "$local_app"
hdiutil verify "$local_dmg"
```

## Planned credential upgrade checks

This release-preflight layer only stages and verifies the complete local bundle. Reviewer-capability
probes, stable helper identities, and signed-update Keychain continuity arrive in their owning
reviewer and release-hardening layers later in the stack. The paragraphs below record those future
acceptance requirements; they are not claims about this intermediate branch.

The same PID, listener-owner, and exact-executable proof must pass before the app or native reviewer
launcher sends the editor capability. Exercise the negative probe test in the release build so a
lookalike loopback service receives only the public identity request, never an Authorization header.

Developer ID releases give `thoughtd` and `thought-mcp-stdio` stable signed identities. The release
workflow verifies those identities, their common Apple team, their designated requirements, and
both CPU architectures before accepting the DMG. This is what lets both helpers share a reviewer
credential and keep that Keychain access across signed app updates.

Unsigned test builds are ad hoc signed only so the two helpers in that exact build can share a
Keychain item. Their designated requirements contain a build-specific code hash, so replacing an
unsigned build can require the tester to reset its reviewer connections. An unsigned DMG must not
be presented as testing the signed-update Keychain guarantee.
