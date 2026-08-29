import type { Editor } from "@tiptap/core";
import { Schema } from "@tiptap/pm/model";
import { Transform } from "@tiptap/pm/transform";
import { describe, expect, it, vi } from "vitest";
import { installEditorCanvasFocus, transactionRanges } from "./editor";

const schema = new Schema({
  nodes: {
    doc: { content: "block+" },
    paragraph: { content: "text*", group: "block" },
    text: {},
  },
});

describe("editor mutation ranges", () => {
  it("captures positions on both sides of one complete transform", () => {
    const before = schema.node("doc", null, [
      schema.node("paragraph", null, [schema.text("yesyes")]),
    ]);
    const transform = new Transform(before).replaceWith(4, 7, schema.text("YES"));

    expect(transactionRanges(transform)).toEqual([
      { beforeFrom: 4, beforeTo: 7, afterFrom: 4, afterTo: 7 },
    ]);
  });
});

describe("editor canvas focus", () => {
  it("focuses only an editable canvas background", () => {
    const page = document.createElement("main");
    page.className = "page";
    const element = document.createElement("div");
    const editorDom = document.createElement("div");
    const block = document.createElement("p");
    editorDom.append(block);
    element.append(editorDom);
    page.append(element);
    document.body.append(page);
    const commandFocus = vi.fn();
    const viewFocus = vi.fn();
    const editor = {
      isEditable: true,
      commands: { focus: commandFocus },
      view: { dom: editorDom, focus: viewFocus },
    } as unknown as Editor;
    const destroy = installEditorCanvasFocus(editor, element);

    const canvasPress = new MouseEvent("mousedown", {
      bubbles: true,
      cancelable: true,
      button: 0,
    });
    page.dispatchEvent(canvasPress);
    expect(canvasPress.defaultPrevented).toBe(true);
    expect(commandFocus).toHaveBeenCalledWith("end");
    expect(viewFocus).toHaveBeenCalledOnce();

    commandFocus.mockClear();
    block.dispatchEvent(new MouseEvent("mousedown", { bubbles: true, button: 0 }));
    expect(commandFocus).not.toHaveBeenCalled();

    destroy();
    page.remove();
  });
});
