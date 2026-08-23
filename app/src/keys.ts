/**
 * The primary shortcut modifier.
 *
 * ⌘ on macOS, Ctrl everywhere else. Under WebKitGTK the Super key never
 * reaches the webview as `metaKey`, so a window bound only to ⌘ cannot open
 * the switcher — and the switcher is the only way to make a document.
 */
export const ACCEL_IS_META = /Mac|iPhone|iPad/.test(navigator.userAgent);

export const accel = (event: KeyboardEvent): boolean =>
  ACCEL_IS_META ? event.metaKey : event.ctrlKey;

/** What to call the modifier in text the user reads. */
export const ACCEL_LABEL = ACCEL_IS_META ? "⌘" : "Ctrl-";

/**
 * Rewrite the `⌘` baked into the markup for platforms that do not have one.
 * The hints are the only place the binding is ever spelled out, so a stale
 * glyph there is a wrong instruction rather than a cosmetic slip.
 */
export function relabelShortcutHints(root: ParentNode = document): void {
  if (ACCEL_IS_META) return;
  for (const key of root.querySelectorAll("kbd")) {
    if (key.textContent) key.textContent = key.textContent.replace("⌘", ACCEL_LABEL);
  }
}
