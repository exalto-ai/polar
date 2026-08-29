import { Editor } from "@tiptap/core";
import { afterEach, describe, expect, it, vi } from "vitest";
import { installLinkShortcut, normalize } from "./link";
import { extensions } from "./schema";

const editors: Editor[] = [];

function linkedEditor() {
  const host = document.createElement("main");
  const element = document.createElement("div");
  host.append(element);
  document.body.append(host);
  const editor = new Editor({ element, extensions, content: "<p>Hello</p>" });
  vi.spyOn(editor.view, "coordsAtPos").mockReturnValue({
    left: 24,
    right: 40,
    top: 18,
    bottom: 36,
  });
  editors.push(editor);
  return { editor, host };
}

afterEach(() => {
  for (const editor of editors.splice(0)) editor.destroy();
  document.body.replaceChildren();
});

describe("link normalisation", () => {
  it("adds a scheme to what people actually paste", () => {
    // Without one the browser resolves it relative to the page, which in a
    // Tauri window means tauri://localhost/example.com.
    expect(normalize("example.com")).toBe("https://example.com");
    expect(normalize("example.com/a/b?c=1")).toBe("https://example.com/a/b?c=1");
  });

  it("leaves an explicit scheme alone", () => {
    expect(normalize("https://example.com")).toBe("https://example.com");
    expect(normalize("http://example.com")).toBe("http://example.com");
    expect(normalize("mailto:someone@example.com")).toBe("mailto:someone@example.com");
  });

  it("completes a protocol-relative link", () => {
    expect(normalize("//example.com")).toBe("https://example.com");
  });

  it("keeps relative destinations and fragments relative", () => {
    expect(normalize("/docs/start")).toBe("/docs/start");
    expect(normalize("./next")).toBe("./next");
    expect(normalize("../previous")).toBe("../previous");
    expect(normalize("#details")).toBe("#details");
    expect(normalize("?page=2")).toBe("?page=2");
  });

  it("recognizes a hostname with a port instead of treating it as a scheme", () => {
    expect(normalize("localhost:3000")).toBe("https://localhost:3000");
    expect(normalize("example.com:8443/docs")).toBe("https://example.com:8443/docs");
  });
});

describe("link command", () => {
  it("opens from a toolbar selection and applies a normalized URL", () => {
    const { editor, host } = linkedEditor();
    editor.commands.setTextSelection({ from: 1, to: 6 });
    const links = installLinkShortcut(editor, host);

    expect(links.open()).toBe(true);
    const field = host.querySelector<HTMLInputElement>(".link-input")!;
    expect(field.hidden).toBe(false);

    field.value = "example.com/docs";
    field.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));

    expect(editor.getJSON().content?.[0].content?.[0].marks).toContainEqual({
      type: "link",
      attrs: {
        href: "https://example.com/docs",
        target: "_blank",
        rel: "noopener noreferrer nofollow",
        class: null,
        title: null,
      },
    });
    links.destroy();
  });

  it("edits the entire link under a cursor and clears it with an empty URL", () => {
    const { editor, host } = linkedEditor();
    editor.commands.setTextSelection({ from: 1, to: 6 });
    editor.chain().focus().setLink({ href: "https://example.com" }).run();
    editor.commands.setTextSelection(3);
    const links = installLinkShortcut(editor, host);

    expect(links.open()).toBe(true);
    expect(editor.state.selection.from).toBe(1);
    expect(editor.state.selection.to).toBe(6);

    const field = host.querySelector<HTMLInputElement>(".link-input")!;
    field.value = "";
    field.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));

    expect(editor.getJSON().content?.[0].content?.[0].marks).toBeUndefined();
    links.destroy();
  });

  it("keeps an invalid URL open for correction", () => {
    const { editor, host } = linkedEditor();
    editor.commands.setTextSelection({ from: 1, to: 6 });
    const links = installLinkShortcut(editor, host);
    links.open();
    const field = host.querySelector<HTMLInputElement>(".link-input")!;

    field.value = "javascript:alert(1)";
    field.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));

    expect(field.hidden).toBe(false);
    expect(field.validationMessage).toBe("Enter a valid link");
    expect(editor.getJSON().content?.[0].content?.[0].marks).toBeUndefined();
    links.destroy();
  });

  it("only handles the keyboard shortcut while the editor has focus", () => {
    const { editor, host } = linkedEditor();
    editor.commands.setTextSelection({ from: 1, to: 6 });
    const links = installLinkShortcut(editor, host);
    const field = host.querySelector<HTMLInputElement>(".link-input")!;
    const other = document.createElement("input");
    host.append(other);
    other.focus();

    document.dispatchEvent(
      new KeyboardEvent("keydown", {
        key: "k",
        shiftKey: true,
        metaKey: true,
        ctrlKey: true,
        bubbles: true,
      }),
    );
    expect(field.hidden).toBe(true);

    editor.view.dom.focus();
    document.dispatchEvent(
      new KeyboardEvent("keydown", {
        key: "k",
        shiftKey: true,
        metaKey: true,
        ctrlKey: true,
        bubbles: true,
      }),
    );
    expect(field.hidden).toBe(false);
    links.destroy();
  });
});
