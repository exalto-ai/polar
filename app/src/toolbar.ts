import type { Editor } from "@tiptap/core";
import { ICONS, icon } from "./icons";

export const ZOOM_LEVELS = [75, 90, 100, 110, 125, 150, 175, 200] as const;
export const FONT_SIZES = [12, 14, 16, 17, 18, 20, 24, 28, 32, 40, 48, 56, 64] as const;

export type BlockStyle = "normal" | "title" | "h1" | "h2" | "h3";
export type BlockStyleState = BlockStyle | "mixed";
export type FontSizeState = string | "mixed";

const ZOOM_KEY = "thought.zoom";

function option(value: string, label: string): HTMLOptionElement {
  const item = document.createElement("option");
  item.value = value;
  item.textContent = label;
  return item;
}

function selectControl(label: string, className: string): HTMLSelectElement {
  const select = document.createElement("select");
  select.className = `format-select ${className}`;
  select.setAttribute("aria-label", label);
  select.title = label;
  return select;
}

function buttonControl(
  label: string,
  content: string | Node,
  className = "",
): HTMLButtonElement {
  const button = document.createElement("button");
  button.type = "button";
  button.className = `format-button ${className}`.trim();
  button.setAttribute("aria-label", label);
  button.setAttribute("aria-pressed", "false");
  button.title = label;
  if (typeof content === "string") button.textContent = content;
  else button.append(content);
  // A toolbar press must not collapse the editor selection before the command
  // gets a chance to act on it.
  button.addEventListener("mousedown", (event) => event.preventDefault());
  return button;
}

function divider(): HTMLSpanElement {
  const line = document.createElement("span");
  line.className = "format-divider";
  line.setAttribute("aria-hidden", "true");
  return line;
}

export function safeZoom(value: string | null): (typeof ZOOM_LEVELS)[number] {
  const parsed = Number(value);
  return ZOOM_LEVELS.includes(parsed as (typeof ZOOM_LEVELS)[number])
    ? (parsed as (typeof ZOOM_LEVELS)[number])
    : 100;
}

export function currentBlockStyle(editor: Editor): BlockStyle {
  if (!editor.isActive("heading")) return "normal";
  const attrs = editor.getAttributes("heading") as { level?: number; variant?: string | null };
  if (attrs.variant === "title") return "title";
  if (attrs.level === 2) return "h2";
  if (attrs.level === 3) return "h3";
  return "h1";
}

function blockStyleOf(
  node: { type: { name: string }; attrs: Record<string, unknown> },
): BlockStyle {
  if (node.type.name !== "heading") return "normal";
  if (node.attrs.variant === "title") return "title";
  if (node.attrs.level === 2) return "h2";
  if (node.attrs.level === 3) return "h3";
  return "h1";
}

export function selectedBlockStyle(editor: Editor): BlockStyleState {
  const { from, to, empty } = editor.state.selection;
  if (empty) return currentBlockStyle(editor);

  const styles = new Set<BlockStyle>();
  editor.state.doc.nodesBetween(from, to, (node) => {
    if (!node.isTextblock) return;
    styles.add(blockStyleOf(node));
    return false;
  });

  return styles.size === 1 ? [...styles][0] : "mixed";
}

export function selectedFontSize(editor: Editor): FontSizeState {
  const { from, to, empty } = editor.state.selection;
  if (empty) {
    const size = editor.getAttributes("fontSize").size;
    return typeof size === "string" ? size : "";
  }

  const sizes = new Set<string>();
  editor.state.doc.nodesBetween(from, to, (node) => {
    if (!node.isText) return;
    const mark = node.marks.find(({ type }) => type.name === "fontSize");
    sizes.add(typeof mark?.attrs.size === "string" ? mark.attrs.size : "");
  });

  return sizes.size === 1 ? [...sizes][0] : sizes.size === 0 ? "" : "mixed";
}

