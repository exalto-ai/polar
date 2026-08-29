/**
 * Creating and working with links.
 *
 * A linked word is still editable text. Clicking it therefore opens a compact
 * action card instead of navigating immediately. Opening the destination is a
 * deliberate action alongside copy, edit, and remove.
 *
 * ⌘⇧K rather than ⌘K creates or edits a link because ⌘K already opens the
 * document switcher.
 */
import type { Editor } from "@tiptap/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { ICONS, icon, type IconNode } from "./icons";
import { accel } from "./keys";

export type LinkController = {
  /** Open the link field for a selection or the link under the cursor. */
  open: () => boolean;
  destroy: () => void;
};

type ActiveLink = { href: string; text: string };

function actionButton(label: string, nodes: readonly IconNode[]): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = "link-card-action";
  button.setAttribute("aria-label", label);
  button.title = label;
  button.append(icon(nodes));
  return button;
}

/** Keep a fixed menu attached to its source and inside the visible window. */
function placeFloating(
  element: HTMLElement,
  source: Pick<DOMRect, "top" | "bottom" | "left">,
  fallback: { width: number; height: number },
) {
  const gap = 8;
  const edge = 8;
  const bounds = element.getBoundingClientRect();
  const width = bounds.width || fallback.width;
  const height = bounds.height || fallback.height;
  const viewportWidth = window.innerWidth || document.documentElement.clientWidth;
  const viewportHeight = window.innerHeight || document.documentElement.clientHeight;
  const maxLeft = Math.max(edge, viewportWidth - width - edge);
  const maxTop = Math.max(edge, viewportHeight - height - edge);
  const below = source.bottom + gap;
  const top = below <= maxTop ? below : source.top - height - gap;
  element.style.left = `${Math.min(Math.max(source.left, edge), maxLeft)}px`;
  element.style.top = `${Math.min(Math.max(top, edge), maxTop)}px`;
}

