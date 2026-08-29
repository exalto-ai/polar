import { Editor } from "@tiptap/core";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { extensions } from "./schema";
import {
  applyBlockStyle,
  currentBlockStyle,
  installToolbar,
  safeZoom,
  type BlockStyle,
} from "./toolbar";

const editors: Editor[] = [];

beforeEach(() => {
  const values = new Map<string, string>();
  const storage: Storage = {
    get length() {
      return values.size;
    },
    clear: () => values.clear(),
    getItem: (key) => values.get(key) ?? null,
    key: (index) => [...values.keys()][index] ?? null,
    removeItem: (key) => values.delete(key),
    setItem: (key, value) => values.set(key, String(value)),
  };
  Object.defineProperty(window, "localStorage", {
    configurable: true,
    value: storage,
  });
});

function makeEditor(content: string | Record<string, unknown> = "<p>Hello</p>") {
  const element = document.createElement("div");
  document.body.append(element);
  const editor = new Editor({ element, extensions, content });
  editors.push(editor);
  return { editor, element };
}

function selectText(editor: Editor) {
  editor.commands.setTextSelection({ from: 1, to: 6 });
}

afterEach(() => {
  for (const editor of editors.splice(0)) editor.destroy();
  document.body.replaceChildren();
  window.localStorage.clear();
});

describe("toolbar block formatting", () => {
  it("accepts only supported zoom levels", () => {
    expect(safeZoom("75")).toBe(75);
    expect(safeZoom("125")).toBe(125);
    expect(safeZoom("200")).toBe(200);
    expect(safeZoom("83")).toBe(100);
    expect(safeZoom("not a number")).toBe(100);
    expect(safeZoom(null)).toBe(100);
  });

  it.each<{
    style: BlockStyle;
    node: string;
    level?: number;
    variant?: string | null;
  }>([
    { style: "normal", node: "paragraph" },
    { style: "title", node: "heading", level: 1, variant: "title" },
    { style: "h1", node: "heading", level: 1, variant: null },
    { style: "h2", node: "heading", level: 2, variant: null },
    { style: "h3", node: "heading", level: 3, variant: null },
  ])("persists $style as document state", ({ style, node, level, variant }) => {
    const { editor } = makeEditor();

    applyBlockStyle(editor, style);
    const json = editor.getJSON();
    expect(json.content?.[0].type).toBe(node);
    if (level !== undefined) {
      expect(json.content?.[0].attrs).toMatchObject({ level, variant });
    }

    const { editor: restored } = makeEditor(json);
    expect(currentBlockStyle(restored)).toBe(style);
  });

  it("removes the title variant when switching to H1", () => {
    const { editor } = makeEditor();

    applyBlockStyle(editor, "title");
    expect(currentBlockStyle(editor)).toBe("title");
    applyBlockStyle(editor, "h1");

    expect(currentBlockStyle(editor)).toBe("h1");
    expect(editor.getJSON().content?.[0].attrs).toMatchObject({
      level: 1,
      variant: null,
    });
  });
});