export function applyBlockStyle(editor: Editor, style: BlockStyle): boolean {
  const chain = editor.chain().focus();
  switch (style) {
    case "normal":
      return chain.setParagraph().run();
    case "title":
      return chain.setNode("heading", { level: 1, variant: "title" }).run();
    case "h1":
      return chain.setNode("heading", { level: 1, variant: null }).run();
    case "h2":
      return chain.setNode("heading", { level: 2, variant: null }).run();
    case "h3":
      return chain.setNode("heading", { level: 3, variant: null }).run();
  }
}

/**
 * Attach the one formatting surface for the current editor instance.
 *
 * The toolbar itself is view state. Zoom is saved locally, while block and
 * inline formatting are document state and therefore travel through Yjs.
 */
export function installToolbar(
  editor: Editor,
  editorElement: HTMLElement,
  openLink: () => boolean,
): () => void {
  const toolbar = document.createElement("div");
  toolbar.className = "format-toolbar";
  toolbar.setAttribute("role", "toolbar");
  toolbar.setAttribute("aria-label", "Text formatting");

  const zoom = selectControl("Editor zoom", "zoom-select");
  for (const level of ZOOM_LEVELS) zoom.append(option(String(level), `${level}%`));

  const block = selectControl("Text style", "block-select");
  const mixedBlock = option("mixed", "Mixed");
  mixedBlock.disabled = true;
  block.append(
    mixedBlock,
    option("normal", "Normal"),
    option("title", "Title"),
    option("h1", "H1"),
    option("h2", "H2"),
    option("h3", "H3"),
  );

  const size = selectControl("Font size", "size-select");
  const mixedSize = option("mixed", "Mixed");
  mixedSize.disabled = true;
  size.append(mixedSize, option("", "Size"));
  for (const pixels of FONT_SIZES) size.append(option(`${pixels}px`, `${pixels} px`));

  const bold = buttonControl("Bold", "B", "is-bold");
  const italic = buttonControl("Italic", "I", "is-italic");
  const link = buttonControl("Add or edit link", icon(ICONS.link2), "is-icon is-link");

  toolbar.append(zoom, divider(), block, size, divider(), bold, italic, link);
  editorElement.before(toolbar);

  const applyZoom = (level: number) => {
    editorElement.style.setProperty("--editor-zoom", String(level / 100));
    window.localStorage.setItem(ZOOM_KEY, String(level));
    // Provenance rails and any other layout observers measure against editor
    // geometry. Zoom changes that geometry without producing a DOM resize.
    window.dispatchEvent(new Event("resize"));
  };

  const initialZoom = safeZoom(window.localStorage.getItem(ZOOM_KEY));
  zoom.value = String(initialZoom);
  applyZoom(initialZoom);

  const update = () => {
    block.value = selectedBlockStyle(editor);
    const fontSize = selectedFontSize(editor);
    const supported = FONT_SIZES.some((pixels) => fontSize === `${pixels}px`);
    size.value = fontSize === "mixed" || supported
      ? fontSize
      : "";
    bold.setAttribute("aria-pressed", String(editor.isActive("bold")));
    italic.setAttribute("aria-pressed", String(editor.isActive("italic")));
    link.setAttribute("aria-pressed", String(editor.isActive("link")));
    link.disabled = editor.state.selection.empty && !editor.isActive("link");
  };

  zoom.addEventListener("change", () => applyZoom(safeZoom(zoom.value)));
  block.addEventListener("change", () => {
    if (block.value === "mixed") return;
    applyBlockStyle(editor, block.value as BlockStyle);
    update();
  });
  size.addEventListener("change", () => {
    if (size.value === "mixed") return;
    const chain = editor.chain().focus();
    if (size.value) chain.setMark("fontSize", { size: size.value }).run();
    else chain.unsetMark("fontSize").run();
    update();
  });
  bold.addEventListener("click", () => editor.chain().focus().toggleBold().run());
  italic.addEventListener("click", () => editor.chain().focus().toggleItalic().run());
  link.addEventListener("click", () => openLink());

  editor.on("selectionUpdate", update);
  editor.on("transaction", update);
  update();

  return () => {
    editor.off("selectionUpdate", update);
    editor.off("transaction", update);
    toolbar.remove();
  };
}
