import { Editor } from "@tiptap/core";
import { openUrl } from "@tauri-apps/plugin-opener";
import { afterEach, describe, expect, it, vi } from "vitest";
import { installLinkShortcut, normalize } from "./link";
import { extensions } from "./schema";

vi.mock("@tauri-apps/plugin-opener", () => ({ openUrl: vi.fn() }));

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
  vi.restoreAllMocks();
  vi.mocked(openUrl).mockReset();
  delete (window as unknown as { __TAURI_INTERNALS__?: unknown }).__TAURI_INTERNALS__;
  Object.defineProperty(navigator, "clipboard", { configurable: true, value: undefined });
});

function markHelloAsLink(editor: Editor, href = "https://example.com/docs") {
  editor.commands.setTextSelection({ from: 1, to: 6 });
  editor.chain().focus().setLink({ href }).run();
  editor.commands.setTextSelection(3);
  const anchor = editor.view.dom.querySelector<HTMLAnchorElement>("a")!;
  vi.spyOn(anchor, "getBoundingClientRect").mockReturnValue({
    left: 30,
    right: 90,
    top: 40,
    bottom: 58,
    width: 60,
    height: 18,
    x: 30,
    y: 40,
    toJSON: () => ({}),
  });
  return anchor;
}

function clickLink(anchor: HTMLAnchorElement) {
  anchor.dispatchEvent(new MouseEvent("click", { bubbles: true, cancelable: true, button: 0 }));
}

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
    expect(normalize("tel:15551234")).toBe("tel:15551234");
    expect(normalize("sms:15551234")).toBe("sms:15551234");
    expect(normalize("urn:1234")).toBe("urn:1234");
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
  it("shows explicit link actions instead of opening linked text", () => {
    const { editor, host } = linkedEditor();
    const anchor = markHelloAsLink(editor);
    const opened = vi.spyOn(window, "open").mockImplementation(() => null);
    const links = installLinkShortcut(editor, host);

    clickLink(anchor);

    expect(opened).not.toHaveBeenCalled();
    expect(editor.state.selection.empty).toBe(true);
    expect(editor.state.selection.from).toBeGreaterThanOrEqual(1);
    expect(editor.state.selection.from).toBeLessThanOrEqual(6);
    const card = host.querySelector<HTMLElement>(".link-card")!;
    expect(card.hidden).toBe(false);
    expect(card.getAttribute("role")).toBe("dialog");
    expect(document.activeElement).toBe(card.querySelector(".link-card-open"));
    expect(card.querySelector(".link-card-title")?.textContent).toBe("Hello");
    expect(card.querySelector(".link-card-url")?.textContent).toBe("example.com/docs");

    card.querySelector<HTMLButtonElement>('.link-card-open')!.click();
    expect(opened).toHaveBeenCalledWith(
      "https://example.com/docs",
      "_blank",
      "noopener,noreferrer",
    );
    links.destroy();
  });

  it("uses the native opener and keeps a visible failure in the action card", async () => {
    Object.defineProperty(window, "__TAURI_INTERNALS__", {
      configurable: true,
      value: {},
    });
    vi.mocked(openUrl).mockRejectedValue(new Error("opener unavailable"));
    const { editor, host } = linkedEditor();
    const anchor = markHelloAsLink(editor);
    const links = installLinkShortcut(editor, host);

    clickLink(anchor);
    host.querySelector<HTMLButtonElement>(".link-card-open")!.click();

    await vi.waitFor(() => {
      expect(openUrl).toHaveBeenCalledWith("https://example.com/docs");
      expect(
        host.querySelector<HTMLElement>(".link-card-destination")!.dataset.feedback,
      ).toBe("Could not open link");
    });
    expect(host.querySelector<HTMLElement>(".link-card")!.hidden).toBe(false);
    links.destroy();
  });

  it("edits or removes the complete link from its action card", () => {
    const { editor, host } = linkedEditor();
    let anchor = markHelloAsLink(editor);
    const links = installLinkShortcut(editor, host);

    clickLink(anchor);
    host.querySelector<HTMLButtonElement>('[aria-label="Edit link"]')!.click();
    const field = host.querySelector<HTMLInputElement>(".link-input")!;
    expect(field.hidden).toBe(false);
    expect(field.value).toBe("https://example.com/docs");
    field.value = "example.org/revised";
    field.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));
    expect(editor.getAttributes("link").href).toBe("https://example.org/revised");

    anchor = editor.view.dom.querySelector<HTMLAnchorElement>("a")!;
    clickLink(anchor);
    host.querySelector<HTMLButtonElement>('[aria-label="Remove link"]')!.click();
    expect(editor.getJSON().content?.[0].content?.[0]).toMatchObject({ text: "Hello" });
    expect(editor.getJSON().content?.[0].content?.[0].marks).toBeUndefined();
    expect(host.querySelector<HTMLElement>(".link-card")!.hidden).toBe(true);
    links.destroy();
  });

  it("copies the exact href and closes the card with Escape", async () => {
    const { editor, host } = linkedEditor();
    const anchor = markHelloAsLink(editor, "/docs/start");
    const writeText = vi.fn().mockResolvedValue(undefined);
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText },
    });
    const links = installLinkShortcut(editor, host);

    clickLink(anchor);
    host.querySelector<HTMLButtonElement>('[aria-label="Copy link"]')!.click();
    expect(writeText).toHaveBeenCalledWith("/docs/start");
    await vi.waitFor(() => {
      expect(host.querySelector<HTMLButtonElement>('[aria-label="Copied"]')).not.toBeNull();
    });
    expect(host.querySelector<HTMLButtonElement>('[aria-label="Copied"]')!.dataset.feedback).toBe(
      "Copied",
    );

    document.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true }));
    expect(host.querySelector<HTMLElement>(".link-card")!.hidden).toBe(true);
    expect(document.activeElement).toBe(editor.view.dom);
    links.destroy();
  });

  it("returns focus to the editor when an outside action dismisses the card", async () => {
    const { editor, host } = linkedEditor();
    const anchor = markHelloAsLink(editor);
    const links = installLinkShortcut(editor, host);

    clickLink(anchor);
    expect(document.activeElement).toBe(
      host.querySelector<HTMLButtonElement>(".link-card-open"),
    );
    host.dispatchEvent(new MouseEvent("mousedown", { bubbles: true }));
    await Promise.resolve();

    expect(host.querySelector<HTMLElement>(".link-card")!.hidden).toBe(true);
    expect(document.activeElement).toBe(editor.view.dom);
    links.destroy();
  });

  it("opens link options from the keyboard while the cursor is in a link", () => {
    const { editor, host } = linkedEditor();
    markHelloAsLink(editor);
    const links = installLinkShortcut(editor, host);
    editor.commands.setTextSelection(6);
    editor.view.dom.focus();

    document.dispatchEvent(
      new KeyboardEvent("keydown", {
        key: "Enter",
        altKey: true,
        bubbles: true,
        cancelable: true,
      }),
    );

    const card = host.querySelector<HTMLElement>(".link-card")!;
    expect(card.hidden).toBe(false);
    expect(document.activeElement).toBe(card.querySelector(".link-card-open"));
    links.destroy();
  });

  it("opens from a toolbar selection and applies a normalized URL", () => {
    const { editor, host } = linkedEditor();
    editor.commands.setTextSelection({ from: 1, to: 6 });
    const links = installLinkShortcut(editor, host);
    vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      callback(0);
      return 1;
    });

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
    expect(field.hidden).toBe(true);
    expect(document.activeElement).toBe(editor.view.dom);
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

  it("keeps an invalid URL open for correction even when editor focus is synchronous", () => {
    const { editor, host } = linkedEditor();
    editor.commands.setTextSelection({ from: 1, to: 6 });
    const links = installLinkShortcut(editor, host);
    links.open();
    const field = host.querySelector<HTMLInputElement>(".link-input")!;
    vi.spyOn(window, "requestAnimationFrame").mockImplementation((callback) => {
      callback(0);
      return 1;
    });

    field.value = "javascript:alert(1)";
    field.dispatchEvent(new KeyboardEvent("keydown", { key: "Enter", bubbles: true }));

    expect(field.hidden).toBe(false);
    expect(field.value).toBe("javascript:alert(1)");
    expect(field.validationMessage).toBe("Enter a valid link");
    expect(document.activeElement).toBe(field);
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