describe("installed toolbar", () => {
  it("sits immediately before the editor and removes itself on cleanup", () => {
    const shell = document.createElement("main");
    const editorElement = document.createElement("div");
    shell.append(editorElement);
    document.body.append(shell);
    const editor = new Editor({ element: editorElement, extensions, content: "<p>Hello</p>" });
    editors.push(editor);

    const cleanup = installToolbar(editor, editorElement, vi.fn());
    const toolbar = shell.querySelector<HTMLElement>('[role="toolbar"]');

    expect(toolbar).not.toBeNull();
    expect(shell.children[0]).toBe(toolbar);
    expect(shell.children[1]).toBe(editorElement);
    expect(toolbar?.getAttribute("aria-label")).toBe("Text formatting");

    cleanup();
    expect(shell.querySelector('[role="toolbar"]')).toBeNull();
  });

  it("restores and changes editor zoom", () => {
    window.localStorage.setItem("thought.zoom", "125");
    const { editor, element } = makeEditor();
    const cleanup = installToolbar(editor, element, vi.fn());
    const zoom = document.querySelector<HTMLSelectElement>('[aria-label="Editor zoom"]')!;

    expect(zoom.value).toBe("125");
    expect(element.style.getPropertyValue("--editor-zoom")).toBe("1.25");

    zoom.value = "150";
    zoom.dispatchEvent(new Event("change"));

    expect(element.style.getPropertyValue("--editor-zoom")).toBe("1.5");
    expect(window.localStorage.getItem("thought.zoom")).toBe("150");
    cleanup();
  });

  it("applies block style and persistent font size from its selectors", () => {
    const { editor, element } = makeEditor();
    selectText(editor);
    const cleanup = installToolbar(editor, element, vi.fn());
    const block = document.querySelector<HTMLSelectElement>('[aria-label="Text style"]')!;
    const size = document.querySelector<HTMLSelectElement>('[aria-label="Font size"]')!;

    block.value = "h2";
    block.dispatchEvent(new Event("change"));
    expect(currentBlockStyle(editor)).toBe("h2");

    selectText(editor);
    size.value = "24px";
    size.dispatchEvent(new Event("change"));
    expect(editor.getJSON().content?.[0].content?.[0].marks).toContainEqual({
      type: "fontSize",
      attrs: { size: "24px" },
    });
    cleanup();
  });

  it("shows mixed selection state and can normalize every selected run", () => {
    const { editor, element } = makeEditor(
      '<p><span style="font-size: 18px">First</span></p>' +
        '<h1><span style="font-size: 24px">Second</span></h1>',
    );
    editor.commands.selectAll();
    const cleanup = installToolbar(editor, element, vi.fn());
    const block = document.querySelector<HTMLSelectElement>('[aria-label="Text style"]')!;
    const size = document.querySelector<HTMLSelectElement>('[aria-label="Font size"]')!;

    expect(block.value).toBe("mixed");
    expect(size.value).toBe("mixed");

    block.value = "h2";
    block.dispatchEvent(new Event("change"));
    const nonemptyBlocks = editor
      .getJSON()
      .content?.filter((node) =>
        node.content?.some((child) => "text" in child && Boolean(child.text)),
      );
    expect(nonemptyBlocks?.map((node) => node.attrs?.level)).toEqual([2, 2]);

    editor.commands.selectAll();
    size.value = "20px";
    size.dispatchEvent(new Event("change"));
    expect(
      editor
        .getJSON()
        .content?.filter((node) =>
          node.content?.some((child) => "text" in child && Boolean(child.text)),
        )
        .map((node) => node.content?.[0].marks?.[0].attrs?.size),
    ).toEqual(["20px", "20px"]);
    cleanup();
  });

  it("toggles bold and italic without losing the editor selection", () => {
    const { editor, element } = makeEditor();
    selectText(editor);
    const cleanup = installToolbar(editor, element, vi.fn());
    const bold = document.querySelector<HTMLButtonElement>('[aria-label="Bold"]')!;
    const italic = document.querySelector<HTMLButtonElement>('[aria-label="Italic"]')!;

    bold.click();
    italic.click();

    const marks = editor.getJSON().content?.[0].content?.[0].marks;
    expect(marks).toContainEqual({ type: "bold" });
    expect(marks).toContainEqual({ type: "italic" });
    expect(bold.getAttribute("aria-pressed")).toBe("true");
    expect(italic.getAttribute("aria-pressed")).toBe("true");
    cleanup();
  });

  it("routes the link command to the link controller", () => {
    const openLink = vi.fn(() => true);
    const { editor, element } = makeEditor();
    selectText(editor);
    const cleanup = installToolbar(editor, element, openLink);
    const link = document.querySelector<HTMLButtonElement>(
      '[aria-label="Add or edit link"]',
    )!;

    expect(link.disabled).toBe(false);
    link.click();
    expect(openLink).toHaveBeenCalledOnce();
    cleanup();
  });
});
