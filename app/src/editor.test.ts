import { Editor } from "@tiptap/core";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { Awareness } from "y-protocols/awareness";
import * as Y from "yjs";
import { createEditor, INPUT_SOURCE_META, transactionInputSource } from "./editor";
import type { SyncProvider } from "./provider";
import { LocalInputSource, type LocalInputSource as LocalInputSourceValue } from "./protocol";
import { extensions } from "./schema";

describe("editor transaction input source", () => {
  let editor: Editor;

  beforeEach(() => {
    editor = new Editor({
      element: document.createElement("div"),
      extensions,
      content: "<p></p>",
    });
  });

  afterEach(() => editor.destroy());

  it("trusts ProseMirror's paste marker over surrounding observations", () => {
    const transaction = editor.state.tr
      .insertText("pasted")
      .setMeta("paste", true)
      .setMeta("uiEvent", "paste")
      .setMeta(INPUT_SOURCE_META, LocalInputSource.Command);

    expect(transactionInputSource(transaction, LocalInputSource.Written)).toBe(
      LocalInputSource.Paste,
    );
  });

  it("accepts only a closed explicit command source", () => {
    const command = editor.state.tr
      .insertText("command")
      .setMeta(INPUT_SOURCE_META, LocalInputSource.Command);
    expect(transactionInputSource(command)).toBe(LocalInputSource.Command);

    const invalid = editor.state.tr
      .insertText("untrusted")
      .setMeta(INPUT_SOURCE_META, "keyboard");
    expect(transactionInputSource(invalid)).toBe(LocalInputSource.Unknown);
  });

  it("uses an observed editor event for written input", () => {
    const transaction = editor.state.tr.insertText("written here");
    expect(transactionInputSource(transaction, LocalInputSource.Written)).toBe(
      LocalInputSource.Written,
    );
  });

  it("recognizes composition as written without a DOM-event fallback", () => {
    const transaction = editor.state.tr.insertText("composed").setMeta("composition", 1);
    expect(transactionInputSource(transaction)).toBe(LocalInputSource.Written);
  });

  it("keeps programmatic and ambiguous drop transactions unknown", () => {
    expect(transactionInputSource(editor.state.tr.insertText("programmatic"))).toBe(
      LocalInputSource.Unknown,
    );

    const drop = editor.state.tr.insertText("dropped").setMeta("uiEvent", "drop");
    expect(transactionInputSource(drop, LocalInputSource.Written)).toBe(
      LocalInputSource.Unknown,
    );
  });

  it("classifies cut as a command, not newly written content", () => {
    const cut = editor.state.tr.insertText("removed").setMeta("uiEvent", "cut");
    expect(transactionInputSource(cut, LocalInputSource.Written)).toBe(
      LocalInputSource.Command,
    );
  });
});

describe("editor source dispatch wrapper", () => {
  it("scopes real paste, observed writing, explicit commands, and unknown fallback", () => {
    const values = new Map<string, string>();
    Object.defineProperty(window, "localStorage", {
      configurable: true,
      value: {
        get length() {
          return values.size;
        },
        clear: () => values.clear(),
        getItem: (key: string) => values.get(key) ?? null,
        key: (index: number) => [...values.keys()][index] ?? null,
        removeItem: (key: string) => values.delete(key),
        setItem: (key: string, value: string) => values.set(key, String(value)),
      } satisfies Storage,
    });
    const host = document.createElement("section");
    const element = document.createElement("div");
    host.append(element);
    document.body.append(host);

    const seen: LocalInputSourceValue[] = [];
    const provider = {
      withLocalInputSource<T>(source: LocalInputSourceValue, run: () => T): T {
        seen.push(source);
        return run();
      },
      noteLocalInputSource: () => {},
      setComposing: () => {},
      subscribeSaveStatus: (listener: (status: "saved") => void) => {
        listener("saved");
        return () => {};
      },
    } as unknown as SyncProvider;
    const doc = new Y.Doc();
    const awareness = new Awareness(doc);
    const collaborative = createEditor(
      host,
      element,
      doc,
      awareness,
      provider,
      { id: doc.clientID, name: "Writer", color: "#123456" },
      {
        newDocument: () => {},
        importMarkdown: () => {},
        exportMarkdown: () => {},
      },
    );

    seen.length = 0;
    collaborative.view.pasteText("pasted");
    expect(seen[seen.length - 1]).toBe(LocalInputSource.Paste);

    collaborative.view.dom.dispatchEvent(
      new InputEvent("beforeinput", {
        bubbles: true,
        inputType: "insertText",
        data: "w",
      }),
    );
    collaborative.commands.insertContent("written");
    expect(seen[seen.length - 1]).toBe(LocalInputSource.Written);

    collaborative
      .chain()
      .setMeta(INPUT_SOURCE_META, LocalInputSource.Command)
      .insertContent("command")
      .run();
    expect(seen[seen.length - 1]).toBe(LocalInputSource.Command);

    // A later programmatic write has no observed browser event and makes no
    // stronger claim. The tracker clears its event at the next task boundary;
    // an explicit transaction is enough to override it synchronously here.
    collaborative
      .chain()
      .setMeta(INPUT_SOURCE_META, LocalInputSource.Unknown)
      .insertContent("unknown")
      .run();
    expect(seen[seen.length - 1]).toBe(LocalInputSource.Unknown);

    collaborative.destroy();
    awareness.destroy();
    doc.destroy();
    host.remove();
  });
});
