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

if [[ "$TARGET" == universal-apple-darwin ]]; then
  # A universal binary is built per-arch and stitched together with lipo;
  # cargo cannot emit one directly.
  for arch in aarch64-apple-darwin x86_64-apple-darwin; do
    rustup target add "$arch" >/dev/null
    cargo build --release --target "$arch" -p polard
  done
  for name in polard polar-mcp-stdio; do
    lipo -create -output "$DEST/$name-$TARGET" \
      "$ROOT/target/aarch64-apple-darwin/release/$name" \
      "$ROOT/target/x86_64-apple-darwin/release/$name"
  done
else
  rustup target add "$TARGET" >/dev/null 2>&1 || true
  cargo build --release --target "$TARGET" -p polard
  for name in polard polar-mcp-stdio; do
    cp "$ROOT/target/$TARGET/release/$name" "$DEST/$name-$TARGET"
  done
fi

chmod +x "$DEST"/*
ls -la "$DEST"
