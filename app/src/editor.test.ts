import { Schema } from "@tiptap/pm/model";
import { Transform } from "@tiptap/pm/transform";
import { describe, expect, it } from "vitest";
import { transactionRanges } from "./editor";

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
