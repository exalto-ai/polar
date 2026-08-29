#!/usr/bin/env bash
# Build the daemon and the stdio shim, and stage them where Tauri expects
# sidecars: `binaries/<name>-<target-triple>`.
#
# The window cannot do anything without the daemon, so a bundle that omits it
# is not a working app. Tauri strips the triple when it copies them next to the
# app executable, which is where find_binary looks.
set -euo pipefail

TARGET="${1:-$(rustc -vV | sed -n 's/^host: //p')}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$ROOT/app/src-tauri/binaries"

echo "staging sidecars for $TARGET"
mkdir -p "$DEST"

sign_macos_sidecar() {
  local path="$1"
  local name="$2"
  local identifier
  case "$name" in
    thoughtd) identifier="ai.exalto.thought.thoughtd" ;;
    thought-mcp-stdio) identifier="ai.exalto.thought.thought-mcp-stdio" ;;
    *)
      echo "unknown Proof of Thought sidecar: $name" >&2
      return 1
      ;;
  esac

  # Rust's linker signature uses a content-derived identifier. Replace it so
  # unsigned local bundles still expose the same named helper identities that
  # a Developer ID release gets from the embedded Info.plist.
  codesign --force --sign - --identifier "$identifier" "$path"
}

if [[ "$TARGET" == universal-apple-darwin ]]; then
  # Tauri builds a universal app by compiling each architecture separately and
  # lipo-ing the result, and during each of those passes it looks for a sidecar
  # named for *that* architecture — not the universal one. So stage all three:
  # both per-arch binaries, and the lipo'd universal for the bundling step.
  for arch in aarch64-apple-darwin x86_64-apple-darwin; do
    rustup target add "$arch" >/dev/null
    cargo build --release --locked --target "$arch" -p thoughtd \
      --bin thoughtd --bin thought-mcp-stdio
    for name in thoughtd thought-mcp-stdio; do
      cp "$ROOT/target/$arch/release/$name" "$DEST/$name-$arch"
      sign_macos_sidecar "$DEST/$name-$arch" "$name"
    done
  done
  for name in thoughtd thought-mcp-stdio; do
    lipo -create -output "$DEST/$name-$TARGET" \
      "$DEST/$name-aarch64-apple-darwin" \
      "$DEST/$name-x86_64-apple-darwin"
    sign_macos_sidecar "$DEST/$name-$TARGET" "$name"
  done
else
  rustup target add "$TARGET" >/dev/null 2>&1 || true
  cargo build --release --locked --target "$TARGET" -p thoughtd \
    --bin thoughtd --bin thought-mcp-stdio
  for name in thoughtd thought-mcp-stdio; do
    cp "$ROOT/target/$TARGET/release/$name" "$DEST/$name-$TARGET"
    if [[ "$TARGET" == *-apple-darwin ]]; then
      sign_macos_sidecar "$DEST/$name-$TARGET" "$name"
    fi
  done
fi

chmod +x "$DEST"/*
ls -la "$DEST"
