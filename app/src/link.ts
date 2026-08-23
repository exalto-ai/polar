/**
 * Making a link.
 *
 * The `link` mark was in the schema from the start with nothing able to create
 * one. ⌘⇧K rather than ⌘K, because ⌘K already opens the document switcher and
 * silently overloading it by selection state would be a guessing game.
 */
import type { Editor } from "@tiptap/core";

export function installLinkShortcut(editor: Editor, host: HTMLElement): () => void {
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
    if (refocus) editor.commands.focus();
  }

  function open() {
    const { from, to } = editor.state.selection;
    // Anchored to the selection, so it reads as attached to the words it will
    // wrap rather than floating somewhere.
    const coords = editor.view.coordsAtPos(from);
    field.style.left = `${coords.left}px`;
    field.style.top = `${coords.bottom + 6}px`;
    field.hidden = false;
    field.value = editor.getAttributes("link").href ?? "";
    field.focus();
    field.select();
    return from !== to;
  }

  function apply() {
    const href = field.value.trim();
    const chain = editor.chain().focus();
    if (!href) {
      // An empty field removes the link rather than setting a broken one.
      chain.unsetLink().run();
    } else {
      chain.setLink({ href: normalize(href) }).run();
    }
    close(false);
  }

  const onKeyDown = (event: KeyboardEvent) => {
    if (event.metaKey && event.shiftKey && event.key.toLowerCase() === "k") {
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
  field.addEventListener("blur", () => close(false));

  return () => {
    document.removeEventListener("keydown", onKeyDown);
    field.remove();
  };
}

/** A bare domain is what people paste; without a scheme it resolves relatively. */
export function normalize(href: string): string {
  if (/^[a-z][a-z0-9+.-]*:/i.test(href)) return href;
  if (href.startsWith("//")) return `https:${href}`;
  return `https://${href}`;
}