function readableHref(href: string): string {
  if (!/^https?:\/\//i.test(href)) return href;
  const withoutScheme = href.replace(/^https?:\/\//i, "");
  return withoutScheme.length > 1 ? withoutScheme.replace(/\/$/, "") : withoutScheme;
}

async function copyText(text: string): Promise<void> {
  if (navigator.clipboard?.writeText) {
    await navigator.clipboard.writeText(text);
    return;
  }

  // Older WKWebView versions do not expose navigator.clipboard. Keep the copy
  // action functional there without leaving a visible temporary control.
  const proxy = document.createElement("textarea");
  proxy.value = text;
  proxy.setAttribute("readonly", "");
  proxy.style.position = "fixed";
  proxy.style.opacity = "0";
  document.body.append(proxy);
  proxy.select();
  const copied = document.execCommand?.("copy") ?? false;
  proxy.remove();
  if (!copied) throw new Error("copy is unavailable");
}

async function openDestination(href: string): Promise<void> {
  const tauri = Boolean(
    (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__,
  );
  if (tauri) {
    await openUrl(href);
    return;
  }
  window.open(href, "_blank", "noopener,noreferrer");
}

export function installLinkShortcut(editor: Editor, host: HTMLElement): LinkController {
  const editorDom = editor.view.dom;
  const field = document.createElement("input");
  field.className = "link-input";
  field.type = "text";
  field.placeholder = "Paste a link, ↵ to apply";
  field.setAttribute("aria-label", "Link destination");
  field.spellcheck = false;
  field.hidden = true;

  const card = document.createElement("div");
  card.className = "link-card";
  card.hidden = true;
  card.setAttribute("role", "dialog");
  card.setAttribute("aria-label", "Link options");

  const openButton = document.createElement("button");
  openButton.type = "button";
  openButton.className = "link-card-open";
  openButton.append(icon(ICONS.globe));

  const destination = document.createElement("span");
  destination.className = "link-card-destination";
  const title = document.createElement("span");
  title.className = "link-card-title";
  const url = document.createElement("span");
  url.className = "link-card-url";
  destination.append(title, url);
  openButton.append(destination);

  const actions = document.createElement("span");
  actions.className = "link-card-actions";
  const copy = actionButton("Copy link", ICONS.copy);
  const edit = actionButton("Edit link", ICONS.pencil);
  const unlink = actionButton("Remove link", ICONS.link2Off);
  actions.append(copy, edit, unlink);
  card.append(openButton, actions);
  host.append(field, card);

  let activeLink: ActiveLink | undefined;
  let copiedTimer: number | undefined;
  let openFeedbackTimer: number | undefined;
  let destroyed = false;

  function resetOpenButton() {
    if (openFeedbackTimer !== undefined) window.clearTimeout(openFeedbackTimer);
    openFeedbackTimer = undefined;
    openButton.classList.remove("is-error");
    delete destination.dataset.feedback;
  }

  function anchorAtSelection(): HTMLAnchorElement | null {
    if (!editor.isActive("link")) return null;
    editor.commands.extendMarkRange("link");
    const { from, to } = editor.state.selection;
    const inside = from + Math.floor((to - from) / 2);
    const { node, offset } = editor.view.domAtPos(inside);
    const candidate =
      node.nodeType === Node.TEXT_NODE
        ? node.parentElement
        : node.childNodes[offset] ?? node;
    if (!candidate) return null;
    const element =
      candidate instanceof Element ? candidate : candidate.parentElement;
    const anchor = element?.closest<HTMLAnchorElement>("a") ?? null;
    return anchor && editorDom.contains(anchor) ? anchor : null;
  }

  function resetCopyButton() {
    if (copiedTimer !== undefined) window.clearTimeout(copiedTimer);
    copiedTimer = undefined;
    copy.classList.remove("is-copied");
    copy.classList.remove("is-error");
    delete copy.dataset.feedback;
    copy.setAttribute("aria-label", "Copy link");
    copy.title = "Copy link";
  }

  function closeCard(refocus = false) {
    const cardHadFocus = card.contains(document.activeElement);
    card.hidden = true;
    activeLink = undefined;
    resetOpenButton();
    resetCopyButton();
    if (refocus) {
      editor.commands.focus();
      editor.view.focus();
    } else if (cardHadFocus) {
      queueMicrotask(() => {
        const active = document.activeElement;
        if (card.hidden && (!active || active === document.body || card.contains(active))) {
          editor.commands.focus();
          editor.view.focus();
        }
      });
    }
  }

  function closeField(refocus = true) {
    field.hidden = true;
    field.value = "";
    field.setCustomValidity("");
    if (refocus) editor.commands.focus();
  }

  function restoreLinkSelection(): boolean {
    // ProseMirror keeps this selection mapped through collaborative edits while
    // focus is in the card. Reusing it is safer than caching numeric positions.
    if (!activeLink || !editor.isActive("link")) return false;
    return editor.commands.extendMarkRange("link");
  }

  function openField() {
    let { from, to } = editor.state.selection;
    if (from === to && editor.isActive("link")) {
      editor.chain().extendMarkRange("link").run();
      ({ from, to } = editor.state.selection);
    }
    if (from === to) return false;

    closeCard(false);
    const coords = editor.view.coordsAtPos(from);
    field.hidden = false;
    placeFloating(field, coords, { width: 260, height: 34 });
    field.value = editor.getAttributes("link").href ?? "";
    field.focus();
    field.select();
    return true;
  }

  function showCard(anchor: HTMLAnchorElement) {
    closeField(false);

    const href = anchor.getAttribute("href") ?? editor.getAttributes("link").href;
    if (!href) return;
    const text = anchor.textContent?.trim() ?? "";
    activeLink = { href, text };
    resetOpenButton();

    title.textContent = text || readableHref(href);
    url.textContent = readableHref(href);
    openButton.setAttribute("aria-label", `Open link ${href}`);
    openButton.title = `Open ${href}`;
    card.hidden = false;
    placeFloating(card, anchor.getBoundingClientRect(), { width: 420, height: 72 });
    openButton.focus({ preventScroll: true });
  }

  function apply() {
    const href = field.value.trim();
    const applied = !href
      // An empty field removes the link rather than setting a broken one.
      ? editor.commands.unsetLink()
      : editor.commands.setLink({ href: normalize(href) });
    if (!applied) {
      field.setCustomValidity("Enter a valid link");
      field.reportValidity();
      field.focus();
      return;
    }
    field.setCustomValidity("");
    closeField(false);
    editor.commands.focus();
  }

  const onEditorClick = (event: MouseEvent) => {
    if (destroyed) return;
    if (event.button !== 0 || !(event.target instanceof Element)) return;
    const anchor = event.target.closest<HTMLAnchorElement>("a");
    if (!anchor || !editorDom.contains(anchor)) return;
    event.preventDefault();
    event.stopPropagation();
    showCard(anchor);
  };

  const onKeyDown = (event: KeyboardEvent) => {
    if (destroyed) return;
    if (!card.hidden && event.key === "Escape") {
      event.preventDefault();
      event.stopPropagation();
      closeCard(true);
      return;
    }

    const active = document.activeElement;
    const editorHasFocus =
      active === editorDom || (!!active && editorDom.contains(active));
    if (
      editorHasFocus &&
      event.altKey &&
      !accel(event) &&
      !event.shiftKey &&
      event.key === "Enter"
    ) {
      const anchor = anchorAtSelection();
      if (anchor) {
        event.preventDefault();
        event.stopPropagation();
        showCard(anchor);
      }
      return;
    }
    if (
      editorHasFocus &&
      accel(event) &&
      event.shiftKey &&
      event.key.toLowerCase() === "k"
    ) {
      event.preventDefault();
      const hasSelection = openField();
      if (!hasSelection) {
        // Nothing to wrap; a link needs text.
        closeField();
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
      closeField();
    }
  };

  const onDocumentMouseDown = (event: MouseEvent) => {
    if (!(event.target instanceof Node)) return;
    if (!card.hidden && !card.contains(event.target)) closeCard(false);
    if (!field.hidden && event.target !== field) closeField(false);
  };

  const onViewportChange = () => {
    closeCard(false);
    closeField(false);
  };

  openButton.addEventListener("click", () => {
    if (!activeLink) return;
    const href = activeLink.href;
    void openDestination(href)
      .then(() => closeCard(true))
      .catch(() => {
        openButton.classList.add("is-error");
        destination.dataset.feedback = "Could not open link";
        openButton.setAttribute("aria-label", `Could not open link ${href}`);
        openButton.title = `Could not open ${href}`;
        openFeedbackTimer = window.setTimeout(() => {
          if (!activeLink) return;
          resetOpenButton();
          openButton.setAttribute("aria-label", `Open link ${activeLink.href}`);
          openButton.title = `Open ${activeLink.href}`;
        }, 2200);
      });
  });
  copy.addEventListener("click", () => {
    if (!activeLink) return;
    void copyText(activeLink.href)
      .then(() => {
        copy.classList.add("is-copied");
        copy.dataset.feedback = "Copied";
        copy.setAttribute("aria-label", "Copied");
        copy.title = "Copied";
        copiedTimer = window.setTimeout(resetCopyButton, 1200);
      })
      .catch(() => {
        copy.classList.add("is-error");
        copy.dataset.feedback = "Copy failed";
        copy.setAttribute("aria-label", "Could not copy link");
        copy.title = "Could not copy link";
        copiedTimer = window.setTimeout(resetCopyButton, 1800);
      });
  });
  edit.addEventListener("click", () => {
    if (!activeLink) return;
    if (!restoreLinkSelection()) return;
    closeCard(false);
    openField();
  });
  unlink.addEventListener("click", () => {
    if (!restoreLinkSelection()) return;
    editor.chain().focus().unsetLink().run();
    closeCard(false);
  });

  document.addEventListener("keydown", onKeyDown);
  document.addEventListener("mousedown", onDocumentMouseDown);
  document.addEventListener("scroll", onViewportChange, true);
  window.addEventListener("resize", onViewportChange);
  editorDom.addEventListener("click", onEditorClick);
  field.addEventListener("keydown", onFieldKey);
  field.addEventListener("input", () => field.setCustomValidity(""));
  field.addEventListener("blur", () => closeField(false));

  const destroy = () => {
    if (destroyed) return;
    destroyed = true;
    editor.off("destroy", destroy);
    document.removeEventListener("keydown", onKeyDown);
    document.removeEventListener("mousedown", onDocumentMouseDown);
    document.removeEventListener("scroll", onViewportChange, true);
    window.removeEventListener("resize", onViewportChange);
    editorDom.removeEventListener("click", onEditorClick);
    if (copiedTimer !== undefined) window.clearTimeout(copiedTimer);
    if (openFeedbackTimer !== undefined) window.clearTimeout(openFeedbackTimer);
    field.remove();
    card.remove();
  };
  editor.on("destroy", destroy);

  return { open: openField, destroy };
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
  if (/^(?:localhost|(?:[a-z0-9-]+\.)+[a-z0-9-]+):\d+(?:[/?#]|$)/i.test(href)) {
    return `https://${href}`;
  }
  if (/^[a-z][a-z0-9+.-]*:/i.test(href)) return href;
  return `https://${href}`;
}
