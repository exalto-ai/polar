/**
 * Making a link.
 *
 * The `link` mark was in the schema from the start with nothing able to create
 * one. ⌘⇧K rather than ⌘K, because ⌘K already opens the document switcher and
 * silently overloading it by selection state would be a guessing game.
 */
import type { Editor } from "@tiptap/core";
import { accel } from "./keys";

export type LinkController = {
  /** Open the link field for a selection or the link under the cursor. */
  open: () => boolean;
  destroy: () => void;
};

export function installLinkShortcut(editor: Editor, host: HTMLElement): LinkController {
  const field = document.createElement("input");
  field.className = "link-input";
  field.type = "text";
  field.placeholder = "Paste a link, ↵ to apply";
  field.spellcheck = false;
  field.hidden = true;
  host.append(field);

  function close(refocus = true) {
    field.hidden = true;
    field.value = "";
    field.setCustomValidity("");
    if (refocus) editor.commands.focus();
  }

  function open() {
    let { from, to } = editor.state.selection;
    if (from === to && editor.isActive("link")) {
      editor.chain().extendMarkRange("link").run();
      ({ from, to } = editor.state.selection);
    }
    if (from === to) return false;
    // Anchored to the selection, so it reads as attached to the words it will
    // wrap rather than floating somewhere.
    const coords = editor.view.coordsAtPos(from);
    field.hidden = false;
    const gap = 6;
    const edge = 8;
    const width = field.getBoundingClientRect().width || 260;
    const height = field.getBoundingClientRect().height || 34;
    const maxLeft = Math.max(edge, window.innerWidth - width - edge);
    const maxTop = Math.max(edge, window.innerHeight - height - edge);
    const below = coords.bottom + gap;
    const top = below <= maxTop ? below : coords.top - height - gap;
    field.style.left = `${Math.min(Math.max(coords.left, edge), maxLeft)}px`;
    field.style.top = `${Math.min(Math.max(top, edge), maxTop)}px`;
    field.value = editor.getAttributes("link").href ?? "";
    field.focus();
    field.select();
    return true;
  }

  function apply() {
    const href = field.value.trim();
    const chain = editor.chain().focus();
    const applied = !href
      // An empty field removes the link rather than setting a broken one.
      ? chain.unsetLink().run()
      : chain.setLink({ href: normalize(href) }).run();
    if (!applied) {
      field.setCustomValidity("Enter a valid link");
      field.reportValidity();
      field.focus();
      return;
    }
    field.setCustomValidity("");
    close(false);
  }

  const onKeyDown = (event: KeyboardEvent) => {
    const active = document.activeElement;
    const editorHasFocus =
      active === editor.view.dom || (!!active && editor.view.dom.contains(active));
    if (
      editorHasFocus &&
      accel(event) &&
      event.shiftKey &&
      event.key.toLowerCase() === "k"
    ) {
      event.preventDefault();
      const hasSelection = open();
      if (!hasSelection) {
        // Nothing to wrap; a link needs text.
        close();
      }
    }
  };

  const onFieldKey = (event: KeyboardEvent) => {
    event.stopPropagation();
    if (event.key === "Enter") {
      event.preventDefault();
      apply();
    } else if (event.key === "Escape") {
      event.preventDefault();
      close();
    }
  };

  document.addEventListener("keydown", onKeyDown);
  field.addEventListener("keydown", onFieldKey);
  field.addEventListener("input", () => field.setCustomValidity(""));
  field.addEventListener("blur", () => close(false));

  return {
    open,
    destroy: () => {
      document.removeEventListener("keydown", onKeyDown);
      field.remove();
    },
  };
}

/** A bare domain is what people paste; without a scheme it resolves relatively. */
export function normalize(href: string): string {
  if (href.startsWith("//")) return `https:${href}`;
  if (
    href.startsWith("/") ||
    href.startsWith("./") ||
    href.startsWith("../") ||
    href.startsWith("#") ||
    href.startsWith("?")
  ) {
    return href;
  }
  // A hostname followed by a port looks like a URI scheme to a generic scheme
  // regex. Recognize that common local-development shape first.
  if (/^(?:localhost|[a-z0-9.-]+):\d+(?:[/?#]|$)/i.test(href)) return `https://${href}`;
  if (/^[a-z][a-z0-9+.-]*:/i.test(href)) return href;
  return `https://${href}`;
}
