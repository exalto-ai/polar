import { Editor } from "@tiptap/core";
import { Plugin } from "@tiptap/pm/state";
import { afterEach, beforeEach, describe, expect, it } from "vitest";
import { Awareness } from "y-protocols/awareness";
import * as Y from "yjs";
import {
  createEditor,
  INPUT_SOURCE_META,
  transactionAnchorHints,
  transactionInputSource,
} from "./editor";
import type { SyncProvider } from "./provider";
import {
  LocalInputSource,
  type AnchoredRangeHint,
  type LocalInputSource as LocalInputSourceValue,
} from "./protocol";
import { extensions } from "./schema";

describe("editor dispatch input source", () => {
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

  it("captures changed positions before and after the transaction", () => {
    const transaction = editor.state.tr.insertText("hello", 1);
    expect(transactionAnchorHints(transaction)).toEqual([
      { beforeFrom: 1, beforeTo: 1, afterFrom: 1, afterTo: 6 },
    ]);
  });

  it("uses zero hints for a transaction with no document change", () => {
    expect(transactionAnchorHints(editor.state.tr.setMeta("test", true))).toEqual([]);
  });

  it("expands composition edits to complete Unicode grapheme boundaries", () => {
    editor.commands.setContent("<p>e</p>");
    const composition = editor.state.tr
      .insertText("\u0301", 2)
      .setMeta("composition", 1);

    expect(transactionAnchorHints(composition)).toEqual([
      { beforeFrom: 1, beforeTo: 2, afterFrom: 1, afterTo: 3 },
    ]);
  });

  it("sorts reverse-ordered multi-step ranges into canonical document order", () => {
    editor.commands.setContent("<p>abcdef</p>");
    const transaction = editor.state.tr.insertText("X", 6).insertText("Y", 2);

    expect(transactionAnchorHints(transaction)).toEqual([
      { beforeFrom: 2, beforeTo: 2, afterFrom: 2, afterTo: 3 },
      { beforeFrom: 6, beforeTo: 6, afterFrom: 7, afterTo: 8 },
    ]);
  });
});

describe("editor source dispatch wrapper", () => {
  it("scopes source and anchor hints for every semantic dispatch", () => {
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

    const seen: {
      source: LocalInputSourceValue;
      hints: readonly AnchoredRangeHint[];
    }[] = [];
    const provider = {
      withLocalTransaction<T>(
        source: LocalInputSourceValue,
        hints: readonly AnchoredRangeHint[],
        run: () => T,
      ): T {
        seen.push({ source, hints });
        return run();
      },
      noteLocalInputSource: () => {},
      setComposing: () => {},
      isHydrated: true,
      subscribeHydration: (listener: (hydrated: boolean) => void) => {
        listener(true);
        return () => {};
      },
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
    expect(seen[seen.length - 1].source).toBe(LocalInputSource.Paste);
    expect(seen[seen.length - 1].hints.length).toBeGreaterThan(0);

    collaborative.view.dom.dispatchEvent(
      new InputEvent("beforeinput", {
        bubbles: true,
        inputType: "insertText",
        data: "w",
      }),
    );
    collaborative.commands.insertContent("written");
    expect(seen[seen.length - 1].source).toBe(LocalInputSource.Written);
    expect(seen[seen.length - 1].hints.length).toBeGreaterThan(0);

    collaborative
      .chain()
      .setMeta(INPUT_SOURCE_META, LocalInputSource.Command)
      .insertContent("command")
      .run();
    expect(seen[seen.length - 1].source).toBe(LocalInputSource.Command);

    // A later programmatic write has no observed browser event and makes no
    // stronger claim. The tracker clears its event at the next task boundary;
    // an explicit transaction is enough to override it synchronously here.
    collaborative
      .chain()
      .setMeta(INPUT_SOURCE_META, LocalInputSource.Unknown)
      .insertContent("unknown")
      .run();
    expect(seen[seen.length - 1].source).toBe(LocalInputSource.Unknown);

    const composition = collaborative.state.tr
      .insertText("に")
      .setMeta("composition", 41);
    collaborative.view.dispatch(composition);
    expect(seen[seen.length - 1]).toEqual({
      source: LocalInputSource.Written,
      hints: transactionAnchorHints(composition),
    });

    collaborative.registerPlugin(
      new Plugin({
        appendTransaction(transactions, _oldState, newState) {
          if (!transactions.some((item) => item.getMeta("append-test"))) {
            return null;
          }
          const paragraph = newState.schema.nodes.paragraph.create(
            null,
            newState.schema.text("appended"),
          );
          return newState.tr.insert(newState.doc.content.size, paragraph);
        },
      }),
    );
    seen.length = 0;
    collaborative.view.dispatch(
      collaborative.state.tr
        .insertText("root", 1)
        .setMeta("append-test", true)
        .setMeta(INPUT_SOURCE_META, LocalInputSource.Command),
    );
    expect(seen).toHaveLength(1);
    expect(seen[0].source).toBe(LocalInputSource.Command);
    // The appended paragraph is outside the root insertion. Root-only hints
    // would have one range and would incorrectly claim complete V2 evidence.
    expect(seen[0].hints).toHaveLength(2);
    expect(seen[0].hints[1].afterFrom).toBeGreaterThan(
      seen[0].hints[0].afterTo,
    );

    collaborative.destroy();
    awareness.destroy();
    doc.destroy();
    host.remove();
  });

  it("stays read-only until the provider applies its first Sync", () => {
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
    let hydrationListener: (hydrated: boolean) => void = () => {};
    let hydrated = false;
    const provider = {
      withLocalTransaction<T>(
        _source: LocalInputSourceValue,
        _hints: readonly AnchoredRangeHint[],
        run: () => T,
      ): T {
        return run();
      },
      noteLocalInputSource: () => {},
      setComposing: () => {},
      get isHydrated() {
        return hydrated;
      },
      subscribeHydration: (listener: (hydrated: boolean) => void) => {
        hydrationListener = listener;
        listener(false);
        return () => {
          hydrationListener = () => {};
        };
      },
      subscribeSaveStatus: (listener: (status: "connecting") => void) => {
        listener("connecting");
        return () => {};
      },
    } as unknown as SyncProvider;
    const doc = new Y.Doc();
    let localUpdates = 0;
    doc.on("update", () => {
      localUpdates += 1;
    });
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

    expect(collaborative.isEditable).toBe(false);
    expect(localUpdates).toBe(0);
    expect(doc.getXmlFragment("content").length).toBe(0);
    hydrated = true;
    hydrationListener(true);
    expect(collaborative.isEditable).toBe(true);

    collaborative.destroy();
    awareness.destroy();
    doc.destroy();
    host.remove();
  });
});
